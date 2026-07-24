//! Background task processing and periodic library scan scheduling.

use std::{collections::HashMap, time::Duration};

use komga_core::{
    MediaStatus, MetadataUpdate, ScanError, ScanInterval, SeriesMetadataPatch, ThumbnailBook, new_id,
    scan_library_files,
};
use komga_media::{
    MediaAnalysisError, MediaOperationError, analyze_book, extract_comic_info_metadata, extract_epub_metadata,
    extract_mylar_metadata, generate_thumbnail, image_dimension,
};
use komga_storage::{StorageError, TaskRecord};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::AppState;

/// Cloneable control used to coordinate HTTP and background-service shutdown.
#[derive(Clone)]
pub struct ShutdownHandle {
    token: CancellationToken,
}

impl ShutdownHandle {
    /// Requests shutdown of the HTTP server, workers, scheduler, and SSE streams.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Resolves when shutdown has been requested, consuming this handle.
    pub async fn cancelled_owned(self) {
        self.token.cancelled().await;
    }
}

/// Owned handles for background services started around an [`AppState`].
pub struct AppRuntime {
    shutdown: ShutdownHandle,
    task_notify: std::sync::Arc<tokio::sync::Notify>,
    schedule_notify: std::sync::Arc<tokio::sync::Notify>,
    handles: Vec<JoinHandle<()>>,
}

impl AppRuntime {
    /// Returns a shutdown handle suitable for signal handling and Axum.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }

    /// Cancels background work and waits for every service to release its resources.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        self.task_notify.notify_waiters();
        self.schedule_notify.notify_waiters();
        for handle in self.handles {
            if let Err(error) = handle.await {
                tracing::error!(%error, "background service terminated unexpectedly");
            }
        }
    }
}

