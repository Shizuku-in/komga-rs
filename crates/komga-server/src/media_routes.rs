//! Analyzed book pages, generated posters, and manual analysis endpoints.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use headers_accept::Accept;
use komga_core::{
    Book, ImageOutput, Media, MediaOperationError, MediaStatus, PageDto, TypedBytes, UserRole, convert_image,
    extract_page, extract_raw_page, media_page_dtos,
};
use komga_storage::TaskRecord;
use mediatype::{
    MediaType,
    names::{APPLICATION, IMAGE, JPEG, PDF},
};
use serde::Deserialize;
use url::Url;

use crate::{AppState, auth::AuthContext, content_routes::load_visible_book, error::ApiError, routes::require_admin};

static APPLICATION_PDF: MediaType<'static> = MediaType::new(APPLICATION, PDF);
static IMAGE_JPEG: MediaType<'static> = MediaType::new(IMAGE, JPEG);

#[derive(Debug, Deserialize)]
pub(crate) struct PageQuery {
    convert: Option<String>,
    #[serde(default)]
    zero_based: bool,
    #[serde(default = "default_true", rename = "contentNegotiation")]
    content_negotiation: bool,
}

impl Default for PageQuery {
    fn default() -> Self {
        Self {
            convert: None,
            zero_based: false,
            content_negotiation: true,
        }
    }
}

fn default_true() -> bool {
    true
}

pub(crate) async fn analyze_book(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .filter(|book| book.deleted_date.is_none())
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    state
        .enqueue_task(&TaskRecord::analyze_book(&book.id, &book.series_id, 6))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn list_pages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Json<Vec<PageDto>>, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let media = load_media(&state, &book_id).await?;
    ensure_pages_ready(&media)?;
    Ok(Json(media_page_dtos(&media)))
}

pub(crate) async fn get_page(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, page_number)): Path<(String, i32)>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_page_streaming(&auth)?;
    let (book, media) = load_visible_book_media(&state, &auth, &book_id).await?;
    ensure_pages_ready(&media)?;
    if is_not_modified(&headers, &media) {
        return Ok(not_modified_response(&media));
    }
    let page_number = if query.zero_based {
        page_number
            .checked_add(1)
            .ok_or_else(|| ApiError::bad_request("Page number does not exist"))?
    } else {
        page_number
    };
    if query.content_negotiation && media.media_type.as_deref() == Some("application/pdf") && accepts_pdf(&headers) {
        let raw_book = book.clone();
        let raw_media = media.clone();
        let content = tokio::task::spawn_blocking(move || extract_raw_page(&raw_book, &raw_media, page_number))
            .await
            .map_err(|error| {
                tracing::error!(%error, %book_id, "raw page extractor panicked");
                ApiError::internal("Could not read raw book page")
            })?
            .map_err(map_media_error)?;
        return Ok(content_response(
            content,
            &format!("{}-{page_number}", book.name),
            &media,
        ));
    }
    let output = query
        .convert
        .as_deref()
        .map(|value| ImageOutput::parse(value).ok_or_else(|| ApiError::bad_request("Invalid conversion format")))
        .transpose()?;
    let source_book = book.clone();
    let source_media = media.clone();
    let content = tokio::task::spawn_blocking(move || extract_page(&source_book, &source_media, page_number))
        .await
        .map_err(|error| {
            tracing::error!(%error, %book_id, "page extractor panicked");
            ApiError::internal("Could not read book page")
        })?
        .map_err(map_media_error)?;
    let content = if let Some(output) = output {
        tokio::task::spawn_blocking(move || convert_image(&content.bytes, output, None))
            .await
            .map_err(|error| {
                tracing::error!(%error, %book_id, "image converter panicked");
                ApiError::internal("Could not convert book page")
            })?
            .map_err(map_media_error)?
    } else {
        content
    };
    Ok(content_response(
        content,
        &format!("{}-{page_number}", book.name),
        &media,
    ))
}

pub(crate) async fn get_page_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, page_number)): Path<(String, i32)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (book, media) = load_visible_book_media(&state, &auth, &book_id).await?;
    ensure_pages_ready(&media)?;
    if is_not_modified(&headers, &media) {
        return Ok(not_modified_response(&media));
    }
    let thumbnail_book = book.clone();
    let thumbnail_media = media.clone();
    let content = tokio::task::spawn_blocking(move || {
        let source = extract_page(&thumbnail_book, &thumbnail_media, page_number)?;
        convert_image(&source.bytes, ImageOutput::Jpeg, Some(300))
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, %book_id, "page thumbnail generator panicked");
        ApiError::internal("Could not create page thumbnail")
    })?
    .map_err(map_media_error)?;
    Ok(content_response(
        content,
        &format!("{}-{page_number}-thumbnail", book.name),
        &media,
    ))
}

pub(crate) async fn get_raw_page(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, page_number)): Path<(String, i32)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_page_streaming(&auth)?;
    let (book, media) = load_visible_book_media(&state, &auth, &book_id).await?;
    ensure_pages_ready(&media)?;
    if is_not_modified(&headers, &media) {
        return Ok(not_modified_response(&media));
    }
    if media.media_type.as_deref() != Some("application/pdf") {
        return Err(ApiError::bad_request(
            "Extractor does not support raw extraction of pages",
        ));
    }
    let raw_book = book.clone();
    let raw_media = media.clone();
    let content = tokio::task::spawn_blocking(move || extract_raw_page(&raw_book, &raw_media, page_number))
        .await
        .map_err(|error| {
            tracing::error!(%error, %book_id, "raw page extractor panicked");
            ApiError::internal("Could not read raw book page")
        })?
        .map_err(map_media_error)?;
    Ok(content_response(
        content,
        &format!("{}-{page_number}", book.name),
        &media,
    ))
}

