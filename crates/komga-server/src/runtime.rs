//! Background task processing and periodic library scan scheduling.

use std::time::Duration;

use komga_core::{
    BookEvent, BookMetadataPatch, BookThumbnailEvent, ContainerEvent, DomainEvent, HashedPageTarget, LibraryEvent,
    MediaStatus, MetadataUpdate, ScanError, SeriesEvent, SeriesMetadataPatch, ThumbnailBook, new_id,
    oneshot_series_metadata_patch, scan_library_files,
};
use komga_media::{
    FileHashError, KepubSpanError, MediaAnalysisError, MediaOperationError, analyze_book, enrich_kobo_spans,
    extract_comic_info_metadata, extract_epub_metadata, extract_isbn_barcode, extract_mylar_metadata,
    generate_thumbnail, hash_file, image_dimension, reconcile_scan_hashes,
};
use komga_search::EntityKind;
use komga_storage::{RebuildIndexEntity, StorageError, TaskRecord};
use serde::Deserialize;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::AppState;

mod analysis;
mod conversion;
mod duplicate_pages;
mod hashing;
mod koreader_hashing;
mod lifecycle;
mod local_artwork;
mod maintenance;
mod metadata;
mod page_hashing;
mod scan;
mod scheduler;
mod source_deletion;
mod source_import;
mod thumbnail;

