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
use komga_core::{Book, Media, MediaExtension, MediaProfile, MediaStatus, PDF_MEDIA_TYPE, UserRole};
use komga_media::{
    ImageOutput, MediaOperationError, TypedBytes, convert_image, extract_page, extract_raw_page, extract_resource,
    page_output_dimension, page_output_media_type,
};
use komga_storage::TaskRecord;
use mediatype::{
    MediaType,
    names::{APPLICATION, IMAGE, JPEG, PDF},
};
use serde::Deserialize;

use crate::{
    AppState,
    auth::{self, AuthContext},
    content_access::load_visible_book,
    dto::{PageDto, PositionsDto},
    error::ApiError,
    routes::require_admin,
};

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

pub(crate) async fn analyze_library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    for book in state.storage.list_books_by_library_id(&library_id).await? {
        state
            .enqueue_task(&TaskRecord::analyze_book(&book.id, &book.series_id, 6))
            .await?;
    }
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn analyze_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    for book in state.storage.list_books_by_series_id(&series_id).await? {
        state
            .enqueue_task(&TaskRecord::analyze_book(&book.id, &book.series_id, 6))
            .await?;
    }
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
    Ok(Json(page_dtos(&media)))
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
    if query.content_negotiation && media.profile() == Some(MediaProfile::Pdf) && accepts_pdf(&headers) {
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

pub(crate) async fn get_opds_v1_page(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<(String, i32)>,
    Query(mut query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    query.zero_based = true;
    get_page(state, auth, path, Query(query), headers).await
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
    if media.profile() != Some(MediaProfile::Pdf) {
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

pub(crate) async fn get_positions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let media = load_media(&state, &book_id).await?;
    if is_not_modified(&headers, &media) {
        return Ok(not_modified_response(&media));
    }
    let Some(MediaExtension::Epub(extension)) = media.extension.as_ref() else {
        return Err(ApiError::not_found("EPUB positions not found"));
    };
    let mut response = Json(PositionsDto {
        total: i32::try_from(extension.positions.len()).unwrap_or(i32::MAX),
        positions: extension.positions.clone(),
    })
    .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.readium.position-list+json"),
    );
    set_last_modified(response.headers_mut(), &media);
    Ok(response)
}

pub(crate) async fn get_epub_resource(
    State(state): State<AppState>,
    Path((book_id, resource)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let resource = resource.trim_start_matches('/').to_owned();
    let is_font = is_font_resource(&resource);
    let user = auth::optional_user(&state, &headers).await?;
    if !is_font {
        let user = user.as_ref().ok_or_else(ApiError::unauthorized)?;
        load_visible_book(&state, user, &book_id).await?;
    }
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .filter(|book| book.deleted_date.is_none())
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let media = load_media(&state, &book_id).await?;
    if media.profile() != Some(MediaProfile::Epub) {
        return Err(ApiError::bad_request(format!(
            "Book media type '{}' not compatible with requested profile",
            media.media_type.as_deref().unwrap_or_default()
        )));
    }
    if !media.files.iter().any(|file| file.file_name == resource) {
        return Err(ApiError::not_found("EPUB resource not found"));
    }
    if is_not_modified(&headers, &media) {
        let mut response = not_modified_response(&media);
        set_epub_content_security_policy(response.headers_mut());
        return Ok(response);
    }
    let source_media = media.clone();
    let source_resource = resource.clone();
    let materialized = state
        .mobi
        .materialize_book(&book, &media, &state.shutdown)
        .await
        .map_err(|error| {
            tracing::warn!(%error, %book_id, "could not materialize EPUB resource source");
            ApiError::internal("Could not read EPUB resource")
        })?;
    let content =
        tokio::task::spawn_blocking(move || extract_resource(&materialized.book, &source_media, &source_resource))
            .await
            .map_err(|error| {
                tracing::error!(%error, %book_id, "EPUB resource extractor panicked");
                ApiError::internal("Could not read EPUB resource")
            })?
            .map_err(map_media_error)?;
    Ok(epub_resource_response(content, &resource, &media))
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
        MediaOperationError::UnsupportedProfile => {
            ApiError::bad_request("Book media profile does not support this operation")
        }
        MediaOperationError::ResourceNotFound => ApiError::not_found("EPUB resource not found"),
        MediaOperationError::NotReady => ApiError::not_found("Book analysis failed"),
        MediaOperationError::Image(message) => ApiError::not_found(message),
        MediaOperationError::Read(message) => {
            tracing::warn!(%message, "could not read book page");
            ApiError::not_found("File not found, it may have moved")
        }
    }
}

fn is_font_resource(resource: &str) -> bool {
    resource
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .is_some_and(|extension| matches!(extension.as_str(), "otf" | "woff" | "woff2" | "eot" | "ttf" | "svg"))
}

fn accepts_pdf(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| Accept::try_from(value).ok())
        .and_then(|accept| accept.negotiate([&APPLICATION_PDF, &IMAGE_JPEG]))
        .is_some_and(|media_type| media_type == &APPLICATION_PDF)
}

fn page_dtos(media: &Media) -> Vec<PageDto> {
    media
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let dimension = page_output_dimension(media, page);
            PageDto {
                number: i32::try_from(index + 1).unwrap_or(i32::MAX),
                file_name: page.file_name.clone(),
                media_type: page_output_media_type(media, page).to_owned(),
                width: dimension.map(|value| value.width),
                height: dimension.map(|value| value.height),
                size_bytes: page.file_size,
                size: page.file_size.map(binary_size).unwrap_or_default(),
            }
        })
        .collect()
}