impl AppState {
    /// Starts scan workers and the periodic library scheduler.
    ///
    /// State construction intentionally does not start background work, allowing
    /// HTTP tests and embedding applications to control lifecycle explicitly.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the configured task pool size cannot be read.
    pub async fn start_runtime(&self) -> Result<AppRuntime, StorageError> {
        let worker_count = self.storage.task_pool_size().await?;
        let mut handles = Vec::with_capacity(worker_count + 1);
        for index in 0..worker_count {
            handles.push(tokio::spawn(worker_loop(self.clone(), index)));
        }
        handles.push(tokio::spawn(scheduler_loop(self.clone())));
        self.task_notify.notify_waiters();
        Ok(AppRuntime {
            shutdown: ShutdownHandle {
                token: self.shutdown.clone(),
            },
            task_notify: self.task_notify.clone(),
            schedule_notify: self.schedule_notify.clone(),
            handles,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanLibraryPayload {
    library_id: String,
    scan_deep: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LibraryTaskPayload {
    library_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookTaskPayload {
    book_id: String,
    #[serde(default)]
    capabilities: Vec<MetadataCapability>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesTaskPayload {
    series_id: String,
    #[serde(default)]
    capabilities: Vec<MetadataCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum MetadataCapability {
    Status,
    Title,
    Summary,
    Number,
    NumberSort,
    ReleaseDate,
    Authors,
    Tags,
    Isbn,
    ReadLists,
    Thumbnails,
    Links,
    ReadingDirection,
    Publisher,
    AgeRating,
    Language,
    Genres,
    TotalBookCount,
    Collections,
}

fn has_capability(requested: &[MetadataCapability], supported: &[MetadataCapability]) -> bool {
    requested.is_empty() || requested.iter().any(|value| supported.contains(value))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FindThumbnailsPayload {
    for_bigger_result_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskOutcome {
    Completed,
    Cancelled,
    Retry,
}

const SUPPORTED_TASK_TYPES: [&str; 8] = [
    "ScanLibrary",
    "EmptyTrash",
    "AnalyzeBook",
    "RefreshBookMetadata",
    "RefreshSeriesMetadata",
    "GenerateBookThumbnail",
    "AggregateSeriesMetadata",
    "FindBookThumbnailsToRegenerate",
];

async fn worker_loop(state: AppState, index: usize) {
    let owner = format!("komga-rs-{}-{index}", std::process::id());
    loop {
        let notified = state.task_notify.notified();
        tokio::pin!(notified);
        if state.shutdown.is_cancelled() {
            break;
        }
        match state.tasks.take_first_of_types(&owner, &SUPPORTED_TASK_TYPES).await {
            Ok(Some(task)) => {
                let outcome = process_task(&state, &task).await;
                let result = match outcome {
                    TaskOutcome::Completed => state.tasks.delete(&task.id).await,
                    TaskOutcome::Cancelled | TaskOutcome::Retry => state.tasks.release(&task.id, &owner).await,
                };
                if let Err(error) = result {
                    tracing::error!(%error, task_id = %task.id, "could not finalize task ownership");
                }
                if outcome == TaskOutcome::Retry {
                    tokio::select! {
                        () = state.shutdown.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
            Ok(None) => {
                tokio::select! {
                    () = state.shutdown.cancelled() => break,
                    () = &mut notified => {}
                }
            }
            Err(error) => {
                tracing::error!(%error, worker = %owner, "could not claim task");
                tokio::select! {
                    () = state.shutdown.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
    }
}

async fn process_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    match task.simple_type.as_str() {
        "ScanLibrary" => process_scan_task(state, task).await,
        "EmptyTrash" => process_empty_trash_task(state, task).await,
        "AnalyzeBook" => process_analyze_book_task(state, task).await,
        "RefreshBookMetadata" => process_refresh_book_metadata_task(state, task).await,
        "RefreshSeriesMetadata" => process_refresh_series_metadata_task(state, task).await,
        "GenerateBookThumbnail" => process_generate_thumbnail_task(state, task).await,
        "AggregateSeriesMetadata" => process_aggregate_series_metadata_task(state, task).await,
        "FindBookThumbnailsToRegenerate" => process_find_thumbnails_task(state, task).await,
        _ => TaskOutcome::Completed,
    }
}

async fn process_find_thumbnails_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<FindThumbnailsPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid FindBookThumbnailsToRegenerate payload");
            return TaskOutcome::Completed;
        }
    };
    let smaller_than = if payload.for_bigger_result_only {
        match state.storage.thumbnail_max_edge().await {
            Ok(size) => Some(i32::try_from(size).unwrap_or(i32::MAX)),
            Err(error) => {
                tracing::error!(%error, "could not read thumbnail size");
                return TaskOutcome::Retry;
            }
        }
    } else {
        None
    };
    let targets = match state
        .storage
        .list_book_thumbnail_regeneration_targets(smaller_than)
        .await
    {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, "could not find thumbnail regeneration targets");
            return TaskOutcome::Retry;
        }
    };
    for book_id in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::generate_book_thumbnail(&book_id, task.priority))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue generated thumbnail");
            return TaskOutcome::Retry;
        }
    }
    TaskOutcome::Completed
}

async fn process_scan_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<ScanLibraryPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid ScanLibrary payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => {
            tracing::warn!(library_id = %payload.library_id, "scan task references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load library for scan");
            return TaskOutcome::Retry;
        }
    };
    let empty_trash_after_scan = library.empty_trash_after_scan;
    let cancellation = state.shutdown.clone();
    let scan = tokio::task::spawn_blocking(move || scan_library_files(&library, || cancellation.is_cancelled())).await;
    let scan = match scan {
        Ok(Ok(scan)) => scan,
        Ok(Err(ScanError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(ScanError::RootUnavailable(path))) => {
            if let Err(error) = state.storage.set_library_unavailable(&payload.library_id, true).await {
                tracing::error!(%error, library_id = %payload.library_id, "could not mark library unavailable");
            }
            tracing::error!(library_id = %payload.library_id, root = %path.display(), "library scan failed");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "filesystem scanner panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .apply_library_scan(&payload.library_id, &scan, payload.scan_deep)
        .await
    {
        Ok(stats) => {
            tracing::info!(library_id = %payload.library_id, ?stats, "library scan completed");
            for series_id in stats.sidecars_changed {
                if let Err(error) = state
                    .enqueue_task(&TaskRecord::refresh_series_metadata(&series_id, task.priority))
                    .await
                {
                    tracing::error!(%error, %series_id, "could not enqueue sidecar metadata refresh");
                    return TaskOutcome::Retry;
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not persist library scan");
            return TaskOutcome::Retry;
        }
    }
    if empty_trash_after_scan {
        if perform_empty_trash(state, &payload.library_id).await == TaskOutcome::Retry {
            return TaskOutcome::Retry;
        }
    } else if let Err(error) = state.storage.cleanup_empty_containers().await {
        tracing::error!(%error, library_id = %payload.library_id, "could not clean empty collections and read lists");
        return TaskOutcome::Retry;
    }
    let targets = match state.storage.list_book_analysis_targets(&payload.library_id).await {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not list books requiring analysis");
            return TaskOutcome::Retry;
        }
    };
    for (book_id, series_id) in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::analyze_book(&book_id, &series_id, task.priority))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue book analysis");
            return TaskOutcome::Retry;
        }
    }
    if payload.scan_deep {
        let targets = match state.storage.list_ready_books_by_library_id(&payload.library_id).await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::error!(%error, library_id = %payload.library_id, "could not list ready books for metadata refresh");
                return TaskOutcome::Retry;
            }
        };
        for (book_id, series_id) in targets {
            if let Err(error) = state
                .enqueue_task(&TaskRecord::refresh_book_metadata(&book_id, &series_id, task.priority))
                .await
            {
                tracing::error!(%error, %book_id, "could not enqueue book metadata refresh");
                return TaskOutcome::Retry;
            }
        }
    }
    TaskOutcome::Completed
}

async fn process_empty_trash_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<LibraryTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid EmptyTrash payload");
            return TaskOutcome::Completed;
        }
    };
    perform_empty_trash(state, &payload.library_id).await
}

async fn perform_empty_trash(state: &AppState, library_id: &str) -> TaskOutcome {
    let result = match state.storage.empty_trash(library_id).await {
        Ok(Some(result)) => result,
        Ok(None) => {
            tracing::warn!(%library_id, "empty-trash task references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, %library_id, "could not empty library trash");
            return TaskOutcome::Retry;
        }
    };
    for target in &result.numbering_refreshes {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::refresh_book_numbering(
                &target.book_id,
                &target.series_id,
                4,
            ))
            .await
        {
            tracing::error!(%error, book_id = %target.book_id, "could not enqueue numbering metadata refresh");
            return TaskOutcome::Retry;
        }
    }
    tracing::info!(%library_id, ?result, "library trash emptied");
    TaskOutcome::Completed
}

async fn process_analyze_book_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid AnalyzeBook payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => {
            tracing::warn!(book_id = %payload.book_id, "analysis task references a missing book");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for analysis");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load analysis settings");
            return TaskOutcome::Retry;
        }
    };
    let import_metadata = library.import_comic_info_book
        || library.import_comic_info_series
        || library.import_comic_info_collection
        || library.import_comic_info_read_list
        || library.import_epub_book
        || library.import_epub_series;
    let cancellation = state.shutdown.clone();
    let analyzed_book = book.clone();
    let result = tokio::task::spawn_blocking(move || {
        analyze_book(&analyzed_book, library.analyze_dimensions, || {
            cancellation.is_cancelled()
        })
    })
    .await;
    let media = match result {
        Ok(Ok(media)) => media,
        Ok(Err(MediaAnalysisError::Cancelled)) => return TaskOutcome::Cancelled,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "media analyzer panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    if let Err(error) = state.storage.replace_media_analysis(&media).await {
        tracing::error!(%error, book_id = %book.id, "could not persist media analysis");
        return TaskOutcome::Retry;
    }
    if media.status == MediaStatus::Ready
        && let Err(error) = state
            .enqueue_task(&TaskRecord::generate_book_thumbnail(
                &book.id,
                task.priority.saturating_add(1),
            ))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue generated thumbnail");
        return TaskOutcome::Retry;
    }
    if media.status == MediaStatus::Ready
        && import_metadata
        && let Err(error) = state
            .enqueue_task(&TaskRecord::refresh_book_metadata(
                &book.id,
                &book.series_id,
                task.priority,
            ))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue book metadata refresh");
        return TaskOutcome::Retry;
    }
    tracing::info!(book_id = %book.id, status = media.status.as_str(), pages = media.page_count, "book analysis completed");
    TaskOutcome::Completed
}

