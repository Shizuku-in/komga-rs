//! Administrative process lifecycle endpoints.

use axum::{
    Json,
    body::Body,
    extract::{Extension, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tokio::{fs::File, io::AsyncReadExt};
use tokio_util::io::ReaderStream;

use crate::{AppState, auth::AuthContext, error::ApiError, routes::require_admin};

#[derive(Debug, Serialize)]
pub(crate) struct ShutdownDto {
    message: &'static str,
}

const STREAM_BUFFER_CAPACITY: usize = 8 * 1024;

pub(crate) async fn logfile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Response, ApiError> {
    require_admin(&auth)?;
    let path = state
        .log_file
        .as_deref()
        .ok_or_else(|| ApiError::not_found("Log file not found"))?;
    let file = File::open(path).await.map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => ApiError::not_found("Log file not found"),
        _ => {
            tracing::error!(%error, path = %path.display(), "could not open application log file");
            ApiError::internal("Could not open log file")
        }
    })?;
    let metadata = file.metadata().await.map_err(|error| {
        tracing::error!(%error, path = %path.display(), "could not inspect application log file");
        ApiError::internal("Could not inspect log file")
    })?;
    if !metadata.is_file() {
        return Err(ApiError::not_found("Log file not found"));
    }

    let stream = ReaderStream::with_capacity(file.take(metadata.len()), STREAM_BUFFER_CAPACITY);
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain;charset=UTF-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string())
            .map_err(|_| ApiError::internal("Could not encode log file length"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"komga.log\""),
    );
    Ok(response)
}

pub(crate) async fn shutdown(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<ShutdownDto>, ApiError> {
    require_admin(&auth)?;
    let shutdown = state.shutdown.clone();
    // Hand the acknowledgement back to Axum before waking graceful shutdown.
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        shutdown.cancel();
    });
    Ok(Json(ShutdownDto {
        message: "Shutting down, bye...",
    }))
}
