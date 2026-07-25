use std::collections::BTreeMap;

use komga_core::{HashedPageTarget, PageHashAction};
use komga_media::{ZipPageRemovalError, remove_hashed_zip_pages};

use super::*;

pub(super) async fn process_find_duplicate_pages_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<LibraryTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid FindDuplicatePagesToDelete payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load duplicate-page library");
            return TaskOutcome::Retry;
        }
    };
    let targets = match state
        .storage
        .page_targets_by_actions(&[PageHashAction::DeleteAuto], Some(&library.id))
        .await
    {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, library_id = %library.id, "could not find automatic duplicate pages");
            return TaskOutcome::Retry;
        }
    };
    let mut grouped = BTreeMap::<String, Vec<HashedPageTarget>>::new();
    for (book_id, target) in targets {
        grouped.entry(book_id).or_default().push(target);
    }
    for (book_id, targets) in grouped {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::remove_hashed_pages(
                &book_id,
                &targets,
                task.priority.saturating_add(1),
            ))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue automatic duplicate-page removal");
            return TaskOutcome::Retry;
        }
    }
    TaskOutcome::Completed
}

pub(super) async fn process_remove_hashed_pages_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<RemoveHashedPagesPayload>(&task.payload) {
        Ok(payload) if !payload.pages.is_empty() => payload,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid RemoveHashedPages payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load duplicate-page book");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load duplicate-page library");
            return TaskOutcome::Retry;
        }
    };
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) => media,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load media for duplicate-page removal");
            return TaskOutcome::Retry;
        }
    };
    let work_book = book.clone();
    let work_media = media.clone();
    let analyze_dimensions = library.analyze_dimensions;
    let cancellation = state.shutdown.clone();
    let targets = payload.pages;
    let result = tokio::task::spawn_blocking(move || {
        remove_hashed_zip_pages(&work_book, &work_media, &targets, analyze_dimensions, || {
            cancellation.is_cancelled()
        })
    })
    .await;
    let removal = match result {
        Ok(Ok(removal)) => removal,
        Ok(Err(ZipPageRemovalError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(error @ (ZipPageRemovalError::Stale(_) | ZipPageRemovalError::Unsupported(_)))) => {
            tracing::warn!(%error, book_id = %book.id, "duplicate-page removal task is stale");
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Completed;
        }
        Ok(Err(error @ (ZipPageRemovalError::Hash(_) | ZipPageRemovalError::Validation(_)))) => {
            tracing::error!(%error, book_id = %book.id, "duplicate-page removal failed validation");
            return TaskOutcome::Completed;
        }
        Ok(Err(error @ ZipPageRemovalError::Archive(_))) => {
            tracing::error!(%error, book_id = %book.id, "duplicate-page source archive is invalid");
            return TaskOutcome::Completed;
        }
        Ok(Err(error @ ZipPageRemovalError::Io(_))) => {
            tracing::error!(%error, book_id = %book.id, "duplicate-page rewrite encountered an I/O error");
            return TaskOutcome::Retry;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "duplicate-page rewrite panicked");
            return TaskOutcome::Retry;
        }
    };

    match state
        .storage
        .apply_zip_page_removal_if_current(&book, &media, &removal.book, &removal.media, &removal.removed)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(book_id = %book.id, "database changed after duplicate-page source replacement");
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist duplicate-page removal");
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Retry;
        }
    }

    if removal.removed_first_page
        && let Err(error) = state
            .enqueue_task(&TaskRecord::generate_book_thumbnail(
                &book.id,
                task.priority.saturating_add(1),
            ))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue cover regeneration after page removal");
        enqueue_recovery_scan(state, &book.library_id).await;
        return TaskOutcome::Retry;
    }
    if library.hash_pages
        && let Err(error) = state
            .enqueue_task(&TaskRecord::hash_book_pages(&book.id, task.priority.saturating_add(1)))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue page hashing after page removal");
        enqueue_recovery_scan(state, &book.library_id).await;
        return TaskOutcome::Retry;
    }
    if library.hash_files
        && let Err(error) = state
            .enqueue_task(&TaskRecord::hash_book(&book.id, task.priority.saturating_add(1)))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue file hashing after page removal");
        enqueue_recovery_scan(state, &book.library_id).await;
        return TaskOutcome::Retry;
    }
    if !state.sync_books(std::slice::from_ref(&book.id)).await {
        state.recover_search_after_http_failure().await;
    }
    tracing::info!(book_id = %book.id, pages = removal.removed.len(), "duplicate pages removed");
    TaskOutcome::Completed
}

