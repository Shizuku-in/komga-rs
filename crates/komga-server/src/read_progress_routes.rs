//! Traditional and Readium reading-progress HTTP endpoints.
//!
//! These handlers apply the same library, age, and sharing-label visibility
//! checks as book delivery. Progress derivation remains in `komga-core`, while
//! persistence and series aggregation are handled atomically by storage.

use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use komga_core::{
    Media, R2Progression, ReadProgressError, SeriesReadProgressDto, completed_progress, page_progress, progression,
};
use serde::Deserialize;

use crate::{
    AppState,
    auth::AuthContext,
    content_access::{load_visible_book, load_visible_series},
    error::ApiError,
    routes::ApiJson,
};

const PROGRESSION_MEDIA_TYPE: &str = "application/vnd.readium.progression+json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReadProgressUpdateDto {
    page: Option<i32>,
    completed: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TachiyomiReadProgressUpdateDto {
    last_book_number_sort_read: f64,
}

/// Marks every incomplete book in a visible series complete.
pub(crate) async fn mark_series_read_completed(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    load_visible_series(&state, &auth.user, &series_id).await?;
    state
        .storage
        .mark_series_read_completed(&series_id, &auth.user.id, None, Utc::now().naive_utc())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes all progress for a visible series.
pub(crate) async fn delete_series_read_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    load_visible_series(&state, &auth.user, &series_id).await?;
    state
        .storage
        .delete_series_read_progress(&series_id, &auth.user.id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Returns the Mihon/Tachiyomi series progress contract.
pub(crate) async fn get_tachiyomi_series_read_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<Json<SeriesReadProgressDto>, ApiError> {
    load_visible_series(&state, &auth.user, &series_id).await?;
    Ok(Json(
        state
            .storage
            .find_series_read_progress(&series_id, &auth.user.id)
            .await?,
    ))
}

/// Marks books through the supplied Mihon/Tachiyomi sort number complete.
pub(crate) async fn update_tachiyomi_series_read_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
    ApiJson(update): ApiJson<TachiyomiReadProgressUpdateDto>,
) -> Result<StatusCode, ApiError> {
    if !update.last_book_number_sort_read.is_finite() {
        return Err(ApiError::bad_request("lastBookNumberSortRead must be finite"));
    }
    load_visible_series(&state, &auth.user, &series_id).await?;
    state
        .storage
        .mark_series_read_completed(
            &series_id,
            &auth.user.id,
            Some(update.last_book_number_sort_read),
            Utc::now().naive_utc(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Updates one user's traditional one-based page progress.
pub(crate) async fn update_read_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    ApiJson(update): ApiJson<ReadProgressUpdateDto>,
) -> Result<StatusCode, ApiError> {
    validate_page_update(&update)?;
    load_visible_book(&state, &auth.user, &book_id).await?;
    let media = load_media(&state, &book_id).await?;
    let read_date = Utc::now().naive_utc();
    let progress = if update.completed == Some(true) {
        completed_progress(&book_id, &auth.user.id, &media, read_date)
    } else {
        page_progress(
            &book_id,
            &auth.user.id,
            &media,
            update.page.expect("validated page update"),
            read_date,
        )
        .map_err(map_progress_error)?
    };
    state.storage.save_read_progress(&progress).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes one user's progress. Missing progress is intentionally idempotent.
pub(crate) async fn delete_read_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    state.storage.delete_read_progress(&book_id, &auth.user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Returns one user's stored Readium progression, when present.
pub(crate) async fn get_progression(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Response, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let Some(progress) = state.storage.find_read_progress(&book_id, &auth.user.id).await? else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let mut response = Json(progress.to_r2_progression()).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROGRESSION_MEDIA_TYPE));
    Ok(response)
}

/// Stores a newer Readium progression after normalizing it against analyzed media.
pub(crate) async fn update_progression(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    ApiJson(update): ApiJson<R2Progression>,
) -> Result<StatusCode, ApiError> {
    load_visible_book(&state, &auth.user, &book_id).await?;
    let media = load_media(&state, &book_id).await?;
    let saved = state.storage.find_read_progress(&book_id, &auth.user.id).await?;
    let progress = progression(&book_id, &auth.user.id, &media, saved.as_ref(), update).map_err(map_progress_error)?;
    state.storage.save_read_progress(&progress).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_page_update(update: &ReadProgressUpdateDto) -> Result<(), ApiError> {
    if update.completed == Some(true) {
        return Ok(());
    }
    match update.page {
        Some(page) if page > 0 => Ok(()),
        Some(_) => Err(ApiError::bad_request("page must be greater than zero")),
        None => Err(ApiError::bad_request("page is required unless completed is true")),
    }
}

async fn load_media(state: &AppState, book_id: &str) -> Result<Media, ApiError> {
    state
        .storage
        .find_media_by_book_id(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book analysis failed"))
}

fn map_progress_error(error: ReadProgressError) -> ApiError {
    match error {
        ReadProgressError::Invalid(message) => ApiError::bad_request(message),
        ReadProgressError::Conflict => ApiError::conflict("Progression is older than existing"),
    }
}
