//! Axum composition, authentication, and process configuration for Komga.
//!
//! [`build_app`] assembles the compatibility routes around an [`AppState`].
//! Domain policy and SQLite access remain in `komga-core` and `komga-storage`
//! respectively, which keeps HTTP concerns out of those crates.

#![warn(missing_docs)]

mod auth;
/// Command-line and environment configuration.
pub mod config;
mod dto;
mod error;
mod routes;

pub use auth::{REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER};

use std::sync::Arc;

use auth::SessionStore;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{delete, get, patch},
};
use komga_storage::Storage;
use tokio::sync::Mutex;
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
    sessions: SessionStore,
    claim_lock: Arc<Mutex<()>>,
}

impl AppState {
    /// Creates application state around an initialized storage handle.
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            sessions: SessionStore::default(),
            claim_lock: Arc::new(Mutex::new(())),
        }
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
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    Router::new()
        .route("/actuator/health", get(routes::health))
        .route("/api/v1/claim", get(routes::claim_status).post(routes::claim_server))
        .merge(protected)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