pub(crate) async fn get_book_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let book = load_visible_book(&state, &auth.user, &book_id).await?;
    let media = load_media(&state, &book_id).await?;
    if is_not_modified(&headers, &media) {
        return Ok(not_modified_response(&media));
    }
    let thumbnail = state
        .storage
        .find_selected_book_thumbnail(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
    let bytes = if let Some(bytes) = thumbnail.thumbnail {
        bytes
    } else if let Some(url) = thumbnail.url {
        let path = Url::parse(&url)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
        tokio::fs::read(path)
            .await
            .map_err(|_| ApiError::not_found("Book thumbnail not found"))?
    } else {
        return Err(ApiError::not_found("Book thumbnail not found"));
    };
    Ok(content_response(
        TypedBytes {
            bytes,
            media_type: "image/jpeg".to_owned(),
        },
        &format!("{}-thumbnail", book.name),
        &media,
    ))
}

async fn load_visible_book_media(
    state: &AppState,
    auth: &AuthContext,
    book_id: &str,
) -> Result<(Book, Media), ApiError> {
    load_visible_book(state, &auth.user, book_id).await?;
    let book = state
        .storage
        .find_book_by_id(book_id)
        .await?
        .filter(|book| book.deleted_date.is_none())
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let media = load_media(state, book_id).await?;
    Ok((book, media))
}

async fn load_media(state: &AppState, book_id: &str) -> Result<Media, ApiError> {
    state
        .storage
        .find_media_by_book_id(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book analysis failed"))
}

fn ensure_pages_ready(media: &Media) -> Result<(), ApiError> {
    match media.status {
        MediaStatus::Unknown => Err(ApiError::not_found("Book has not been analyzed yet")),
        MediaStatus::Outdated => Err(ApiError::not_found("Book is outdated and must be re-analyzed")),
        MediaStatus::Error => Err(ApiError::not_found("Book analysis failed")),
        MediaStatus::Unsupported => Err(ApiError::not_found("Book format is not supported")),
        MediaStatus::Ready => Ok(()),
    }
}

fn require_page_streaming(auth: &AuthContext) -> Result<(), ApiError> {
    auth.user
        .roles
        .contains(&UserRole::PageStreaming)
        .then_some(())
        .ok_or_else(ApiError::forbidden)
}

fn map_media_error(error: MediaOperationError) -> ApiError {
    match error {
        MediaOperationError::PageOutOfBounds => ApiError::bad_request("Page number does not exist"),
        MediaOperationError::NotReady => ApiError::not_found("Book analysis failed"),
        MediaOperationError::Image(message) => ApiError::not_found(message),
        MediaOperationError::Read(message) => {
            tracing::warn!(%message, "could not read book page");
            ApiError::not_found("File not found, it may have moved")
        }
    }
}

fn accepts_pdf(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| Accept::try_from(value).ok())
        .and_then(|accept| accept.negotiate([&APPLICATION_PDF, &IMAGE_JPEG]))
        .is_some_and(|media_type| media_type == &APPLICATION_PDF)
}

fn content_response(content: TypedBytes, file_stem: &str, media: &Media) -> Response {
    let extension = match content.media_type.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/vnd.microsoft.icon" => "ico",
        "application/pdf" => "pdf",
        _ => "bin",
    };
    let safe_name = file_stem
        .chars()
        .map(|value| {
            if value.is_ascii_control() || matches!(value, '"' | '\\') {
                '_'
            } else {
                value
            }
        })
        .collect::<String>();
    let mut response = Body::from(content.bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content.media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"{safe_name}.{extension}\"")) {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, value);
    }
    set_last_modified(response.headers_mut(), media);
    response
}

fn not_modified_response(media: &Media) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    set_last_modified(response.headers_mut(), media);
    response
}

fn is_not_modified(headers: &HeaderMap, media: &Media) -> bool {
    let Some(modified) = media_modified_time(media) else {
        return false;
    };
    let Some(requested) = headers
        .get(header::IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
    else {
        return false;
    };
    let modified_seconds = modified.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let requested_seconds = requested.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    modified_seconds <= requested_seconds
}

fn set_last_modified(headers: &mut HeaderMap, media: &Media) {
    if let Some(modified) = media_modified_time(media)
        && let Ok(value) = HeaderValue::from_str(&httpdate::fmt_http_date(modified))
    {
        headers.insert(header::LAST_MODIFIED, value);
    }
}

fn media_modified_time(media: &Media) -> Option<SystemTime> {
    let timestamp = media.last_modified_date?.and_utc().timestamp();
    u64::try_from(timestamp)
        .ok()
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(accept: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_str(accept).unwrap());
        headers
    }

    #[test]
    fn pdf_content_negotiation_matches_kotlin_precedence() {
        for (accept, expected) in [
            ("application/pdf", true),
            ("application/pdf, image/*", true),
            ("image/*", false),
            ("image/avif", false),
            ("application/atom+xml", false),
            ("image/jpeg, application/pdf", false),
            ("application/pdf;q=0.5, image/jpeg", false),
            ("image/jpeg;q=0.5, application/pdf;q=0.8", true),
            ("*/*", true),
        ] {
            assert_eq!(accepts_pdf(&headers(accept)), expected, "{accept}");
        }
        assert!(!accepts_pdf(&HeaderMap::new()));
        assert!(!accepts_pdf(&headers("not a media type")));
    }
}
