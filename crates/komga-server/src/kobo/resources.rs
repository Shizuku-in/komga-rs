use axum::{
    Json,
    body::Bytes,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, Uri},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use komga_core::{
    DomainEvent, MediaExtension, R2Device, R2Location, R2Locator, R2Progression, ReadProgressEvent, SeriesDto, User,
    UserRole, progression,
};
use rand::distr::{Alphanumeric, SampleString};
use serde::Deserialize;

use crate::{
    AppState, book_file_routes, content_access::load_visible_book, error::ApiError, routes::ApiJson, thumbnail_routes,
    webpub_routes::ManifestUrls,
};

use super::{
    auth::KoboAuthContext,
    dto::{
        AuthDto, KoboBookMetadataDto, ReadingStateUpdateDto, ReadingStateUpdateResultDto, RequestResultDto,
        ResourcesDto, ResultDto, StatusDto, WrappedResultDto, metadata as metadata_dto, reading_state,
    },
};

const FALLBACK_RESOURCES: &str = include_str!("resources.json");

pub(super) async fn ping() -> Json<&'static str> {
    Json("pong")
}

pub(super) async fn initialization(
    Extension(auth): Extension<KoboAuthContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut resources: serde_json::Value =
        serde_json::from_str(FALLBACK_RESOURCES).expect("embedded Kobo Resources JSON is valid");
    let base = request_base(&headers, &uri)?;
    let base = base.trim_end_matches('/');
    let image_root = format!("{base}/kobo/{}/v1/books", auth.auth_token);
    resources["image_host"] = serde_json::Value::String(base.to_owned());
    resources["image_url_template"] = serde_json::Value::String(format!(
        "{image_root}/{{ImageId}}/thumbnail/{{Width}}/{{Height}}/false/image.jpg"
    ));
    resources["image_url_quality_template"] = serde_json::Value::String(format!(
        "{image_root}/{{ImageId}}/thumbnail/{{Width}}/{{Height}}/{{Quality}}/{{IsGreyscale}}/image.jpg"
    ));
    let mut response = Json(ResourcesDto { resources }).into_response();
    response
        .headers_mut()
        .insert("x-kobo-apitoken", HeaderValue::from_static("e30="));
    Ok(response)
}

pub(super) async fn auth_device(body: Bytes) -> Json<AuthDto> {
    let user_key = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("UserKey")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let mut rng = rand::rng();
    Json(AuthDto {
        access_token: Alphanumeric.sample_string(&mut rng, 24),
        refresh_token: Alphanumeric.sample_string(&mut rng, 24),
        token_type: "Bearer",
        tracking_id: uuid::Uuid::new_v4().to_string(),
        user_key,
    })
}

pub(super) async fn metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<Vec<KoboBookMetadataDto>>, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let projection = state
        .storage
        .find_sync_book_projection(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let thumbnail_id = state
        .storage
        .find_selected_book_thumbnail(&book_id)
        .await?
        .map(|thumbnail| thumbnail.id);
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    Ok(Json(vec![metadata_dto(
        &projection,
        thumbnail_id,
        book.file_size,
        download_url(&headers, &uri, &auth.auth_token, &book_id)?,
    )]))
}

pub(super) async fn get_state(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
) -> Result<Json<Vec<super::dto::ReadingStateDto>>, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let created = book.created_date.unwrap_or_else(|| Utc::now().naive_utc());
    let progress = state.storage.find_sync_read_progress(&book_id, &auth.user.id).await?;
    Ok(Json(vec![reading_state(progress.as_ref(), &book_id, created)]))
}

pub(super) async fn put_state(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    ApiJson(mut update): ApiJson<ReadingStateUpdateDto>,
) -> Result<Json<RequestResultDto>, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let Some(update) = update.reading_states.drain(..).next() else {
        return Err(ApiError::bad_request("ReadingStates must not be empty"));
    };
    if update.current_bookmark.location.is_none() || update.current_bookmark.content_source_progress_percent.is_none() {
        return Err(ApiError::bad_request(
            "CurrentBookmark.Location and ContentSourceProgressPercent are required",
        ));
    }
    let result = save_progress(&state, &auth, &book_id, update).await;
    if let Err(error) = &result {
        tracing::error!(%error, %book_id, "could not update Kobo progression");
    }
    let (request_result, bookmark, statistics, status) = if result.is_ok() {
        (
            ResultDto::Success,
            ResultDto::Success,
            ResultDto::Ignored,
            ResultDto::Success,
        )
    } else {
        (
            ResultDto::Failure,
            ResultDto::Failure,
            ResultDto::Failure,
            ResultDto::Failure,
        )
    };
    Ok(Json(RequestResultDto {
        request_result,
        update_results: vec![ReadingStateUpdateResultDto {
            entitlement_id: book_id,
            current_bookmark_result: WrappedResultDto { result: bookmark },
            statistics_result: WrappedResultDto { result: statistics },
            status_info_result: WrappedResultDto { result: status },
        }],
    }))
}

