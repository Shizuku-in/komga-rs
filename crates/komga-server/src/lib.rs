//! Axum composition, authentication, and process configuration for Komga.
//!
//! [`build_app`] assembles the compatibility routes around an [`AppState`].
//! Domain policy and SQLite access remain in `komga-core` and `komga-storage`
//! respectively, which keeps HTTP concerns out of those crates.

#![warn(missing_docs)]

mod auth;
mod book_file_routes;
/// Command-line and environment configuration.
pub mod config;
mod content_routes;
mod dto;
mod error;
mod filesystem_routes;
mod library_routes;
mod media_routes;
mod metadata_routes;
mod read_progress_routes;
mod routes;
mod runtime;
mod settings_routes;
mod sse_routes;
mod thumbnail_routes;
mod webpub_routes;

pub use auth::{REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER};
pub use runtime::{AppRuntime, ShutdownHandle};

use std::sync::Arc;

use auth::SessionStore;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{delete, get, patch, post},
};
use komga_storage::{Storage, StorageError, TaskRecord, TaskStorage};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

/// Shared dependencies used by the HTTP router.
///
/// Construct this state with [`AppState::new`] and pass it to [`build_app`].
/// Session state is intentionally process-local, matching the current Kotlin
/// Caffeine session repository: restarting the process expires active sessions.
#[derive(Clone)]
pub struct AppState {
    storage: Storage,
    tasks: TaskStorage,
    sessions: SessionStore,
    claim_lock: Arc<Mutex<()>>,
    task_notify: Arc<Notify>,
    schedule_notify: Arc<Notify>,
    shutdown: CancellationToken,
}

impl AppState {
    /// Creates application state around initialized main and task databases.
    ///
    /// # Errors
    ///
    /// Returns a storage error when server settings cannot be loaded.
    pub async fn new(storage: Storage, tasks: TaskStorage) -> Result<Self, StorageError> {
        let remember_me_duration_days = storage.remember_me_duration_days().await?;
        Ok(Self {
            storage,
            tasks,
            sessions: SessionStore::new(remember_me_duration_days),
            claim_lock: Arc::new(Mutex::new(())),
            task_notify: Arc::new(Notify::new()),
            schedule_notify: Arc::new(Notify::new()),
            shutdown: CancellationToken::new(),
        })
    }

    pub(crate) async fn enqueue_task(&self, task: &TaskRecord) -> Result<(), StorageError> {
        self.tasks.save(task).await?;
        self.task_notify.notify_waiters();
        Ok(())
    }

    pub(crate) fn library_schedule_changed(&self) {
        self.schedule_notify.notify_one();
    }
}

