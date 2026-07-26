//! Background task processing and periodic library scan scheduling.

use std::time::Duration;

use komga_core::{
    BookEvent, BookThumbnailEvent, ContainerEvent, DomainEvent, HashedPageTarget, LibraryEvent, MediaStatus,
    MetadataUpdate, ScanError, SeriesEvent, SeriesMetadataPatch, ThumbnailBook, new_id, scan_library_files,
};
use komga_media::{
    FileHashError, KepubSpanError, MediaAnalysisError, MediaOperationError, analyze_book, enrich_kobo_spans,
    extract_comic_info_metadata, extract_epub_metadata, extract_mylar_metadata, generate_thumbnail, hash_file,
    image_dimension, reconcile_scan_hashes,
};
use komga_search::EntityKind;
use komga_storage::{RebuildIndexEntity, StorageError, TaskRecord};
use serde::Deserialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::AppState;

mod analysis;
mod duplicate_pages;
mod hashing;
mod koreader_hashing;
mod lifecycle;
mod maintenance;
mod metadata;
mod page_hashing;
mod scan;
mod scheduler;
mod thumbnail;

use analysis::process_analyze_book_task;
use duplicate_pages::{process_find_duplicate_pages_task, process_remove_hashed_pages_task};
use hashing::process_hash_book_task;
use koreader_hashing::process_hash_book_koreader_task;
use lifecycle::{perform_empty_trash, process_empty_trash_task};
use maintenance::{process_aggregate_series_metadata_task, process_find_thumbnails_task};
use metadata::{process_refresh_book_metadata_task, process_refresh_series_metadata_task};
use page_hashing::{process_find_missing_page_hashes_task, process_hash_book_pages_task};
use scan::process_scan_task;
use scheduler::scheduler_loop;
use thumbnail::process_generate_thumbnail_task;

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
pub(super) struct ScanLibraryPayload {
    pub(super) library_id: String,
    pub(super) scan_deep: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LibraryTaskPayload {
    pub(super) library_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BookTaskPayload {
    pub(super) book_id: String,
    #[serde(default)]
    pub(super) capabilities: Vec<MetadataCapability>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveHashedPagesPayload {
    book_id: String,
    pages: Vec<HashedPageTarget>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SeriesTaskPayload {
    pub(super) series_id: String,
    #[serde(default)]
    pub(super) capabilities: Vec<MetadataCapability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum MetadataCapability {
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

pub(super) fn has_capability(requested: &[MetadataCapability], supported: &[MetadataCapability]) -> bool {
    requested.is_empty() || requested.iter().any(|value| supported.contains(value))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FindThumbnailsPayload {
    for_bigger_result_only: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RebuildIndexPayload {
    entities: Option<std::collections::BTreeSet<RebuildIndexEntity>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskOutcome {
    Completed,
    Cancelled,
    Retry,
}

const SUPPORTED_TASK_TYPES: [&str; 16] = [
    "ScanLibrary",
    "EmptyTrash",
    "AnalyzeBook",
    "HashBook",
    "HashBookKoreader",
    "FindBooksWithMissingPageHash",
    "HashBookPages",
    "FindDuplicatePagesToDelete",
    "RemoveHashedPages",
    "RefreshBookMetadata",
    "RefreshSeriesMetadata",
    "GenerateBookThumbnail",
    "AggregateSeriesMetadata",
    "FindBookThumbnailsToRegenerate",
    "RebuildIndex",
    "UpgradeIndex",
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
        "HashBook" => process_hash_book_task(state, task).await,
        "HashBookKoreader" => process_hash_book_koreader_task(state, task).await,
        "FindBooksWithMissingPageHash" => process_find_missing_page_hashes_task(state, task).await,
        "HashBookPages" => process_hash_book_pages_task(state, task).await,
        "FindDuplicatePagesToDelete" => process_find_duplicate_pages_task(state, task).await,
        "RemoveHashedPages" => process_remove_hashed_pages_task(state, task).await,
        "RefreshBookMetadata" => process_refresh_book_metadata_task(state, task).await,
        "RefreshSeriesMetadata" => process_refresh_series_metadata_task(state, task).await,
        "GenerateBookThumbnail" => process_generate_thumbnail_task(state, task).await,
        "AggregateSeriesMetadata" => process_aggregate_series_metadata_task(state, task).await,
        "FindBookThumbnailsToRegenerate" => process_find_thumbnails_task(state, task).await,
        "RebuildIndex" => {
            let payload = match serde_json::from_str::<RebuildIndexPayload>(&task.payload) {
                Ok(payload) => payload,
                Err(error) => {
                    tracing::error!(%error, task_id = %task.id, "invalid RebuildIndex payload");
                    return TaskOutcome::Completed;
                }
            };
            let kinds = payload
                .entities
                .map(|entities| entities.into_iter().map(search_kind).collect::<Vec<_>>())
                .unwrap_or_else(|| all_search_kinds().to_vec());
            if state.rebuild_search(&kinds).await {
                TaskOutcome::Completed
            } else {
                TaskOutcome::Retry
            }
        }
        "UpgradeIndex" => {
            if state.rebuild_search(&all_search_kinds()).await {
                TaskOutcome::Completed
            } else {
                TaskOutcome::Retry
            }
        }
        _ => TaskOutcome::Completed,
    }
}

fn search_kind(entity: RebuildIndexEntity) -> EntityKind {
    match entity {
        RebuildIndexEntity::Book => EntityKind::Book,
        RebuildIndexEntity::Series => EntityKind::Series,
        RebuildIndexEntity::Collection => EntityKind::Collection,
        RebuildIndexEntity::ReadList => EntityKind::ReadList,
    }
}

fn all_search_kinds() -> [EntityKind; 4] {
    [
        EntityKind::Book,
        EntityKind::Series,
        EntityKind::Collection,
        EntityKind::ReadList,
    ]
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use komga_core::{Library, scan_library_files};
    use komga_search::SearchMutation;
    use komga_storage::{Storage, TaskStorage};
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn rebuild_index_honors_subsets_and_upgrade_index_rebuilds_everything() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Volume.cbz"), [1_u8]).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        let state = AppState::new(storage, tasks).await.unwrap();

        state
            .search
            .mutate([
                SearchMutation::Delete {
                    kind: EntityKind::Book,
                    id: book.id.clone(),
                },
                SearchMutation::Delete {
                    kind: EntityKind::Series,
                    id: series.id.clone(),
                },
            ])
            .unwrap();
        let subset = TaskRecord::rebuild_index(Some(&BTreeSet::from([RebuildIndexEntity::Book])), 8);
        assert_eq!(process_task(&state, &subset).await, TaskOutcome::Completed);
        assert_eq!(state.search.search(EntityKind::Book, "volume")[0].id, book.id);
        assert!(state.search.search(EntityKind::Series, "series").is_empty());

        state
            .search
            .mutate([
                SearchMutation::Delete {
                    kind: EntityKind::Book,
                    id: book.id.clone(),
                },
                SearchMutation::Delete {
                    kind: EntityKind::Series,
                    id: series.id.clone(),
                },
            ])
            .unwrap();
        let upgrade = TaskRecord {
            id: "UPGRADE_INDEX".into(),
            priority: 8,
            group_id: None,
            class: "org.gotson.komga.application.tasks.Task$UpgradeIndex".into(),
            simple_type: "UpgradeIndex".into(),
            payload: "{}".into(),
            owner: None,
        };
        assert_eq!(process_task(&state, &upgrade).await, TaskOutcome::Completed);
        assert_eq!(state.search.search(EntityKind::Book, "volume")[0].id, book.id);
        assert_eq!(state.search.search(EntityKind::Series, "series")[0].id, series.id);
    }
}
