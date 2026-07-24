//! Administrator endpoints for editing book and series metadata.
//!
//! JSON omission and explicit `null` are resolved in the HTTP DTOs before the
//! framework-independent metadata policy is applied. Book changes enqueue the
//! same series-aggregation task used by Kotlin Komga.

use std::collections::{BTreeMap, BTreeSet};

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
};
use komga_core::{BookMetadata, MetadataError};
use komga_storage::TaskRecord;

use crate::{
    AppState,
    auth::AuthContext,
    dto::{BookMetadataUpdateDto, SeriesMetadataUpdateDto},
    error::ApiError,
    routes::{ApiJson, require_admin},
};

const DEFAULT_TASK_PRIORITY: i32 = 4;

/// Updates the editable metadata for one book.
pub(crate) async fn update_book_metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    ApiJson(update): ApiJson<BookMetadataUpdateDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let existing = state
        .storage
        .find_book_metadata(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let updated = existing.apply_patch(update.into_patch()).map_err(map_metadata_error)?;
    if !state.storage.save_book_metadata(&updated).await? {
        return Err(ApiError::not_found("Book not found"));
    }
    if let Some(book) = state.storage.find_book_by_id(&book_id).await? {
        state
            .enqueue_task(&TaskRecord::aggregate_series_metadata(
                &book.series_id,
                DEFAULT_TASK_PRIORITY,
            ))
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Updates metadata for every existing book named in the request map.
pub(crate) async fn update_book_metadata_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(updates): ApiJson<BTreeMap<String, BookMetadataUpdateDto>>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;

    let mut prepared = Vec::<(BookMetadata, String)>::new();
    for (book_id, update) in updates {
        let Some(existing) = state.storage.find_book_metadata(&book_id).await? else {
            continue;
        };
        let updated = existing.apply_patch(update.into_patch()).map_err(map_metadata_error)?;
        if let Some(book) = state.storage.find_book_by_id(&book_id).await? {
            prepared.push((updated, book.series_id));
        }
    }

    let mut affected_series = BTreeSet::new();
    for (metadata, series_id) in prepared {
        if state.storage.save_book_metadata(&metadata).await? {
            affected_series.insert(series_id);
        }
    }
    for series_id in affected_series {
        state
            .enqueue_task(&TaskRecord::aggregate_series_metadata(
                &series_id,
                DEFAULT_TASK_PRIORITY,
            ))
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Updates the editable metadata for one series.
pub(crate) async fn update_series_metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
    ApiJson(update): ApiJson<SeriesMetadataUpdateDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let existing = state
        .storage
        .find_series_metadata(&series_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    let updated = existing.apply_patch(update.into_patch()).map_err(map_metadata_error)?;
    if !state.storage.save_series_metadata(&updated).await? {
        return Err(ApiError::not_found("Series not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn map_metadata_error(error: MetadataError) -> ApiError {
    match error {
        MetadataError::Invalid(message) => ApiError::bad_request(message),
    }
}