/// Builds the Komga HTTP router and its middleware stack.
///
/// The returned router includes public health and claim endpoints, authenticated
/// user/session endpoints, CORS handling, request IDs, a 2 MiB JSON body limit,
/// and request tracing. It can be served directly with `axum::serve` or cloned
/// for in-process contract tests.
pub fn build_app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v2/users/me", get(routes::current_user))
        .route("/api/v2/users/me/password", patch(routes::update_my_password))
        .route(
            "/api/v2/users/me/api-keys",
            get(routes::list_api_keys).post(routes::create_api_key),
        )
        .route("/api/v2/users/me/api-keys/{key_id}", delete(routes::delete_api_key))
        .route("/api/v2/users", get(routes::list_users).post(routes::create_user))
        .route(
            "/api/v2/users/{user_id}",
            patch(routes::patch_user).delete(routes::delete_user),
        )
        .route("/api/v2/users/{user_id}/password", patch(routes::update_user_password))
        .route("/api/v1/login/set-cookie", get(routes::set_cookie))
        .route("/api/logout", get(routes::logout).post(routes::logout))
        .route("/api/v1/series/list", post(content_routes::list_series))
        .route("/api/v1/series/{series_id}", get(content_routes::get_series))
        .route(
            "/api/v1/series/{series_id}/thumbnail",
            get(thumbnail_routes::get_series_thumbnail),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnails",
            get(thumbnail_routes::list_series_thumbnails)
                .post(thumbnail_routes::upload_series_thumbnail)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnails/{thumbnail_id}",
            get(thumbnail_routes::get_series_thumbnail_by_id).delete(thumbnail_routes::delete_series_thumbnail),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnails/{thumbnail_id}/selected",
            axum::routing::put(thumbnail_routes::select_series_thumbnail),
        )
        .route(
            "/api/v1/series/{series_id}/read-progress",
            post(read_progress_routes::mark_series_read_completed)
                .delete(read_progress_routes::delete_series_read_progress),
        )
        .route(
            "/api/v2/series/{series_id}/read-progress/tachiyomi",
            get(read_progress_routes::get_tachiyomi_series_read_progress)
                .put(read_progress_routes::update_tachiyomi_series_read_progress),
        )
        .route(
            "/api/v1/series/{series_id}/metadata",
            patch(metadata_routes::update_series_metadata),
        )
        .route("/api/v1/books/list", post(content_routes::list_books))
        .route(
            "/api/v1/books/thumbnails",
            axum::routing::put(thumbnail_routes::regenerate_book_thumbnails),
        )
        .route(
            "/api/v1/books/metadata",
            patch(metadata_routes::update_book_metadata_batch),
        )
        .route("/api/v1/books/{book_id}", get(content_routes::get_book))
        .route(
            "/api/v1/books/{book_id}/metadata",
            patch(metadata_routes::update_book_metadata),
        )
        .route(
            "/api/v1/books/{book_id}/read-progress",
            patch(read_progress_routes::update_read_progress).delete(read_progress_routes::delete_read_progress),
        )
        .route(
            "/api/v1/books/{book_id}/progression",
            get(read_progress_routes::get_progression).put(read_progress_routes::update_progression),
        )
        .route(
            "/api/v1/books/{book_id}/file",
            get(book_file_routes::download_book_file),
        )
        .route(
            "/api/v1/books/{book_id}/file/{*file_name}",
            get(book_file_routes::download_book_file_with_name),
        )
        .route("/api/v1/books/{book_id}/analyze", post(media_routes::analyze_book))
        .route("/api/v1/books/{book_id}/pages", get(media_routes::list_pages))
        .route("/api/v1/books/{book_id}/positions", get(media_routes::get_positions))
        .route("/api/v1/books/{book_id}/manifest", get(webpub_routes::get_manifest))
        .route(
            "/api/v1/books/{book_id}/manifest/divina",
            get(webpub_routes::get_divina_manifest),
        )
        .route(
            "/api/v1/books/{book_id}/manifest/pdf",
            get(webpub_routes::get_pdf_manifest),
        )
        .route(
            "/api/v1/books/{book_id}/manifest/epub",
            get(webpub_routes::get_epub_manifest),
        )
        .route(
            "/api/v1/books/{book_id}/pages/{page_number}",
            get(media_routes::get_page),
        )
        .route(
            "/api/v1/books/{book_id}/pages/{page_number}/thumbnail",
            get(media_routes::get_page_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/pages/{page_number}/raw",
            get(media_routes::get_raw_page),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnail",
            get(thumbnail_routes::get_book_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnails",
            get(thumbnail_routes::list_book_thumbnails)
                .post(thumbnail_routes::upload_book_thumbnail)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnails/{thumbnail_id}",
            get(thumbnail_routes::get_book_thumbnail_by_id).delete(thumbnail_routes::delete_book_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnails/{thumbnail_id}/selected",
            axum::routing::put(thumbnail_routes::select_book_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/previous",
            get(content_routes::get_previous_book),
        )
        .route("/api/v1/books/{book_id}/next", get(content_routes::get_next_book))
        .route(
            "/api/v1/libraries",
            get(library_routes::list_libraries).post(library_routes::create_library),
        )
        .route(
            "/api/v1/libraries/{library_id}",
            get(library_routes::get_library)
                .put(library_routes::update_library)
                .patch(library_routes::update_library)
                .delete(library_routes::delete_library),
        )
        .route(
            "/api/v1/libraries/{library_id}/scan",
            post(library_routes::scan_library),
        )
        .route(
            "/api/v1/libraries/{library_id}/empty-trash",
            post(library_routes::empty_trash),
        )
        .route("/api/v1/filesystem", post(filesystem_routes::directory_listing))
        .route(
            "/api/v1/client-settings/user/list",
            get(settings_routes::list_user_settings),
        )
        .route(
            "/api/v1/client-settings/global",
            patch(settings_routes::save_global_settings).delete(settings_routes::delete_global_settings),
        )
        .route(
            "/api/v1/client-settings/user",
            patch(settings_routes::save_user_settings).delete(settings_routes::delete_user_settings),
        )
        .route("/api/v1/tasks", delete(settings_routes::clear_tasks))
        .route("/sse/v1/events", get(sse_routes::events))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    Router::new()
        .route("/actuator/health", get(routes::health))
        .route("/api/v1/claim", get(routes::claim_status).post(routes::claim_server))
        .route(
            "/api/v1/client-settings/global/list",
            get(settings_routes::list_global_settings),
        )
        .route("/api/v1/oauth2/providers", get(settings_routes::oauth_providers))
        .route(
            "/api/v1/books/{book_id}/resource/{*resource}",
            get(media_routes::get_epub_resource),
        )
        .merge(protected)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
