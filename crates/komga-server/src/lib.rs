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
mod metrics_routes;
mod opds_router;
mod opds_v1;
mod opds_v2;
mod operations_routes;
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
mod source_file_routes;
mod source_import_routes;
mod sse_routes;
mod thumbnail_routes;
mod webpub_routes;

pub use auth::{REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER};
pub use runtime::{AppRuntime, ShutdownHandle};

use std::{
    num::{NonZeroU16, NonZeroUsize},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
};

use auth::SessionStore;
use axum::Router;
use komga_core::DomainEvent;
use komga_search::{IndexState, SearchIndex};
use komga_storage::{ServerSettings, Storage, StorageError, TaskRecord, TaskStorage};
use tokio::sync::{Mutex, Notify, broadcast, watch};
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
    task_pool_size: watch::Sender<usize>,
    domain_events: broadcast::Sender<Arc<DomainEvent>>,
    shutdown: CancellationToken,
    page_hashing: NonZeroUsize,
    kobo_sync_item_limit: NonZeroUsize,
    kobo_kepub: kobo::kepub::KepubService,
    kobo_proxy: kobo::proxy::KoboProxy,
    kobo_port: Arc<AtomicU16>,
    task_metrics: metrics_routes::TaskMetrics,
    operations: operations_routes::OperationsService,
    database_kobo_proxy: Option<bool>,
    database_kepubify_path: Option<PathBuf>,
    configuration_server_port: u16,
    configuration_server_context_path: Option<String>,
    effective_server_port: Option<u16>,
    effective_server_context_path: String,
    configuration_kobo_proxy: bool,
    configuration_kepubify_path: Option<PathBuf>,
}

/// Configuration-source and effective HTTP binding values exposed by the
/// Kotlin-compatible server-settings API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerBindSettings {
    /// Port supplied by CLI or environment configuration.
    pub configuration_port: u16,
    /// Optional context path supplied by CLI or environment configuration.
    pub configuration_context_path: Option<String>,
    /// Port actually bound by the current process.
    pub effective_port: Option<u16>,
    /// Context path used by the current process, or an empty string at root.
    pub effective_context_path: String,
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
        let server_settings = storage.server_settings().await?;
        let (task_pool_size, _) = watch::channel(server_settings.task_pool_size);
        let (domain_events, _) = broadcast::channel(1_024);
        Ok(Self {
            storage,
            search,
            tasks,
            sessions: SessionStore::new(server_settings.remember_me_duration_days),
            claim_lock: Arc::new(Mutex::new(())),
            task_notify: Arc::new(Notify::new()),
            schedule_notify: Arc::new(Notify::new()),
            task_pool_size,
            domain_events,
            shutdown: CancellationToken::new(),
            page_hashing: NonZeroUsize::new(3).expect("page hashing default is nonzero"),
            kobo_sync_item_limit: NonZeroUsize::new(100).expect("Kobo sync default is nonzero"),
            kobo_kepub: kobo::kepub::KepubService::configure(server_settings.kepubify_path.as_ref().map(PathBuf::from))
                .await,
            kobo_proxy: kobo::proxy::KoboProxy::new(server_settings.kobo_proxy.unwrap_or(false)),
            kobo_port: Arc::new(AtomicU16::new(server_settings.kobo_port.unwrap_or(0))),
            task_metrics: metrics_routes::TaskMetrics::default(),
            operations: operations_routes::OperationsService::default(),
            database_kobo_proxy: server_settings.kobo_proxy,
            database_kepubify_path: server_settings.kepubify_path.map(PathBuf::from),
            configuration_server_port: 25_600,
            configuration_server_context_path: None,
            effective_server_port: None,
            effective_server_context_path: String::new(),
            configuration_kobo_proxy: false,
            configuration_kepubify_path: None,
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
        self.configuration_kepubify_path = path.clone();
        self.kobo_kepub
            .reconfigure(self.database_kepubify_path.clone(), path)
            .await;
        self
    }

    /// Enables or disables forwarding unsupported Kobo requests to the official store.
    #[must_use]
    pub fn with_kobo_proxy(mut self, enabled: bool) -> Self {
        self.configuration_kobo_proxy = enabled;
        self.kobo_proxy.set_enabled(self.database_kobo_proxy.unwrap_or(enabled));
        self
    }

    /// Records the configured and effective HTTP binding used by this process.
    #[must_use]
    pub fn with_server_bind_settings(mut self, settings: ServerBindSettings) -> Self {
        self.configuration_server_port = settings.configuration_port;
        self.configuration_server_context_path = settings.configuration_context_path;
        self.effective_server_port = settings.effective_port;
        self.effective_server_context_path = settings.effective_context_path;
        self
    }

    async fn apply_server_settings(&self, settings: &ServerSettings, remember_me_key_renewed: bool) {
        self.sessions
            .set_remember_me_duration_days(settings.remember_me_duration_days);
        if remember_me_key_renewed {
            self.sessions.remove_remembered();
        }
        self.task_pool_size.send_replace(settings.task_pool_size);
        self.task_notify.notify_waiters();
        self.kobo_proxy
            .set_enabled(settings.kobo_proxy.unwrap_or(self.configuration_kobo_proxy));
        self.kobo_port.store(settings.kobo_port.unwrap_or(0), Ordering::Relaxed);
        self.kobo_kepub
            .reconfigure(
                settings.kepubify_path.as_ref().map(PathBuf::from),
                self.configuration_kepubify_path.clone(),
            )
            .await;
    }

    fn kobo_public_port(&self) -> Option<u16> {
        NonZeroU16::new(self.kobo_port.load(Ordering::Relaxed)).map(NonZeroU16::get)
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
