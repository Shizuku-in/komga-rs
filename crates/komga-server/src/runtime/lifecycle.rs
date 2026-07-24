use komga_storage::TaskRecord;

use crate::AppState;

use super::{LibraryTaskPayload, TaskOutcome};

pub(super) async fn process_empty_trash_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<LibraryTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid EmptyTrash payload");
            return TaskOutcome::Completed;
        }
    };
    perform_empty_trash(state, &payload.library_id).await
}

pub(super) async fn perform_empty_trash(state: &AppState, library_id: &str) -> TaskOutcome {
    let result = match state.storage.empty_trash(library_id).await {
        Ok(Some(result)) => result,
        Ok(None) => {
            tracing::warn!(%library_id, "empty-trash task references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, %library_id, "could not empty library trash");
            return TaskOutcome::Retry;
        }
    };
    for target in &result.numbering_refreshes {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::refresh_book_numbering(
                &target.book_id,
                &target.series_id,
                4,
            ))
            .await
        {
            tracing::error!(%error, book_id = %target.book_id, "could not enqueue numbering metadata refresh");
            return TaskOutcome::Retry;
        }
    }
    tracing::info!(%library_id, ?result, "library trash emptied");
    TaskOutcome::Completed
}