async fn process_refresh_book_metadata_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid RefreshBookMetadata payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for metadata refresh");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load metadata settings");
            return TaskOutcome::Retry;
        }
    };
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) if media.status == MediaStatus::Ready => media,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load media for metadata refresh");
            return TaskOutcome::Retry;
        }
    };
    let mut metadata = match state.storage.find_book_metadata(&book.id).await {
        Ok(Some(value)) => value,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id=%book.id, "could not load book metadata");
            return TaskOutcome::Retry;
        }
    };
    let extraction_book = book.clone();
    let extraction_media = media.clone();
    let append_volume = library.import_comic_info_series_append_volume;
    let comic_enabled = has_capability(
        &payload.capabilities,
        &[
            MetadataCapability::Title,
            MetadataCapability::Summary,
            MetadataCapability::Number,
            MetadataCapability::NumberSort,
            MetadataCapability::ReleaseDate,
            MetadataCapability::Authors,
            MetadataCapability::Tags,
            MetadataCapability::Isbn,
            MetadataCapability::Links,
            MetadataCapability::ReadLists,
            MetadataCapability::Collections,
        ],
    );
    let comic = if comic_enabled {
        tokio::task::spawn_blocking(move || {
            extract_comic_info_metadata(&extraction_book, &extraction_media, append_volume)
        })
        .await
    } else {
        Ok(Ok(None))
    };
    if let Ok(Ok(Some(value))) = comic {
        if library.import_comic_info_book {
            match metadata.apply_provider_patch(value.book) {
                Ok(v) => metadata = v,
                Err(error) => tracing::warn!(%error, book_id=%book.id, "invalid ComicInfo book metadata"),
            }
        }
        let collections = if library.import_comic_info_collection {
            value.containers.collections
        } else {
            Vec::new()
        };
        let read_lists = if library.import_comic_info_read_list {
            value
                .containers
                .read_lists
                .into_iter()
                .map(|v| (v.name, v.number))
                .collect()
        } else {
            Vec::new()
        };
        if let Err(error) = state
            .storage
            .append_comic_info_containers(&book.series_id, &book.id, &collections, &read_lists)
            .await
        {
            tracing::error!(%error, book_id=%book.id, "could not persist ComicInfo containers");
            return TaskOutcome::Retry;
        }
    } else if let Ok(Err(error)) = comic {
        tracing::warn!(%error, book_id=%book.id, "could not import ComicInfo metadata");
    }
    if library.import_epub_book
        && has_capability(
            &payload.capabilities,
            &[
                MetadataCapability::Title,
                MetadataCapability::Summary,
                MetadataCapability::Number,
                MetadataCapability::NumberSort,
                MetadataCapability::ReleaseDate,
                MetadataCapability::Authors,
                MetadataCapability::Tags,
                MetadataCapability::Isbn,
            ],
        )
    {
        let extraction_book = book.clone();
        let result = tokio::task::spawn_blocking(move || extract_epub_metadata(&extraction_book, &media)).await;
        if let Ok(Ok(Some(value))) = result {
            match metadata.apply_provider_patch(value.book) {
                Ok(v) => metadata = v,
                Err(error) => tracing::warn!(%error, book_id=%book.id, "invalid EPUB metadata"),
            }
        } else if let Ok(Err(error)) = result {
            tracing::warn!(%error, book_id=%book.id, "could not import EPUB metadata");
        }
    }
    if let Err(error) = state.storage.save_book_metadata(&metadata).await {
        tracing::error!(%error, book_id=%book.id, "could not persist book metadata");
        return TaskOutcome::Retry;
    }
    if let Err(error) = state
        .enqueue_task(&TaskRecord::refresh_series_metadata(
            &book.series_id,
            task.priority.saturating_sub(1),
        ))
        .await
    {
        tracing::error!(%error, series_id=%book.series_id, "could not enqueue series metadata refresh");
        return TaskOutcome::Retry;
    }
    TaskOutcome::Completed
}

