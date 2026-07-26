//! Administrative batch book-import endpoint.

use axum::{Json, extract::Extension, extract::State, http::StatusCode};
use komga_core::CopyMode;
use komga_storage::TaskRecord;
use serde::Deserialize;

use crate::{AppState, auth::AuthContext, error::ApiError, routes::require_admin};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BookImportBatch {
    #[serde(default)]
    books: Vec<BookImportItem>,
    copy_mode: CopyMode,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BookImportItem {
    source_file: String,
    series_id: String,
    upgrade_book_id: Option<String>,
    destination_name: Option<String>,
}

pub(crate) async fn import_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(batch): Json<BookImportBatch>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    for item in batch.books {
        let task = TaskRecord::import_book(
            &item.source_file,
            &item.series_id,
            batch.copy_mode,
            item.destination_name.as_deref(),
            item.upgrade_book_id.as_deref(),
        );
        if let Err(error) = state.enqueue_task(&task).await {
            tracing::error!(%error, source_file = %item.source_file, series_id = %item.series_id, "could not enqueue book import");
        }
    }
    Ok(StatusCode::ACCEPTED)
}
