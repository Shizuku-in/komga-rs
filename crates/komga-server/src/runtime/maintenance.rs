use super::*;

pub(super) async fn process_find_thumbnails_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<FindThumbnailsPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid FindBookThumbnailsToRegenerate payload");
            return TaskOutcome::Completed;
        }
    };
    let smaller_than = if payload.for_bigger_result_only {
        match state.storage.thumbnail_max_edge().await {
            Ok(size) => Some(i32::try_from(size).unwrap_or(i32::MAX)),
            Err(error) => {
                tracing::error!(%error, "could not read thumbnail size");
                return TaskOutcome::Retry;
            }
        }
    } else {
        None
    };
    let targets = match state
        .storage
        .list_book_thumbnail_regeneration_targets(smaller_than)
        .await
    {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, "could not find thumbnail regeneration targets");
            return TaskOutcome::Retry;
        }
    };
    for book_id in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::generate_book_thumbnail(&book_id, task.priority))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue generated thumbnail");
            return TaskOutcome::Retry;
        }
    }
    TaskOutcome::Completed
}

pub(super) async fn process_aggregate_series_metadata_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<SeriesTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid AggregateSeriesMetadata payload");
            return TaskOutcome::Completed;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .rebuild_book_metadata_aggregation(&payload.series_id)
        .await
    {
        Ok(true) => TaskOutcome::Completed,
        Ok(false) => {
            tracing::warn!(series_id = %payload.series_id, "aggregation task references a missing series");
            TaskOutcome::Completed
        }
        Err(error) => {
            tracing::error!(%error, series_id = %payload.series_id, "could not aggregate series metadata");
            TaskOutcome::Retry
        }
    }
}