async fn process_refresh_series_metadata_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<SeriesTaskPayload>(&task.payload) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(%error, "invalid RefreshSeriesMetadata payload");
            return TaskOutcome::Completed;
        }
    };
    let series = match state.storage.find_series_by_id(&payload.series_id).await {
        Ok(Some(v)) if v.deleted_date.is_none() => v,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, "could not load series");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&series.library_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, "could not load library");
            return TaskOutcome::Retry;
        }
    };
    let mut target = match state.storage.find_series_metadata(&series.id).await {
        Ok(Some(v)) => v,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, "could not load series metadata");
            return TaskOutcome::Retry;
        }
    };
    let books = match state.storage.list_books_by_series_id(&series.id).await {
        Ok(v) => v,
        Err(error) => {
            tracing::error!(%error, "could not list series books");
            return TaskOutcome::Retry;
        }
    };
    let mut comic_patches = Vec::new();
    let mut epub_patches = Vec::new();
    for book in books.into_iter().filter(|b| b.deleted_date.is_none()) {
        let media = match state.storage.find_media_by_book_id(&book.id).await {
            Ok(Some(v)) if v.status == MediaStatus::Ready => v,
            Ok(_) => continue,
            Err(error) => {
                tracing::error!(%error, "could not load media");
                return TaskOutcome::Retry;
            }
        };
        if library.import_comic_info_series
            && has_capability(
                &payload.capabilities,
                &[
                    MetadataCapability::Title,
                    MetadataCapability::Summary,
                    MetadataCapability::ReadingDirection,
                    MetadataCapability::Publisher,
                    MetadataCapability::AgeRating,
                    MetadataCapability::Language,
                    MetadataCapability::Genres,
                    MetadataCapability::TotalBookCount,
                    MetadataCapability::Collections,
                ],
            )
        {
            match extract_comic_info_metadata(&book, &media, library.import_comic_info_series_append_volume) {
                Ok(Some(v)) => comic_patches.push(v.series),
                Err(error) => tracing::warn!(%error, book_id=%book.id, "could not import ComicInfo series metadata"),
                _ => {}
            }
        }
        if library.import_epub_series
            && has_capability(
                &payload.capabilities,
                &[
                    MetadataCapability::Title,
                    MetadataCapability::Summary,
                    MetadataCapability::Publisher,
                    MetadataCapability::Language,
                    MetadataCapability::Genres,
                ],
            )
        {
            match extract_epub_metadata(&book, &media) {
                Ok(Some(v)) => epub_patches.push(v.series),
                Err(error) => tracing::warn!(%error, book_id=%book.id, "could not import EPUB series metadata"),
                _ => {}
            }
        }
    }
    if let Some(patch) = aggregate_series_provider_patches(&comic_patches)
        && let Ok(updated) = target.apply_provider_patch(patch)
    {
        target = updated;
    }
    if let Some(patch) = aggregate_series_provider_patches(&epub_patches)
        && let Ok(updated) = target.apply_provider_patch(patch)
    {
        target = updated;
    }
    if library.import_mylar_series
        && has_capability(
            &payload.capabilities,
            &[
                MetadataCapability::Status,
                MetadataCapability::Title,
                MetadataCapability::Summary,
                MetadataCapability::Publisher,
                MetadataCapability::AgeRating,
                MetadataCapability::TotalBookCount,
            ],
        )
    {
        match extract_mylar_metadata(&series) {
            Ok(Some(patch)) => match target.apply_provider_patch(patch) {
                Ok(v) => target = v,
                Err(error) => tracing::warn!(%error, "invalid Mylar metadata"),
            },
            Err(error) => tracing::warn!(%error, series_id=%series.id, "could not import Mylar metadata"),
            _ => {}
        }
    }
    if let Err(error) = state.storage.save_series_metadata(&target).await {
        tracing::error!(%error, "could not persist series metadata");
        return TaskOutcome::Retry;
    }
    if let Err(error) = state
        .enqueue_task(&TaskRecord::aggregate_series_metadata(
            &series.id,
            task.priority.saturating_sub(1),
        ))
        .await
    {
        tracing::error!(%error, "could not enqueue series aggregation");
        return TaskOutcome::Retry;
    }
    TaskOutcome::Completed
}