use analysis::process_analyze_book_task;
pub(crate) use conversion::enqueue_extension_repairs;
use conversion::{process_convert_book_task, process_find_books_to_convert_task, process_repair_extension_task};
use duplicate_pages::{process_find_duplicate_pages_task, process_remove_hashed_pages_task};
use hashing::process_hash_book_task;
use koreader_hashing::process_hash_book_koreader_task;
use lifecycle::{perform_empty_trash, process_empty_trash_task};
use local_artwork::{process_refresh_book_local_artwork_task, process_refresh_series_local_artwork_task};
use maintenance::{process_aggregate_series_metadata_task, process_find_thumbnails_task};
use metadata::{process_refresh_book_metadata_task, process_refresh_series_metadata_task};
use page_hashing::{process_find_missing_page_hashes_task, process_hash_book_pages_task};
use scan::process_scan_task;
use scheduler::scheduler_loop;
use source_deletion::{process_delete_book_task, process_delete_series_task};
use source_import::process_import_book_task;
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
    /// Historical reflowable EPUBs without Kobo spans are submitted at the
    /// lowest priority before workers begin claiming tasks.
    pub async fn start_runtime(&self) -> Result<AppRuntime, StorageError> {
        for target in self.storage.list_epub_kobo_span_backfill_targets().await? {
            self.tasks
                .save_if_absent(&TaskRecord::analyze_book(&target.book_id, &target.series_id, 0))
                .await?;
        }
        let handles = vec![
            tokio::spawn(worker_supervisor(self.clone(), self.task_pool_size.subscribe())),
            tokio::spawn(scheduler_loop(self.clone())),
            tokio::spawn(authentication_activity_cleanup_loop(self.clone())),
        ];
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

async fn authentication_activity_cleanup_loop(state: AppState) {
    loop {
        let cutoff = chrono::Utc::now()
            .naive_utc()
            .checked_sub_months(chrono::Months::new(1))
            .expect("subtracting one month from the current date is valid");
        match state.storage.delete_authentication_activity_older_than(cutoff).await {
            Ok(deleted) => tracing::debug!(deleted, %cutoff, "authentication activity cleanup completed"),
            Err(error) => tracing::error!(%error, %cutoff, "authentication activity cleanup failed"),
        }
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(24 * 60 * 60)) => {}
        }
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

const SUPPORTED_TASK_TYPES: [&str; 24] = [
    "ScanLibrary",
    "FindBooksToConvert",
    "ConvertBook",
    "RepairExtension",
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
    "DeleteBook",
    "DeleteSeries",
    "ImportBook",
    "RefreshBookLocalArtwork",
    "RefreshSeriesLocalArtwork",
];

struct WorkerHandle {
    owner: String,
    stop: CancellationToken,
    handle: JoinHandle<()>,
}

async fn worker_supervisor(state: AppState, mut desired: tokio::sync::watch::Receiver<usize>) {
    let mut workers = Vec::new();
    let mut retiring = Vec::new();
    let mut next_index = 0_usize;
    resize_workers(
        &state,
        *desired.borrow_and_update(),
        &mut workers,
        &mut retiring,
        &mut next_index,
    );
    loop {
        tokio::select! {
            biased;
            () = state.shutdown.cancelled() => break,
            changed = desired.changed() => {
                if changed.is_err() {
                    break;
                }
                resize_workers(
                    &state,
                    *desired.borrow_and_update(),
                    &mut workers,
                    &mut retiring,
                    &mut next_index,
                );
                collect_finished_workers(&state, &mut retiring).await;
            }
            (index, result) = wait_for_worker_exit(&mut workers) => {
                let desired_count = *desired.borrow();
                handle_worker_exit(
                    &state,
                    desired_count,
                    &mut workers,
                    &mut retiring,
                    &mut next_index,
                    index,
                    result,
                ).await;
            }
        }
    }
    for worker in workers.iter().chain(&retiring) {
        worker.stop.cancel();
    }
    state.task_notify.notify_waiters();
    workers.append(&mut retiring);
    for worker in workers {
        if let Err(error) = worker.handle.await {
            tracing::error!(%error, "task worker terminated unexpectedly");
        }
    }
}

fn resize_workers(
    state: &AppState,
    desired: usize,
    workers: &mut Vec<WorkerHandle>,
    retiring: &mut Vec<WorkerHandle>,
    next_index: &mut usize,
) {
    while workers.len() < desired {
        let stop = CancellationToken::new();
        let owner = format!("komga-rs-{}-{}", std::process::id(), *next_index);
        workers.push(WorkerHandle {
            owner: owner.clone(),
            stop: stop.clone(),
            handle: tokio::spawn(worker_loop(state.clone(), owner, stop)),
        });
        *next_index = next_index.wrapping_add(1);
    }
    while workers.len() > desired {
        if let Some(worker) = workers.pop() {
            worker.stop.cancel();
            retiring.push(worker);
        }
    }
    state.task_notify.notify_waiters();
}

async fn wait_for_worker_exit(workers: &mut [WorkerHandle]) -> (usize, Result<(), tokio::task::JoinError>) {
    if workers.is_empty() {
        std::future::pending().await
    }
    let (result, index, _) =
        futures_util::future::select_all(workers.iter_mut().map(|worker| &mut worker.handle)).await;
    (index, result)
}

async fn handle_worker_exit(
    state: &AppState,
    desired: usize,
    workers: &mut Vec<WorkerHandle>,
    retiring: &mut Vec<WorkerHandle>,
    next_index: &mut usize,
    index: usize,
    result: Result<(), tokio::task::JoinError>,
) {
    let worker = workers.swap_remove(index);
    match result {
        Ok(()) => tracing::warn!(worker = %worker.owner, "task worker exited unexpectedly"),
        Err(error) => tracing::error!(%error, worker = %worker.owner, "task worker terminated unexpectedly"),
    }
    release_worker_tasks(state, &worker.owner).await;
    collect_finished_workers(state, retiring).await;
    resize_workers(state, desired, workers, retiring, next_index);
}

async fn collect_finished_workers(state: &AppState, workers: &mut Vec<WorkerHandle>) {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].handle.is_finished() {
            let worker = workers.swap_remove(index);
            if let Err(error) = worker.handle.await {
                tracing::error!(%error, worker = %worker.owner, "retired task worker terminated unexpectedly");
                release_worker_tasks(state, &worker.owner).await;
            }
        } else {
            index += 1;
        }
    }
}

async fn release_worker_tasks(state: &AppState, owner: &str) {
    match state.tasks.disown(owner).await {
        Ok(released) if released > 0 => tracing::warn!(worker = owner, released, "released tasks from exited worker"),
        Ok(_) => {}
        Err(error) => tracing::error!(%error, worker = owner, "could not release tasks from exited worker"),
    }
}