fn binary_size(bytes: i64) -> String {
    if bytes < 0 {
        return String::new();
    }
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        let rounded = (value * 10.0).round() / 10.0;
        if rounded.fract() == 0.0 {
            format!("{} {}", rounded as i64, UNITS[unit])
        } else {
            format!("{rounded:.1} {}", UNITS[unit])
        }
    }
}

fn content_response(content: TypedBytes, file_stem: &str, media: &Media) -> Response {
    let extension = match content.media_type.as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
        "image/jxl" => "jxl",
        "image/jp2" => "jp2",
        "image/j2c" | "image/x-j2c" => "j2c",
        "image/heif" => "heif",
        "image/heic" => "heic",
        "image/avif" => "avif",
        "image/x-targa" | "image/x-tga" => "tga",
        "image/vnd-ms.dds" | "image/x-dds" => "dds",
        "image/vnd.radiance" | "image/x-hdr" => "hdr",
        "image/x-exr" | "image/exr" => "exr",
        "image/x-portable-bitmap" => "pbm",
        "image/x-portable-graymap" => "pgm",
        "image/x-portable-pixmap" => "ppm",
        "image/x-portable-anymap" => "pnm",
        "image/x-farbfeld" => "ff",
        "image/x-qoi" | "image/qoi" => "qoi",
        PDF_MEDIA_TYPE => "pdf",
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

fn epub_resource_response(content: TypedBytes, resource: &str, media: &Media) -> Response {
    let file_name = resource.rsplit('/').next().unwrap_or(resource);
    let safe_name = file_name
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
    if let Ok(value) = HeaderValue::from_str(&format!("inline; filename=\"{safe_name}\"")) {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, value);
    }
    set_epub_content_security_policy(response.headers_mut());
    set_last_modified(response.headers_mut(), media);
    response
}

fn set_epub_content_security_policy(headers: &mut HeaderMap) {
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("script-src 'none'; object-src 'none';"),
    );
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

    #[test]
    fn epub_font_resources_match_kotlin_anonymous_extensions() {
        for resource in [
            "font.otf",
            "font.WOFF",
            "font.woff2",
            "font.eot",
            "font.ttf",
            "image.svg",
        ] {
            assert!(is_font_resource(resource), "{resource}");
        }
        for resource in ["chapter.xhtml", "style.css", "cover.jpg", "font.svgz"] {
            assert!(!is_font_resource(resource), "{resource}");
        }
    }
}