async fn enqueue_recovery_scan(state: &AppState, library_id: &str) {
    if let Err(error) = state.enqueue_task(&TaskRecord::scan_library(library_id, true, 8)).await {
        tracing::error!(%error, %library_id, "could not enqueue deep scan after page-removal inconsistency");
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, fs::File, io::Write, path::PathBuf};

    use chrono::Utc;
    use komga_core::{Library, PageHashAction, User, page_progress, scan_library_files};
    use komga_media::{analyze_book, extract_page, hash_page};
    use komga_search::{EntityKind, SearchMutation};
    use komga_storage::{PageHashUpdate, Storage, TaskStorage};
    use url::Url;
    use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

    use super::*;

    #[tokio::test]
    async fn automatic_removal_updates_file_database_followups_and_search_without_remapping_progress() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let path = series.join("Volume.cbz");
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/hashpage");
        let first_page = fs::read(fixtures.join("tg.png/1.png")).unwrap();
        let other_page = fs::read(fixtures.join("xt.png/1.png")).unwrap();
        let mut archive = ZipWriter::new(File::create(&path).unwrap());
        for number in 1..=3 {
            archive
                .start_file(format!("{number}.png"), SimpleFileOptions::default())
                .unwrap();
            archive
                .write_all(if number == 1 { &first_page } else { &other_page })
                .unwrap();
        }
        archive.finish().unwrap();

        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.hash_files = true;
        library.hash_pages = true;
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let book = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        let analyzed = analyze_book(&book, false, || false).unwrap();
        storage.replace_media_analysis(&analyzed).await.unwrap();
        let book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let updates = media
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let content = extract_page(&book, &media, i32::try_from(index + 1).unwrap()).unwrap();
                let computed = hash_page(&content.bytes, &page.media_type).unwrap();
                PageHashUpdate {
                    number: i32::try_from(index).unwrap(),
                    file_name: page.file_name.clone(),
                    media_type: page.media_type.clone(),
                    file_hash: computed.hash,
                    algorithm: computed.algorithm.into(),
                }
            })
            .collect::<Vec<_>>();
        assert!(
            storage
                .update_page_hashes_if_current(&book, &media, &updates)
                .await
                .unwrap()
        );
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let delete_hash = media.pages[0].file_hash.clone();
        storage
            .create_or_update_page_hash(&delete_hash, media.pages[0].file_size, PageHashAction::DeleteAuto, None)
            .await
            .unwrap();
        let user = User::new("reader@example.org".into(), "hash".into());
        storage.create_user(&user).await.unwrap();
        storage
            .save_read_progress(&page_progress(&book.id, &user.id, &media, 2, Utc::now().naive_utc()).unwrap())
            .await
            .unwrap();

        let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
        state
            .search
            .mutate([SearchMutation::Delete {
                kind: EntityKind::Book,
                id: book.id.clone(),
            }])
            .unwrap();
        let discovery = TaskRecord::find_duplicate_pages_to_delete(&library.id, 0);
        assert_eq!(
            process_find_duplicate_pages_task(&state, &discovery).await,
            TaskOutcome::Completed
        );
        let removal = tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.simple_type == "RemoveHashedPages")
            .unwrap();
        let payload: serde_json::Value = serde_json::from_str(&removal.payload).unwrap();
        assert_eq!(payload["bookId"], book.id);
        assert_eq!(payload["pages"][0]["pageNumber"], 1);
        assert_eq!(payload["pages"][0]["fileHash"], delete_hash);

        assert_eq!(
            process_remove_hashed_pages_task(&state, &removal).await,
            TaskOutcome::Completed
        );
        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert!(saved_book.file_hash.is_empty() && saved_book.file_hash_koreader.is_empty());
        let saved_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_media.pages.len(), 2);
        assert_eq!(saved_media.pages[0].file_name, "2.png");
        assert!(saved_media.pages.iter().all(|page| !page.file_hash.is_empty()));
        assert_eq!(
            storage
                .find_known_page_hash(&delete_hash)
                .await
                .unwrap()
                .unwrap()
                .delete_count,
            1
        );
        let history = storage
            .list_historical_events(&komga_core::PageRequest::default())
            .await
            .unwrap();
        assert_eq!(history.total_elements, 1);
        assert_eq!(history.content[0].event_type, "DuplicatePageDeleted");
        assert_eq!(history.content[0].book_id.as_deref(), Some(book.id.as_str()));
        assert_eq!(history.content[0].properties["page file hash"], delete_hash);
        assert_eq!(history.content[0].properties["page file name"], "1.png");
        assert_eq!(
            storage
                .find_read_progress(&book.id, &user.id)
                .await
                .unwrap()
                .unwrap()
                .page,
            2
        );
        assert_eq!(ZipArchive::new(File::open(&path).unwrap()).unwrap().len(), 2);
        let queued = tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|task| task.simple_type)
            .collect::<Vec<_>>();
        assert!(queued.iter().any(|value| value == "GenerateBookThumbnail"));
        assert!(queued.iter().any(|value| value == "HashBookPages"));
        assert!(queued.iter().any(|value| value == "HashBook"));
        assert_eq!(state.search.search(EntityKind::Book, "volume")[0].id, book.id);

        let after = fs::read(&path).unwrap();
        assert_eq!(
            process_remove_hashed_pages_task(&state, &removal).await,
            TaskOutcome::Completed
        );
        assert_eq!(fs::read(&path).unwrap(), after);
        assert!(
            tasks
                .list()
                .await
                .unwrap()
                .iter()
                .any(|task| task.simple_type == "ScanLibrary")
        );
    }
}