async fn save_progress(
    state: &AppState,
    auth: &KoboAuthContext,
    book_id: &str,
    update: super::dto::ReadingStateDto,
) -> Result<(), String> {
    let media = state
        .storage
        .find_media_by_book_id(book_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Book analysis is missing".to_owned())?;
    let location = update
        .current_bookmark
        .location
        .as_ref()
        .ok_or_else(|| "Bookmark location is required".to_owned())?;
    let source_progress = update
        .current_bookmark
        .content_source_progress_percent
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .ok_or_else(|| "ContentSourceProgressPercent must be between 0 and 100".to_owned())?;
    let total_progress = update.current_bookmark.progress_percent;
    if total_progress.is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value)) {
        return Err("ProgressPercent must be between 0 and 100".into());
    }
    let locator = if update.status_info.status == StatusDto::Finished {
        match media.extension.as_ref() {
            Some(MediaExtension::Epub(extension)) => extension
                .positions
                .last()
                .cloned()
                .ok_or_else(|| "EPUB has no reading positions".to_owned())?,
            None => return Err("EPUB extension is missing".into()),
        }
    } else {
        R2Locator {
            href: location.source.clone(),
            media_type: "application/xhtml+xml".into(),
            title: None,
            locations: Some(R2Location {
                progression: Some(source_progress / 100.0),
                total_progression: total_progress.map(|value| value / 100.0),
                ..R2Location::default()
            }),
            text: None,
            kobo_span: location
                .r#type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("kobospan"))
                .then(|| location.value.clone())
                .flatten(),
        }
    };
    let saved = state
        .storage
        .find_read_progress(book_id, &auth.user.id)
        .await
        .map_err(|error| error.to_string())?;
    let progress = progression(
        book_id,
        &auth.user.id,
        &media,
        saved.as_ref(),
        R2Progression {
            modified: update.last_modified,
            device: R2Device {
                id: auth.api_key_id.clone(),
                name: auth.api_key_comment.clone(),
            },
            locator,
        },
    )
    .map_err(|error| error.to_string())?;
    state
        .storage
        .save_read_progress(&progress)
        .await
        .map_err(|error| error.to_string())?;
    state.publish_domain_event(DomainEvent::ReadProgressChanged(ReadProgressEvent {
        book_id: book_id.to_owned(),
        user_id: auth.user.id.clone(),
    }));
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct BookFileQuery {
    #[serde(default)]
    convert_kepub: bool,
}

pub(super) async fn book_file(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    Query(query): Query<BookFileQuery>,
) -> Result<Response, ApiError> {
    if !auth.user.roles.contains(&UserRole::FileDownload) {
        return Err(ApiError::forbidden());
    }
    load_visible_book(&state, &auth.user, &book_id).await?;
    if query.convert_kepub {
        return Err(ApiError::service_unavailable("Kepub conversion is not available"));
    }
    book_file_routes::download_for_user(&state, &auth.user, &book_id).await
}

pub(super) async fn cover(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, thumbnail_id, _width, _height, _greyscale)): Path<(String, String, String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cover_impl(&state, &auth.user, &thumbnail_id, &headers).await
}

pub(super) async fn cover_with_quality(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, thumbnail_id, _width, _height, _quality, _greyscale)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cover_impl(&state, &auth.user, &thumbnail_id, &headers).await
}

async fn cover_impl(
    state: &AppState,
    user: &User,
    thumbnail_id: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let thumbnail = state
        .storage
        .find_book_thumbnail_by_id(thumbnail_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
    ensure_cover_visible(state, user, &thumbnail.book_id).await?;
    let mut bytes = thumbnail_routes::read_thumbnail(thumbnail.thumbnail, thumbnail.url).await?;
    if thumbnail.media_type != "image/jpeg" {
        bytes = komga_media::convert_image(&bytes, komga_media::ImageOutput::Jpeg, None)
            .map_err(|error| {
                tracing::error!(%error, %thumbnail_id, "could not convert Kobo cover to JPEG");
                ApiError::internal("Could not convert book thumbnail")
            })?
            .bytes;
    }
    thumbnail_routes::thumbnail_response(bytes, headers)
}

async fn ensure_cover_visible(state: &AppState, user: &User, book_id: &str) -> Result<(), ApiError> {
    let book = state
        .storage
        .find_book_dto_by_id(book_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let series = state
        .storage
        .find_series_dto_by_id(&book.series_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    if !series_visible(user, &series) {
        return Err(ApiError::not_found("Book not found"));
    }
    Ok(())
}

fn series_visible(user: &User, series: &SeriesDto) -> bool {
    let age = series.metadata.age_rating.and_then(|value| u32::try_from(value).ok());
    user.can_access_library(&series.library_id) && user.is_content_allowed(age, &series.metadata.sharing_labels)
}

pub(super) async fn catch_all() -> Json<serde_json::Value> {
    Json(serde_json::json!({}))
}

pub(super) fn download_url(
    headers: &HeaderMap,
    uri: &Uri,
    auth_token: &str,
    book_id: &str,
) -> Result<String, ApiError> {
    Ok(format!(
        "{}?convert_kepub=false",
        ManifestUrls::from_request_with_prefix(headers, uri, &[])?
            .absolute_path(&["kobo", auth_token, "v1", "books", book_id, "file", "epub",])
    ))
}

fn request_base(headers: &HeaderMap, uri: &Uri) -> Result<String, ApiError> {
    Ok(ManifestUrls::from_request_with_prefix(headers, uri, &[])?.absolute_path(&[]))
}