fn aggregate_series_provider_patches(patches: &[SeriesMetadataPatch]) -> Option<SeriesMetadataPatch> {
    if patches.is_empty() {
        return None;
    }
    let mut genres = std::collections::BTreeSet::new();
    let mut ages = Vec::new();
    let mut totals = Vec::new();
    for patch in patches {
        if let MetadataUpdate::Set(values) = &patch.genres {
            genres.extend(values.iter().cloned());
        }
        if let MetadataUpdate::Set(value) = patch.age_rating {
            ages.push(value);
        }
        if let MetadataUpdate::Set(value) = patch.total_book_count {
            totals.push(value);
        }
    }
    Some(SeriesMetadataPatch {
        status: mode(patches.iter().filter_map(|p| p.status)),
        title: mode(patches.iter().filter_map(|p| p.title.clone())),
        summary: mode(patches.iter().filter_map(|p| p.summary.clone())),
        reading_direction: mode(patches.iter().filter_map(|p| match p.reading_direction {
            MetadataUpdate::Set(v) => Some(v),
            _ => None,
        }))
        .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        publisher: mode(patches.iter().filter_map(|p| p.publisher.clone())),
        age_rating: ages.into_iter().max().map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        language: mode(patches.iter().filter_map(|p| p.language.clone())),
        genres: if genres.is_empty() {
            MetadataUpdate::Keep
        } else {
            MetadataUpdate::Set(genres)
        },
        total_book_count: totals
            .into_iter()
            .max()
            .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        ..Default::default()
    })
}

