//! Administrative source-file deletion task endpoints.

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
};
use komga_storage::TaskRecord;

use crate::{AppState, auth::AuthContext, error::ApiError, routes::require_admin};

pub(crate) async fn delete_book_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state.enqueue_task(&TaskRecord::delete_book(&book_id)).await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn delete_series_files(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state.enqueue_task(&TaskRecord::delete_series(&series_id)).await?;
    Ok(StatusCode::ACCEPTED)
}
