use super::*;

pub(super) async fn process_analyze_book_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid AnalyzeBook payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => {
            tracing::warn!(book_id = %payload.book_id, "analysis task references a missing book");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for analysis");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load analysis settings");
            return TaskOutcome::Retry;
        }
    };
    let import_metadata = library.import_comic_info_book
        || library.import_comic_info_series
        || library.import_comic_info_collection
        || library.import_comic_info_read_list
        || library.import_epub_book
        || library.import_epub_series;
    let cancellation = state.shutdown.clone();
    let analyzed_book = book.clone();
    let result = tokio::task::spawn_blocking(move || {
        analyze_book(&analyzed_book, library.analyze_dimensions, || {
            cancellation.is_cancelled()
        })
    })
    .await;
    let media = match result {
        Ok(Ok(media)) => media,
        Ok(Err(MediaAnalysisError::Cancelled)) => return TaskOutcome::Cancelled,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "media analyzer panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    if let Err(error) = state.storage.replace_media_analysis(&media).await {
        tracing::error!(%error, book_id = %book.id, "could not persist media analysis");
        return TaskOutcome::Retry;
    }
    if !state.sync_books(std::slice::from_ref(&book.id)).await {
        state.recover_search_after_http_failure().await;
        return TaskOutcome::Retry;
    }
    if media.status == MediaStatus::Ready
        && let Err(error) = state
            .enqueue_task(&TaskRecord::generate_book_thumbnail(
                &book.id,
                task.priority.saturating_add(1),
            ))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue generated thumbnail");
        return TaskOutcome::Retry;
    }
    if media.status == MediaStatus::Ready
        && import_metadata
        && let Err(error) = state
            .enqueue_task(&TaskRecord::refresh_book_metadata(
                &book.id,
                &book.series_id,
                task.priority,
            ))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue book metadata refresh");
        return TaskOutcome::Retry;
    }
    tracing::info!(book_id = %book.id, status = media.status.as_str(), pages = media.page_count, "book analysis completed");
    TaskOutcome::Completed
}
