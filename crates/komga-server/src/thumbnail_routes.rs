//! Book and series poster management.

use axum::{
    Json,
    body::Body,
    extract::{Extension, Multipart, Path, Query, State, multipart::MultipartError},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use komga_core::{Dimension, ThumbnailBook, ThumbnailBookDto, ThumbnailSeries, ThumbnailSeriesDto, new_id};
use komga_media::{detect_image_media_type, image_dimension};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{
    AppState,
    auth::AuthContext,
    content_access::{load_visible_book, load_visible_series},
    error::ApiError,
    routes::require_admin,
};
use komga_storage::TaskRecord;

#[derive(Debug, Default, Deserialize)]
pub(crate) struct UploadQuery {
    pub(crate) selected: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct RegenerateQuery {
    #[serde(default)]
    for_bigger_result_only: bool,
}

pub(crate) async fn get_book_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    thumbnail_response(selected_book_thumbnail_bytes(&state, &book_id).await?, &headers)
}

pub(crate) async fn selected_book_thumbnail_bytes(state: &AppState, book_id: &str) -> Result<Vec<u8>, ApiError> {
    let thumbnail = state
        .storage
        .find_selected_book_thumbnail(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
    let bytes = match read_thumbnail(thumbnail.thumbnail, thumbnail.url).await {
        Ok(bytes) => bytes,
        Err(error) => {
            state
                .storage
                .remove_stale_book_thumbnail(book_id, &thumbnail.id)
                .await?;
            let replacement = state
                .storage
                .find_selected_book_thumbnail(book_id)
                .await?
                .ok_or(error)?;
            read_thumbnail(replacement.thumbnail, replacement.url).await?
        }
    };
    Ok(bytes)
}

pub(crate) async fn get_book_thumbnail_by_id(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, thumbnail_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let thumbnail = state
        .storage
        .find_book_thumbnail(&book_id, &thumbnail_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
    thumbnail_response(read_thumbnail(thumbnail.thumbnail, thumbnail.url).await?, &headers)
}

pub(crate) async fn list_book_thumbnails(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Json<Vec<ThumbnailBookDto>>, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    Ok(Json(
        state
            .storage
            .list_book_thumbnails(&book_id)
            .await?
            .iter()
            .map(ThumbnailBook::to_dto)
            .collect(),
    ))
}

pub(crate) async fn upload_book_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    Query(query): Query<UploadQuery>,
    multipart: Multipart,
) -> Result<Json<ThumbnailBookDto>, ApiError> {
    require_admin(&auth)?;
    state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let upload = parse_upload(multipart, query.selected).await?;
    let thumbnail = ThumbnailBook {
        id: new_id(),
        thumbnail: Some(upload.bytes),
        url: None,
        selected: false,
        thumbnail_type: "USER_UPLOADED".into(),
        media_type: upload.media_type,
        file_size: upload.file_size,
        dimension: upload.dimension,
        book_id,
    };
    Ok(Json(
        state
            .storage
            .insert_book_thumbnail(&thumbnail, upload.selected)
            .await?
            .to_dto(),
    ))
}

pub(crate) async fn select_book_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, thumbnail_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    if !state.storage.select_book_thumbnail(&book_id, &thumbnail_id).await? {
        return Err(ApiError::not_found("Book thumbnail not found"));
    }
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn delete_book_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, thumbnail_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let thumbnail = state
        .storage
        .find_book_thumbnail(&book_id, &thumbnail_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
    if thumbnail.thumbnail_type != "USER_UPLOADED" {
        return Err(ApiError::bad_request("Only uploaded thumbnails can be deleted"));
    }
    state.storage.delete_book_thumbnail(&book_id, &thumbnail_id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn get_series_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let series = load_visible_series(&state, &auth.user, &series_id).await?;
    if let Some(thumbnail) = state.storage.find_selected_series_thumbnail(&series_id).await? {
        match read_thumbnail(thumbnail.thumbnail, thumbnail.url).await {
            Ok(bytes) => return thumbnail_response(bytes, &headers),
            Err(_) => {
                state
                    .storage
                    .remove_stale_series_thumbnail(&series_id, &thumbnail.id)
                    .await?;
                if let Some(replacement) = state.storage.find_selected_series_thumbnail(&series_id).await? {
                    return thumbnail_response(read_thumbnail(replacement.thumbnail, replacement.url).await?, &headers);
                }
            }
        }
    }
    let library = state
        .storage
        .find_library_by_id(&series.library_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Library not found"))?;
    let book_id = state
        .storage
        .find_series_cover_book_id(&series_id, &auth.user.id, library.series_cover)
        .await?
        .ok_or_else(|| ApiError::not_found("Series thumbnail not found"))?;
    let bytes = selected_book_thumbnail_bytes(&state, &book_id).await?;
    thumbnail_response(bytes, &headers)
}

pub(crate) async fn get_series_thumbnail_by_id(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((series_id, thumbnail_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    load_visible_series(&state, &auth.user, &series_id).await?;
    let thumbnail = state
        .storage
        .find_series_thumbnail(&series_id, &thumbnail_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series thumbnail not found"))?;
    thumbnail_response(read_thumbnail(thumbnail.thumbnail, thumbnail.url).await?, &headers)
}

pub(crate) async fn list_series_thumbnails(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<Json<Vec<ThumbnailSeriesDto>>, ApiError> {
    load_visible_series(&state, &auth.user, &series_id).await?;
    Ok(Json(
        state
            .storage
            .list_series_thumbnails(&series_id)
            .await?
            .iter()
            .map(ThumbnailSeries::to_dto)
            .collect(),
    ))
}

pub(crate) async fn upload_series_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
    Query(query): Query<UploadQuery>,
    multipart: Multipart,
) -> Result<Json<ThumbnailSeriesDto>, ApiError> {
    require_admin(&auth)?;
    let series = state
        .storage
        .find_series_dto_by_id(&series_id, &auth.user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    if series.oneshot {
        return Err(ApiError::bad_request("One-shot series cannot have a series thumbnail"));
    }
    let upload = parse_upload(multipart, query.selected).await?;
    let thumbnail = ThumbnailSeries {
        id: new_id(),
        thumbnail: Some(upload.bytes),
        url: None,
        selected: false,
        thumbnail_type: "USER_UPLOADED".into(),
        media_type: upload.media_type,
        file_size: upload.file_size,
        dimension: upload.dimension,
        series_id,
    };
    Ok(Json(
        state
            .storage
            .insert_series_thumbnail(&thumbnail, upload.selected)
            .await?
            .to_dto(),
    ))
}

pub(crate) async fn select_series_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((series_id, thumbnail_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state
        .storage
        .find_series_dto_by_id(&series_id, &auth.user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    if !state.storage.select_series_thumbnail(&series_id, &thumbnail_id).await? {
        return Err(ApiError::not_found("Series thumbnail not found"));
    }
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn delete_series_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((series_id, thumbnail_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state
        .storage
        .find_series_dto_by_id(&series_id, &auth.user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    let thumbnail = state
        .storage
        .find_series_thumbnail(&series_id, &thumbnail_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series thumbnail not found"))?;
    if thumbnail.thumbnail_type != "USER_UPLOADED" {
        return Err(ApiError::bad_request("Only uploaded thumbnails can be deleted"));
    }
    state.storage.delete_series_thumbnail(&series_id, &thumbnail_id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn regenerate_book_thumbnails(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<RegenerateQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state
        .enqueue_task(&TaskRecord::find_book_thumbnails_to_regenerate(
            query.for_bigger_result_only,
            0,
        ))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) struct Upload {
    pub(crate) bytes: Vec<u8>,
    pub(crate) selected: bool,
    pub(crate) media_type: String,
    pub(crate) file_size: i64,
    pub(crate) dimension: Dimension,
}

pub(crate) async fn parse_upload(mut multipart: Multipart, query_selected: Option<bool>) -> Result<Upload, ApiError> {
    let mut bytes = None;
    let mut form_selected = None;
    while let Some(field) = multipart.next_field().await.map_err(map_multipart_error)? {
        match field.name() {
            Some("file") if bytes.is_none() => {
                bytes = Some(field.bytes().await.map_err(map_multipart_error)?.to_vec());
            }
            Some("selected") if query_selected.is_none() => {
                let value = field.text().await.map_err(map_multipart_error)?;
                form_selected = Some(
                    value
                        .parse::<bool>()
                        .map_err(|_| ApiError::bad_request("selected must be true or false"))?,
                );
            }
            _ => {}
        }
    }
    let bytes = bytes.ok_or_else(|| ApiError::bad_request("file is required"))?;
    let media_type = detect_image_media_type(&bytes).ok_or_else(ApiError::unsupported_media_type)?;
    let dimension = image_dimension(&bytes).unwrap_or(Dimension { width: 0, height: 0 });
    Ok(Upload {
        file_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        bytes,
        selected: query_selected.or(form_selected).unwrap_or(true),
        media_type,
        dimension,
    })
}

fn map_multipart_error(error: MultipartError) -> ApiError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        ApiError::payload_too_large()
    } else {
        ApiError::bad_request(error.to_string())
    }
}

pub(crate) async fn read_thumbnail(bytes: Option<Vec<u8>>, url: Option<String>) -> Result<Vec<u8>, ApiError> {
    if let Some(bytes) = bytes {
        return Ok(bytes);
    }
    let path = url
        .and_then(|value| Url::parse(&value).ok())
        .and_then(|value| value.to_file_path().ok())
        .ok_or_else(|| ApiError::not_found("Thumbnail not found"))?;
    tokio::fs::read(path)
        .await
        .map_err(|_| ApiError::not_found("Thumbnail not found"))
}

pub(crate) fn thumbnail_response(bytes: Vec<u8>, headers: &HeaderMap) -> Result<Response, ApiError> {
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(&bytes)));
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|candidate| candidate.trim() == etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        set_thumbnail_headers(response.headers_mut(), &etag)?;
        return Ok(response);
    }
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    set_thumbnail_headers(response.headers_mut(), &etag)?;
    Ok(response)
}

fn set_thumbnail_headers(headers: &mut HeaderMap, etag: &str) -> Result<(), ApiError> {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=0, must-revalidate"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).map_err(|_| ApiError::internal("Could not create thumbnail ETag"))?,
    );
    Ok(())
}
