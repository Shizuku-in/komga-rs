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
use komga_core::{BookEvent, BookMetadata, DomainEvent, MetadataError, SeriesEvent};
use komga_media::apply_series_title_sort;
use komga_storage::TaskRecord;

use crate::{
    AppState,
    auth::AuthContext,
    dto::{BookMetadataUpdateDto, SeriesMetadataUpdateDto},
    error::ApiError,
    routes::{ApiJson, require_admin},
};

const DEFAULT_TASK_PRIORITY: i32 = 4;
const HIGH_TASK_PRIORITY: i32 = 6;

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
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    if !state.storage.save_book_metadata(&updated).await? {
        return Err(ApiError::not_found("Book not found"));
    }
    state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(&book)));
    if !state.sync_books(std::slice::from_ref(&book_id)).await {
        state.recover_search_after_http_failure().await;
    }
    state
        .enqueue_task(&TaskRecord::aggregate_series_metadata(
            &book.series_id,
            DEFAULT_TASK_PRIORITY,
        ))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Updates metadata for every existing book named in the request map.
pub(crate) async fn update_book_metadata_batch(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(updates): ApiJson<BTreeMap<String, BookMetadataUpdateDto>>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;

    let mut prepared = Vec::<(BookMetadata, komga_core::Book)>::new();
    for (book_id, update) in updates {
        let Some(existing) = state.storage.find_book_metadata(&book_id).await? else {
            continue;
        };
        let updated = existing.apply_patch(update.into_patch()).map_err(map_metadata_error)?;
        if let Some(book) = state.storage.find_book_by_id(&book_id).await? {
            prepared.push((updated, book));
        }
    }

    let mut affected_series = BTreeSet::new();
    let mut affected_books = Vec::new();
    for (metadata, book) in prepared {
        if state.storage.save_book_metadata(&metadata).await? {
            state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(&book)));
            affected_books.push(metadata.book_id.clone());
            affected_series.insert(book.series_id);
        }
    }
    if !state.sync_books(&affected_books).await {
        state.recover_search_after_http_failure().await;
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
    let explicit_title_sort = update.has_explicit_title_sort();
    let mut updated = existing.apply_patch(update.into_patch()).map_err(map_metadata_error)?;
    apply_series_title_sort(&existing, &mut updated, explicit_title_sort);
    let series = state
        .storage
        .find_series_by_id(&series_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    if !state.storage.save_series_metadata(&updated).await? {
        return Err(ApiError::not_found("Series not found"));
    }
    state.publish_domain_event(DomainEvent::SeriesUpdated(SeriesEvent::from(&series)));
    if !state.sync_series(std::slice::from_ref(&series_id)).await {
        state.recover_search_after_http_failure().await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Queues automatic metadata providers for one book.
pub(crate) async fn refresh_book_metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .filter(|b| b.deleted_date.is_none())
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    state
        .enqueue_task(&TaskRecord::refresh_book_metadata(
            &book.id,
            &book.series_id,
            DEFAULT_TASK_PRIORITY,
        ))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

/// Queues automatic metadata providers for one series.
pub(crate) async fn refresh_series_metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let series = state
        .storage
        .find_series_by_id(&series_id)
        .await?
        .filter(|s| s.deleted_date.is_none())
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    state
        .enqueue_task(&TaskRecord::refresh_series_metadata(&series.id, DEFAULT_TASK_PRIORITY))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

/// Queues metadata and local-artwork refreshes for a library's content.
pub(crate) async fn refresh_library_metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    for book in state.storage.list_books_by_library_id(&library_id).await? {
        state
            .enqueue_task(&TaskRecord::refresh_book_metadata(
                &book.id,
                &book.series_id,
                HIGH_TASK_PRIORITY,
            ))
            .await?;
        state
            .enqueue_task(&TaskRecord::refresh_book_local_artwork(&book.id, HIGH_TASK_PRIORITY))
            .await?;
    }
    for series in state.storage.list_series_by_library_id(&library_id).await? {
        state
            .enqueue_task(&TaskRecord::refresh_series_local_artwork(
                &series.id,
                HIGH_TASK_PRIORITY,
            ))
            .await?;
    }
    Ok(StatusCode::ACCEPTED)
}

fn map_metadata_error(error: MetadataError) -> ApiError {
    match error {
        MetadataError::Invalid(message) => ApiError::bad_request(message),
    }
}
