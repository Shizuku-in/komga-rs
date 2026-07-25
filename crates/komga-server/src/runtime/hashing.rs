use chrono::{DateTime, Local};

use super::*;

pub(super) async fn process_hash_book_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid HashBook payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => {
            tracing::warn!(book_id = %payload.book_id, "hash task references a missing book");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for hashing");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load file hashing settings");
            return TaskOutcome::Retry;
        }
    };
    if !library.hash_files || !book.file_hash.is_empty() {
        return TaskOutcome::Completed;
    }
    let path = match book.url.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            tracing::error!(book_id = %book.id, url = %book.url, "book hash source is not a local file");
            return TaskOutcome::Completed;
        }
    };
    let cancellation = state.shutdown.clone();
    let expected_size = book.file_size;
    let expected_modified = book.file_last_modified;
    let result = tokio::task::spawn_blocking(move || {
        let hash = hash_file(&path, || cancellation.is_cancelled())?;
        let metadata = path.metadata()?;
        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let modified = DateTime::<Local>::from(metadata.modified()?).naive_local();
        if size != expected_size || modified != expected_modified {
            return Err(FileHashError::SourceChanged(path.display().to_string()));
        }
        Ok(hash)
    })
    .await;
    let hash = match result {
        Ok(Ok(hash)) => hash,
        Ok(Err(FileHashError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(FileHashError::SourceChanged(error))) => {
            tracing::warn!(book_id = %book.id, source = %error, "book changed while hashing");
            return TaskOutcome::Retry;
        }
        Ok(Err(error)) => {
            tracing::error!(%error, book_id = %book.id, "could not hash book source");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "book file hasher panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state.storage.update_book_file_hash_if_current(&book, &hash).await {
        Ok(true) => {
            tracing::info!(book_id = %book.id, "book file hash updated");
            TaskOutcome::Completed
        }
        Ok(false) => TaskOutcome::Retry,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist book file hash");
            TaskOutcome::Retry
        }
    }
}
