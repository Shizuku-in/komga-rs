//! Axum composition, authentication, and process configuration for Komga.
//!
//! [`build_app`] assembles the compatibility routes around an [`AppState`].
//! Domain policy and SQLite access remain in `komga-core` and `komga-storage`
//! respectively, which keeps HTTP concerns out of those crates.

#![warn(missing_docs)]

mod app_router;
mod auth;
mod book_file_routes;
/// Command-line and environment configuration.
pub mod config;
mod container_routes;
mod content_access;
mod content_routes;
mod dto;
mod error;
mod filesystem_routes;
mod history_routes;
mod kobo;
mod koreader_routes;
mod legacy_content_query;
mod library_routes;
mod media_routes;
mod metadata_routes;
mod opds_router;
mod opds_v1;
mod opds_v2;
mod page_hash_routes;
mod pagination;
mod read_list_file_routes;
mod read_list_match_routes;
mod read_progress_routes;
mod referential_routes;
mod routes;
mod runtime;
mod search_query;
mod search_sync;
mod settings_routes;
mod sse_routes;
mod thumbnail_routes;
mod webpub_routes;

pub use auth::{REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER};
pub use runtime::{AppRuntime, ShutdownHandle};

use std::{num::NonZeroUsize, path::PathBuf, sync::Arc};