fn mode<T: Clone + PartialEq>(values: impl Iterator<Item = T>) -> Option<T> {
    let values = values.collect::<Vec<_>>();
    let mut best = None;
    let mut best_count = 0;
    for value in &values {
        let count = values.iter().filter(|candidate| *candidate == value).count();
        if count > best_count {
            best = Some(value.clone());
            best_count = count;
        }
    }
    best
}

async fn process_generate_thumbnail_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid GenerateBookThumbnail payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for thumbnail generation");
            return TaskOutcome::Retry;
        }
    };
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) => media,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load media for thumbnail generation");
            return TaskOutcome::Retry;
        }
    };
    let max_edge = match state.storage.thumbnail_max_edge().await {
        Ok(max_edge) => max_edge,
        Err(error) => {
            tracing::error!(%error, "could not read thumbnail size");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    let thumbnail_book = book.clone();
    let generated = tokio::task::spawn_blocking(move || generate_thumbnail(&thumbnail_book, &media, max_edge)).await;
    let generated = match generated {
        Ok(Ok(generated)) => generated,
        Ok(Err(MediaOperationError::NotReady | MediaOperationError::PageOutOfBounds)) => return TaskOutcome::Completed,
        Ok(Err(error)) => {
            tracing::error!(%error, book_id = %book.id, "could not generate book thumbnail");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "thumbnail generator panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    let Some(dimension) = image_dimension(&generated.bytes) else {
        tracing::error!(book_id = %book.id, "generated thumbnail has no readable dimensions");
        return TaskOutcome::Completed;
    };
    let thumbnail = ThumbnailBook {
        id: new_id(),
        file_size: i64::try_from(generated.bytes.len()).unwrap_or(i64::MAX),
        thumbnail: Some(generated.bytes),
        url: None,
        selected: false,
        thumbnail_type: "GENERATED".to_owned(),
        media_type: generated.media_type,
        dimension,
        book_id: book.id.clone(),
    };
    if let Err(error) = state.storage.replace_generated_thumbnail(&thumbnail).await {
        tracing::error!(%error, book_id = %book.id, "could not persist generated thumbnail");
        return TaskOutcome::Retry;
    }
    TaskOutcome::Completed
}

async fn process_aggregate_series_metadata_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<SeriesTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid AggregateSeriesMetadata payload");
            return TaskOutcome::Completed;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .rebuild_book_metadata_aggregation(&payload.series_id)
        .await
    {
        Ok(true) => TaskOutcome::Completed,
        Ok(false) => {
            tracing::warn!(series_id = %payload.series_id, "aggregation task references a missing series");
            TaskOutcome::Completed
        }
        Err(error) => {
            tracing::error!(%error, series_id = %payload.series_id, "could not aggregate series metadata");
            TaskOutcome::Retry
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Schedule {
    interval: ScanInterval,
    next: Instant,
}

async fn scheduler_loop(state: AppState) {
    let mut schedules = HashMap::<String, Schedule>::new();
    match state.storage.list_libraries().await {
        Ok(libraries) => {
            for library in &libraries {
                if library.scan_on_startup
                    && let Err(error) = state
                        .enqueue_task(&TaskRecord::scan_library(&library.id, false, 4))
                        .await
                {
                    tracing::error!(%error, library_id = %library.id, "could not enqueue startup scan");
                }
            }
            reconcile_schedules(&mut schedules, &libraries);
        }
        Err(error) => tracing::error!(%error, "could not initialize library schedules"),
    }

    loop {
        let next = schedules.values().map(|schedule| schedule.next).min();
        let sleep = async {
            if let Some(next) = next {
                tokio::time::sleep_until(next).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(sleep);
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            () = state.schedule_notify.notified() => {
                match state.storage.list_libraries().await {
                    Ok(libraries) => reconcile_schedules(&mut schedules, &libraries),
                    Err(error) => tracing::error!(%error, "could not refresh library schedules"),
                }
            }
            () = &mut sleep => {
                let now = Instant::now();
                let due = schedules
                    .iter()
                    .filter(|(_, schedule)| schedule.next <= now)
                    .map(|(library_id, _)| library_id.clone())
                    .collect::<Vec<_>>();
                for library_id in due {
                    if let Err(error) = state
                        .enqueue_task(&TaskRecord::scan_library(&library_id, false, 4))
                        .await
                    {
                        tracing::error!(%error, %library_id, "could not enqueue periodic scan");
                    }
                    if let Some(schedule) = schedules.get_mut(&library_id) {
                        schedule.next = now + scan_interval_duration(schedule.interval);
                    }
                }
            }
        }
    }
}

fn reconcile_schedules(schedules: &mut HashMap<String, Schedule>, libraries: &[komga_core::Library]) {
    let now = Instant::now();
    schedules.retain(|library_id, _| libraries.iter().any(|library| library.id == *library_id));
    for library in libraries {
        if library.scan_interval == ScanInterval::Disabled {
            schedules.remove(&library.id);
            continue;
        }
        match schedules.get(&library.id) {
            Some(schedule) if schedule.interval == library.scan_interval => {}
            _ => {
                schedules.insert(
                    library.id.clone(),
                    Schedule {
                        interval: library.scan_interval,
                        next: now + scan_interval_duration(library.scan_interval),
                    },
                );
            }
        }
    }
}

fn scan_interval_duration(interval: ScanInterval) -> Duration {
    match interval {
        ScanInterval::Disabled => Duration::MAX,
        ScanInterval::Hourly => Duration::from_secs(60 * 60),
        ScanInterval::Every6h => Duration::from_secs(6 * 60 * 60),
        ScanInterval::Every12h => Duration::from_secs(12 * 60 * 60),
        ScanInterval::Daily => Duration::from_secs(24 * 60 * 60),
        ScanInterval::Weekly => Duration::from_secs(7 * 24 * 60 * 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_intervals_match_kotlin_durations() {
        assert_eq!(scan_interval_duration(ScanInterval::Hourly), Duration::from_secs(3600));
        assert_eq!(
            scan_interval_duration(ScanInterval::Every6h),
            Duration::from_secs(21600)
        );
        assert_eq!(
            scan_interval_duration(ScanInterval::Every12h),
            Duration::from_secs(43200)
        );
        assert_eq!(scan_interval_duration(ScanInterval::Daily), Duration::from_secs(86400));
        assert_eq!(
            scan_interval_duration(ScanInterval::Weekly),
            Duration::from_secs(604800)
        );
    }
}
