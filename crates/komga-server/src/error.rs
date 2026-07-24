//! Conversion of application failures into Spring-compatible JSON responses.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use komga_storage::StorageError;
use serde::Serialize;

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody<'a> {
    status: u16,
    error: &'a str,
    message: &'a str,
}

impl ApiError {
    /// Creates a validation or malformed-request response.
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    /// Creates an authentication challenge response.
    pub(crate) fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "Full authentication is required to access this resource",
        )
    }

    /// Creates an authorization failure response.
    pub(crate) fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "Forbidden")
    }

    /// Creates a missing-resource response.
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    /// Creates a state-conflict response.
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// Creates an unsupported-media response.
    pub(crate) fn unsupported_media_type() -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported Media Type")
    }

    /// Creates a request-body size failure.
    pub(crate) fn payload_too_large() -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "Payload Too Large")
    }

    /// Creates an internal failure response without leaking its cause.
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    /// Creates an error with an explicit HTTP status and client-safe message.
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let reason = self.status.canonical_reason().unwrap_or("Error");
        let mut response = (
            self.status,
            Json(ErrorBody {
                status: self.status.as_u16(),
                error: reason,
                message: &self.message,
            }),
        )
            .into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                http::header::WWW_AUTHENTICATE,
                http::HeaderValue::from_static("Basic realm=\"Komga\""),
            );
        }
        response
    }
}

impl From<StorageError> for ApiError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::DuplicateEmail => Self::bad_request("A user with this email already exists"),
            StorageError::DuplicateApiKeyComment => Self::bad_request("ERR_1034"),
            StorageError::InvalidData(error) => {
                tracing::error!(%error, "invalid persisted data");
                Self::internal("Invalid persisted data")
            }
            StorageError::InvalidQuery(error) => Self::bad_request(error),
            StorageError::Database(error) => {
                tracing::error!(%error, "database request failed");
                Self::internal("Database request failed")
            }
            StorageError::Migration(error) => {
                tracing::error!(%error, "database migration failed");
                Self::internal("Database migration failed")
            }
        }
    }
}