async fn worker_loop(state: AppState, owner: String, stop: CancellationToken) {
    loop {
        let notified = state.task_notify.notified();
        tokio::pin!(notified);
        if state.shutdown.is_cancelled() || stop.is_cancelled() {
            break;
        }
        match state.tasks.take_first_of_types(&owner, &SUPPORTED_TASK_TYPES).await {
            Ok(Some(task)) => {
                let started = std::time::Instant::now();
                let outcome = process_task(&state, &task).await;
                record_task_metrics(&state.task_metrics, &task.simple_type, outcome, started.elapsed());
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
                        () = stop.cancelled() => break,
                        () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
            }
            Ok(None) => {
                tokio::select! {
                    () = state.shutdown.cancelled() => break,
                    () = stop.cancelled() => break,
                    () = &mut notified => {}
                }
            }
            Err(error) => {
                tracing::error!(%error, worker = %owner, "could not claim task");
                tokio::select! {
                    () = state.shutdown.cancelled() => break,
                    () = stop.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
    }
}

fn record_task_metrics(
    metrics: &crate::metrics_routes::TaskMetrics,
    task_type: &str,
    outcome: TaskOutcome,
    elapsed: Duration,
) {
    match outcome {
        TaskOutcome::Completed => metrics.record_execution(task_type, elapsed),
        TaskOutcome::Retry => metrics.record_failure(task_type),
        TaskOutcome::Cancelled => {}
    }
}

async fn process_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    match task.simple_type.as_str() {
        "ScanLibrary" => process_scan_task(state, task).await,
        "FindBooksToConvert" => process_find_books_to_convert_task(state, task).await,
        "ConvertBook" => process_convert_book_task(state, task).await,
        "RepairExtension" => process_repair_extension_task(state, task).await,
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
        "DeleteBook" => process_delete_book_task(state, task).await,
        "DeleteSeries" => process_delete_series_task(state, task).await,
        "ImportBook" => process_import_book_task(state, task).await,
        "RefreshBookLocalArtwork" => process_refresh_book_local_artwork_task(state, task).await,
        "RefreshSeriesLocalArtwork" => process_refresh_series_local_artwork_task(state, task).await,
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

    #[test]
    fn task_metrics_record_success_failure_and_ignore_cancellation() {
        let metrics = crate::metrics_routes::TaskMetrics::default();
        record_task_metrics(&metrics, "Completed", TaskOutcome::Completed, Duration::from_secs(1));
        record_task_metrics(&metrics, "Retry", TaskOutcome::Retry, Duration::from_secs(1));
        record_task_metrics(&metrics, "Cancelled", TaskOutcome::Cancelled, Duration::from_secs(1));
        assert_eq!(metrics.counts("Completed"), (1, 0));
        assert_eq!(metrics.counts("Retry"), (0, 1));
        assert_eq!(metrics.counts("Cancelled"), (0, 0));
    }

    #[tokio::test]
    async fn task_workers_resize_up_and_retire_without_cancelling_active_work() {
        let state = AppState::new(
            Storage::connect_memory().await.unwrap(),
            TaskStorage::connect_memory().await.unwrap(),
        )
        .await
        .unwrap();
        let mut workers = Vec::new();
        let mut retiring = Vec::new();
        let mut next_index = 0;
        resize_workers(&state, 3, &mut workers, &mut retiring, &mut next_index);
        assert_eq!(workers.len(), 3);
        assert!(retiring.is_empty());
        assert_eq!(next_index, 3);

        resize_workers(&state, 1, &mut workers, &mut retiring, &mut next_index);
        assert_eq!(workers.len(), 1);
        assert_eq!(retiring.len(), 2);
        assert!(retiring.iter().all(|worker| worker.stop.is_cancelled()));

        state.shutdown.cancel();
        for worker in &workers {
            worker.stop.cancel();
        }
        state.task_notify.notify_waiters();
        workers.append(&mut retiring);
        for worker in workers {
            worker.handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn task_workers_release_claims_and_replace_unexpected_exits() {
        let state = AppState::new(
            Storage::connect_memory().await.unwrap(),
            TaskStorage::connect_memory().await.unwrap(),
        )
        .await
        .unwrap();
        let task = TaskRecord::scan_library("library", false, 1);
        state.tasks.save(&task).await.unwrap();
        let owner = format!("komga-rs-{}-0", std::process::id());
        assert!(state.tasks.take_first(&owner).await.unwrap().is_some());
        let stop = CancellationToken::new();
        let mut workers = vec![WorkerHandle {
            owner: owner.clone(),
            stop,
            handle: tokio::spawn(async { panic!("simulated worker failure") }),
        }];
        let mut retiring = Vec::new();
        let mut next_index = 1;
        let (index, result) = wait_for_worker_exit(&mut workers).await;

        handle_worker_exit(&state, 0, &mut workers, &mut retiring, &mut next_index, index, result).await;
        assert!(workers.is_empty());
        assert_eq!(state.tasks.list().await.unwrap()[0].owner, None);

        resize_workers(&state, 1, &mut workers, &mut retiring, &mut next_index);
        assert_eq!(workers.len(), 1);
        assert_ne!(workers[0].owner, owner);
        state.shutdown.cancel();
        state.task_notify.notify_waiters();
        workers.pop().unwrap().handle.await.unwrap();
    }

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
