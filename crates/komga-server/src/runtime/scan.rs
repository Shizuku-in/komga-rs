use super::*;

pub(super) async fn process_scan_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<ScanLibraryPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid ScanLibrary payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => {
            tracing::warn!(library_id = %payload.library_id, "scan task references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load library for scan");
            return TaskOutcome::Retry;
        }
    };
    let empty_trash_after_scan = library.empty_trash_after_scan;
    let cancellation = state.shutdown.clone();
    let scan = tokio::task::spawn_blocking(move || scan_library_files(&library, || cancellation.is_cancelled())).await;
    let scan = match scan {
        Ok(Ok(scan)) => scan,
        Ok(Err(ScanError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(ScanError::RootUnavailable(path))) => {
            if let Err(error) = state.storage.set_library_unavailable(&payload.library_id, true).await {
                tracing::error!(%error, library_id = %payload.library_id, "could not mark library unavailable");
            }
            tracing::error!(library_id = %payload.library_id, root = %path.display(), "library scan failed");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "filesystem scanner panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .apply_library_scan(&payload.library_id, &scan, payload.scan_deep)
        .await
    {
        Ok(stats) => {
            tracing::info!(library_id = %payload.library_id, ?stats, "library scan completed");
            for series_id in stats.sidecars_changed {
                if let Err(error) = state
                    .enqueue_task(&TaskRecord::refresh_series_metadata(&series_id, task.priority))
                    .await
                {
                    tracing::error!(%error, %series_id, "could not enqueue sidecar metadata refresh");
                    return TaskOutcome::Retry;
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not persist library scan");
            return TaskOutcome::Retry;
        }
    }
    if empty_trash_after_scan {
        if perform_empty_trash(state, &payload.library_id).await == TaskOutcome::Retry {
            return TaskOutcome::Retry;
        }
    } else if let Err(error) = state.storage.cleanup_empty_containers().await {
        tracing::error!(%error, library_id = %payload.library_id, "could not clean empty collections and read lists");
        return TaskOutcome::Retry;
    }
    let targets = match state.storage.list_book_analysis_targets(&payload.library_id).await {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not list books requiring analysis");
            return TaskOutcome::Retry;
        }
    };
    for (book_id, series_id) in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::analyze_book(&book_id, &series_id, task.priority))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue book analysis");
            return TaskOutcome::Retry;
        }
    }
    if payload.scan_deep {
        let targets = match state.storage.list_ready_books_by_library_id(&payload.library_id).await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::error!(%error, library_id = %payload.library_id, "could not list ready books for metadata refresh");
                return TaskOutcome::Retry;
            }
        };
        for (book_id, series_id) in targets {
            if let Err(error) = state
                .enqueue_task(&TaskRecord::refresh_book_metadata(&book_id, &series_id, task.priority))
                .await
            {
                tracing::error!(%error, %book_id, "could not enqueue book metadata refresh");
                return TaskOutcome::Retry;
            }
        }
    }
    TaskOutcome::Completed
}
