//! Kobo device synchronization routes and protocol-specific authentication.

mod auth;
mod dto;
mod resources;
mod sync;
mod token;

use axum::{
    Router, middleware,
    routing::{any, get, post},
};

use crate::AppState;

pub(crate) fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/kobo/{auth_token}/ping", get(resources::ping))
        .route("/kobo/{auth_token}/v1/initialization", get(resources::initialization))
        .route("/kobo/{auth_token}/v1/auth/device", post(resources::auth_device))
        .route("/kobo/{auth_token}/v1/library/sync", get(sync::library_sync))
        .route(
            "/kobo/{auth_token}/v1/library/{book_id}/metadata",
            get(resources::metadata),
        )
        .route("/kobo/{auth_token}/{*path}", any(resources::catch_all))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_kobo_auth))
}
