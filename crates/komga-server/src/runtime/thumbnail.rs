use super::*;

pub(super) async fn process_generate_thumbnail_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid GenerateBookThumbnail payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for thumbnail generation");
            return TaskOutcome::Retry;
        }
    };
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) => media,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load media for thumbnail generation");
            return TaskOutcome::Retry;
        }
    };
    let max_edge = match state.storage.thumbnail_max_edge().await {
        Ok(max_edge) => max_edge,
        Err(error) => {
            tracing::error!(%error, "could not read thumbnail size");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    let materialized = match state.mobi.materialize_book(&book, &media, &state.shutdown).await {
        Ok(materialized) => materialized,
        Err(crate::mobi::MaterializationError::Cancelled) => return TaskOutcome::Cancelled,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not materialize book for thumbnail generation");
            return TaskOutcome::Completed;
        }
    };
    let generated = tokio::task::spawn_blocking(move || generate_thumbnail(&materialized.book, &media, max_edge)).await;
    let generated = match generated {
        Ok(Ok(generated)) => generated,
        Ok(Err(MediaOperationError::NotReady | MediaOperationError::PageOutOfBounds)) => return TaskOutcome::Completed,
        Ok(Err(error)) => {
            tracing::error!(%error, book_id = %book.id, "could not generate book thumbnail");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "thumbnail generator panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    let Some(dimension) = image_dimension(&generated.bytes) else {
        tracing::error!(book_id = %book.id, "generated thumbnail has no readable dimensions");
        return TaskOutcome::Completed;
    };
    let thumbnail = ThumbnailBook {
        id: new_id(),
        file_size: i64::try_from(generated.bytes.len()).unwrap_or(i64::MAX),
        thumbnail: Some(generated.bytes),
        url: None,
        selected: false,
        thumbnail_type: "GENERATED".to_owned(),
        media_type: generated.media_type,
        dimension,
        book_id: book.id.clone(),
    };
    let selected = match state.storage.replace_generated_thumbnail(&thumbnail).await {
        Ok(selected) => selected,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist generated thumbnail");
            return TaskOutcome::Retry;
        }
    };
    state.publish_domain_event(DomainEvent::ThumbnailBookAdded(BookThumbnailEvent {
        book_id: book.id,
        series_id: book.series_id,
        selected,
    }));
    TaskOutcome::Completed
}