use auth::SessionStore;
use axum::Router;
use komga_core::DomainEvent;
use komga_search::{IndexState, SearchIndex};
use komga_storage::{Storage, StorageError, TaskRecord, TaskStorage};
use tokio::sync::{Mutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

/// Shared dependencies used by the HTTP router.
///
/// Construct this state with [`AppState::new`] and pass it to [`build_app`].
/// Session state is intentionally process-local, matching the current Kotlin
/// Caffeine session repository: restarting the process expires active sessions.
#[derive(Clone)]
pub struct AppState {
    storage: Storage,
    search: SearchIndex,
    tasks: TaskStorage,
    sessions: SessionStore,
    claim_lock: Arc<Mutex<()>>,
    task_notify: Arc<Notify>,
    schedule_notify: Arc<Notify>,
    domain_events: broadcast::Sender<Arc<DomainEvent>>,
    shutdown: CancellationToken,
    page_hashing: NonZeroUsize,
    kobo_sync_item_limit: NonZeroUsize,
    kobo_kepub: kobo::kepub::KepubService,
    kobo_proxy: kobo::proxy::KoboProxy,
}

impl AppState {
    /// Creates application state around initialized main and task databases.
    ///
    /// # Errors
    ///
    /// Returns a storage error when server settings cannot be loaded.
    pub async fn new(storage: Storage, tasks: TaskStorage) -> Result<Self, StorageError> {
        let search = SearchIndex::in_memory().expect("in-memory search index creation must succeed");
        Self::with_search(storage, tasks, search).await
    }

    /// Creates application state with an explicitly configured index and
    /// synchronously rebuilds it for deterministic embedding and tests.
    pub async fn with_search(storage: Storage, tasks: TaskStorage, search: SearchIndex) -> Result<Self, StorageError> {
        let state = Self::build(storage, tasks, search).await?;
        state.rebuild_search(&search_sync::all_entity_kinds()).await;
        Ok(state)
    }

    /// Creates application state around a file index without blocking startup.
    ///
    /// Empty or incompatible indexes enqueue one deduplicated priority-8 full
    /// rebuild task. A current index is immediately available to requests.
    pub async fn with_file_search(
        storage: Storage,
        tasks: TaskStorage,
        search: SearchIndex,
    ) -> Result<Self, StorageError> {
        let needs_rebuild = search.state() != IndexState::Current;
        let state = Self::build(storage, tasks, search).await?;
        if needs_rebuild {
            state.enqueue_task(&TaskRecord::rebuild_index(None, 8)).await?;
        }
        Ok(state)
    }

    async fn build(storage: Storage, tasks: TaskStorage, search: SearchIndex) -> Result<Self, StorageError> {
        let remember_me_duration_days = storage.remember_me_duration_days().await?;
        let (domain_events, _) = broadcast::channel(1_024);
        Ok(Self {
            storage,
            search,
            tasks,
            sessions: SessionStore::new(remember_me_duration_days),
            claim_lock: Arc::new(Mutex::new(())),
            task_notify: Arc::new(Notify::new()),
            schedule_notify: Arc::new(Notify::new()),
            domain_events,
            shutdown: CancellationToken::new(),
            page_hashing: NonZeroUsize::new(3).expect("page hashing default is nonzero"),
            kobo_sync_item_limit: NonZeroUsize::new(100).expect("Kobo sync default is nonzero"),
            kobo_kepub: kobo::kepub::KepubService::disabled(),
            kobo_proxy: kobo::proxy::KoboProxy::new(false),
        })
    }

    /// Overrides the number of pages hashed at each edge of ZIP books.
    #[must_use]
    pub fn with_page_hashing(mut self, page_hashing: NonZeroUsize) -> Self {
        self.page_hashing = page_hashing;
        self
    }

    /// Overrides the maximum number of source items claimed by one Kobo sync response.
    #[must_use]
    pub fn with_kobo_sync_item_limit(mut self, limit: NonZeroUsize) -> Self {
        self.kobo_sync_item_limit = limit;
        self
    }

    /// Configures the optional external kepubify process used by Kobo clients.
    ///
    /// An invalid or unavailable executable leaves conversion disabled and does
    /// not prevent the application from starting.
    #[must_use]
    pub async fn with_kobo_kepubify_path(mut self, path: Option<PathBuf>) -> Self {
        self.kobo_kepub = kobo::kepub::KepubService::configure(path).await;
        self
    }

    /// Enables or disables forwarding unsupported Kobo requests to the official store.
    #[must_use]
    pub fn with_kobo_proxy(mut self, enabled: bool) -> Self {
        self.kobo_proxy = kobo::proxy::KoboProxy::new(enabled);
        self
    }

    pub(crate) async fn enqueue_task(&self, task: &TaskRecord) -> Result<(), StorageError> {
        self.tasks.save(task).await?;
        self.task_notify.notify_waiters();
        Ok(())
    }

    pub(crate) async fn enqueue_missing_book_hashes(&self, library_id: &str) -> Result<(), StorageError> {
        for book_id in self.storage.list_books_without_file_hash(library_id).await? {
            self.enqueue_task(&TaskRecord::hash_book(&book_id, 0)).await?;
        }
        Ok(())
    }

    pub(crate) async fn enqueue_missing_koreader_hashes(&self, library_id: &str) -> Result<(), StorageError> {
        for book_id in self.storage.list_books_without_koreader_hash(library_id).await? {
            self.enqueue_task(&TaskRecord::hash_book_koreader(&book_id, 0)).await?;
        }
        Ok(())
    }

    pub(crate) fn library_schedule_changed(&self) {
        self.schedule_notify.notify_one();
    }

    /// Publishes a best-effort notification after a durable domain mutation.
    ///
    /// Events are discarded when no live subscriber is connected.
    pub fn publish_domain_event(&self, event: DomainEvent) {
        let _ = self.domain_events.send(Arc::new(event));
    }

    pub(crate) fn subscribe_domain_events(&self) -> broadcast::Receiver<Arc<DomainEvent>> {
        self.domain_events.subscribe()
    }
}

/// Builds the Komga HTTP router and its middleware stack.
///
/// The returned router includes public health and claim endpoints, authenticated
/// user/session endpoints, CORS handling, request IDs, a 2 MiB JSON body limit,
/// and request tracing. It can be served directly with `axum::serve` or cloned
/// for in-process contract tests.
pub fn build_app(state: AppState) -> Router {
    app_router::build(state)
}
