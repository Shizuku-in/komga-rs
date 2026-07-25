use komga_search::EntityKind;
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
    let deleted = result
        .book_ids
        .iter()
        .cloned()
        .map(|id| (EntityKind::Book, id))
        .chain(result.series_ids.iter().cloned().map(|id| (EntityKind::Series, id)))
        .chain(
            result
                .collection_ids
                .iter()
                .cloned()
                .map(|id| (EntityKind::Collection, id)),
        )
        .chain(
            result
                .read_list_ids
                .iter()
                .cloned()
                .map(|id| (EntityKind::ReadList, id)),
        )
        .collect();
    let refreshed_books = result
        .numbering_refreshes
        .iter()
        .map(|target| target.book_id.clone())
        .collect::<Vec<_>>();
    if !state.delete_search_entities(deleted).await
        || !state.sync_books(&refreshed_books).await
        || !state.sync_series(&result.affected_series_ids).await
    {
        state.recover_search_after_http_failure().await;
        return TaskOutcome::Retry;
    }
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
