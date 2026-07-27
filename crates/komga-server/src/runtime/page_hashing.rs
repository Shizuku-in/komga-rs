use chrono::{DateTime, Local};
use komga_core::{MediaStatus, PDF_MEDIA_TYPE, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, ZIP_MEDIA_TYPE};
use komga_media::{
    MediaOperationError, PageHashError, PdfPageRenderer, RGB_PAGE_HASH_ALGORITHM, extract_page, hash_page,
};
use komga_storage::PageHashUpdate;

use super::*;

#[derive(Debug)]
enum PageHashWorkError {
    Cancelled,
    StaleSnapshot,
    SourceChanged,
    Io(std::io::Error),
    Media(MediaOperationError),
    Hash(PageHashError),
}

pub(super) async fn process_find_missing_page_hashes_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<LibraryTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid FindBooksWithMissingPageHash payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => {
            tracing::warn!(library_id = %payload.library_id, "page hash discovery references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load page hashing settings");
            return TaskOutcome::Retry;
        }
    };
    if !library.hash_pages {
        return TaskOutcome::Completed;
    }
    let targets = match state
        .storage
        .list_books_with_incomplete_page_hashes(&library.id, state.page_hashing.get(), RGB_PAGE_HASH_ALGORITHM)
        .await
    {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, library_id = %library.id, "could not find books with missing page hashes");
            return TaskOutcome::Retry;
        }
    };
    for book_id in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::hash_book_pages(&book_id, task.priority.saturating_add(1)))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue page hashing");
            return TaskOutcome::Retry;
        }
    }
    TaskOutcome::Completed
}

pub(super) async fn process_hash_book_pages_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid HashBookPages payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => {
            tracing::warn!(book_id = %payload.book_id, "page hashing references a missing book");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for page hashing");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not reload page hashing settings");
            return TaskOutcome::Retry;
        }
    };
    if !library.hash_pages {
        return TaskOutcome::Completed;
    }
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) => media,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load media for page hashing");
            return TaskOutcome::Retry;
        }
    };
    if media.status != MediaStatus::Ready
        || !matches!(
            media.media_type.as_deref(),
            Some(ZIP_MEDIA_TYPE | RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE | PDF_MEDIA_TYPE)
        )
    {
        return TaskOutcome::Completed;
    }
    let targets = match state
        .storage
        .list_page_hash_targets(&book.id, state.page_hashing.get(), RGB_PAGE_HASH_ALGORITHM)
        .await
    {
        Ok(targets) if targets.is_empty() => {
            if let Err(error) = state
                .enqueue_task(&TaskRecord::find_duplicate_pages_to_delete(&book.library_id, 0))
                .await
            {
                tracing::error!(%error, book_id = %book.id, "could not enqueue duplicate-page discovery");
                return TaskOutcome::Retry;
            }
            return TaskOutcome::Completed;
        }
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load page hash targets");
            return TaskOutcome::Retry;
        }
    };
    let path = match book.url.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            tracing::error!(book_id = %book.id, url = %book.url, "page hash source is not a local file");
            return TaskOutcome::Completed;
        }
    };
    let hashing_book = book.clone();
    let hashing_media = media.clone();
    let cancellation = state.shutdown.clone();
    let expected_size = book.file_size;
    let expected_modified = book.file_last_modified;
    let result = tokio::task::spawn_blocking(move || {
        let mut updates = Vec::with_capacity(targets.len());
        let pdf_renderer = if hashing_media.media_type.as_deref() == Some(PDF_MEDIA_TYPE) {
            Some(PdfPageRenderer::open(&path).map_err(PageHashWorkError::Media)?)
        } else {
            None
        };
        for target in targets {
            if cancellation.is_cancelled() {
                return Err(PageHashWorkError::Cancelled);
            }
            let index = usize::try_from(target.number).map_err(|_| PageHashWorkError::StaleSnapshot)?;
            let page = hashing_media.pages.get(index).ok_or(PageHashWorkError::StaleSnapshot)?;
            if page.file_name != target.file_name || page.media_type != target.media_type {
                return Err(PageHashWorkError::StaleSnapshot);
            }
            let computed = if let Some(renderer) = &pdf_renderer {
                renderer
                    .render_page_with_hash(index)
                    .map_err(PageHashWorkError::Media)?
                    .hash
            } else {
                let content = extract_page(&hashing_book, &hashing_media, target.number.saturating_add(1))
                    .map_err(PageHashWorkError::Media)?;
                hash_page(&content.bytes, &content.media_type).map_err(PageHashWorkError::Hash)?
            };
            updates.push(PageHashUpdate {
                number: target.number,
                file_name: target.file_name,
                media_type: target.media_type,
                file_hash: computed.hash,
                algorithm: computed.algorithm.into(),
            });
        }
        if cancellation.is_cancelled() {
            return Err(PageHashWorkError::Cancelled);
        }
        let metadata = path.metadata().map_err(PageHashWorkError::Io)?;
        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        let modified = metadata
            .modified()
            .map(DateTime::<Local>::from)
            .map(|value| value.naive_local())
            .map_err(PageHashWorkError::Io)?;
        if size != expected_size || modified != expected_modified {
            return Err(PageHashWorkError::SourceChanged);
        }
        Ok(updates)
    })
    .await;
    let updates = match result {
        Ok(Ok(updates)) => updates,
        Ok(Err(PageHashWorkError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(PageHashWorkError::StaleSnapshot | PageHashWorkError::SourceChanged)) => {
            tracing::warn!(book_id = %book.id, "book or media changed while hashing pages");
            return TaskOutcome::Retry;
        }
        Ok(Err(PageHashWorkError::Io(error))) => {
            tracing::error!(%error, book_id = %book.id, "could not read page hash source");
            return TaskOutcome::Completed;
        }
        Ok(Err(PageHashWorkError::Media(error))) => {
            tracing::error!(%error, book_id = %book.id, "could not extract page for hashing");
            return TaskOutcome::Completed;
        }
        Ok(Err(PageHashWorkError::Hash(error))) => {
            tracing::error!(%error, book_id = %book.id, "could not normalize page for hashing");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "page hasher panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .update_page_hashes_if_current(&book, &media, &updates)
        .await
    {
        Ok(true) => {
            tracing::info!(book_id = %book.id, pages = updates.len(), "book page hashes updated");
            match state
                .enqueue_task(&TaskRecord::find_duplicate_pages_to_delete(&book.library_id, 0))
                .await
            {
                Ok(()) => TaskOutcome::Completed,
                Err(error) => {
                    tracing::error!(%error, book_id = %book.id, "could not enqueue duplicate-page discovery");
                    TaskOutcome::Retry
                }
            }
        }
        Ok(false) => TaskOutcome::Retry,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist page hashes");
            TaskOutcome::Retry
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, fs::File, io::Write, num::NonZeroUsize, path::PathBuf};

    use komga_core::{Library, scan_library_files};
    use komga_media::analyze_book;
    use komga_storage::{Storage, TaskStorage};
    use pdf_writer::{Finish, Rect, Ref};
    use url::Url;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    #[tokio::test]
    async fn discovery_and_hash_tasks_fill_only_configured_zip_edges() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/hashpage/dd.png/1.png");
        let page = fs::read(source).unwrap();
        let mut archive = ZipWriter::new(File::create(series.join("Volume.cbz")).unwrap());
        for number in 1..=8 {
            archive
                .start_file(format!("{number}.png"), SimpleFileOptions::default())
                .unwrap();
            archive.write_all(&page).unwrap();
        }
        archive.finish().unwrap();

        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
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
        let media = analyze_book(&book, false, || false).unwrap();
        storage.replace_media_analysis(&media).await.unwrap();
        let state = AppState::new(storage.clone(), tasks.clone())
            .await
            .unwrap()
            .with_page_hashing(NonZeroUsize::new(2).unwrap());

        library.hash_pages = false;
        storage.update_library(&library).await.unwrap();
        assert_eq!(
            process_hash_book_pages_task(&state, &TaskRecord::hash_book_pages(&book.id, 1)).await,
            TaskOutcome::Completed
        );
        assert!(
            storage
                .find_media_by_book_id(&book.id)
                .await
                .unwrap()
                .unwrap()
                .pages
                .iter()
                .all(|page| page.file_hash.is_empty())
        );
        library.hash_pages = true;
        storage.update_library(&library).await.unwrap();

        let discovery = TaskRecord::find_books_with_missing_page_hash(&library.id, 0);
        assert_eq!(
            process_find_missing_page_hashes_task(&state, &discovery).await,
            TaskOutcome::Completed
        );
        let queued = tasks.list().await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, format!("HASH_BOOK_PAGES_{}", book.id));
        assert_eq!(queued[0].priority, 1);

        assert_eq!(
            process_hash_book_pages_task(&state, &queued[0]).await,
            TaskOutcome::Completed
        );
        let persisted = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(persisted.pages.len(), 8);
        assert!(persisted.pages[..2].iter().all(|page| !page.file_hash.is_empty()));
        assert!(persisted.pages[2..6].iter().all(|page| page.file_hash.is_empty()));
        assert!(persisted.pages[6..].iter().all(|page| !page.file_hash.is_empty()));
        assert!(
            storage
                .list_page_hash_targets(&book.id, 2, RGB_PAGE_HASH_ALGORITHM)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scan_always_enqueues_deduplicated_page_hash_discoveries() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let state = AppState::new(storage, tasks.clone()).await.unwrap();
        let scan = TaskRecord::scan_library(&library.id, false, 4);

        assert_eq!(
            super::super::process_scan_task(&state, &scan).await,
            TaskOutcome::Completed
        );
        assert_eq!(
            super::super::process_scan_task(&state, &scan).await,
            TaskOutcome::Completed
        );
        let queued = tasks.list().await.unwrap();
        assert_eq!(queued.len(), 2);
        assert!(queued.iter().all(|task| task.priority == 0));
        assert!(
            queued
                .iter()
                .any(|task| task.id == format!("FIND_BOOKS_WITH_MISSING_PAGE_HASH_{}", library.id))
        );
        assert!(
            queued
                .iter()
                .any(|task| task.id == format!("FIND_DUPLICATE_PAGES_TO_DELETE_{}", library.id))
        );
    }

    #[tokio::test]
    async fn pdf_hash_task_hashes_rendered_jpeg_edges_with_rgb_algorithm() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        fs::write(
            series.join("Volume.pdf"),
            blank_pdf(&[(100.0, 160.0), (120.0, 160.0), (140.0, 160.0)]),
        )
        .unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
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
        let media = analyze_book(&book, true, || false).unwrap();
        storage.replace_media_analysis(&media).await.unwrap();
        let state = AppState::new(storage.clone(), tasks.clone())
            .await
            .unwrap()
            .with_page_hashing(NonZeroUsize::new(1).unwrap());

        assert_eq!(
            process_find_missing_page_hashes_task(
                &state,
                &TaskRecord::find_books_with_missing_page_hash(&library.id, 0),
            )
            .await,
            TaskOutcome::Completed
        );
        let task = tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.simple_type == "HashBookPages")
            .unwrap();
        assert_eq!(
            process_hash_book_pages_task(&state, &task).await,
            TaskOutcome::Completed
        );

        let persisted = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert!(!persisted.pages[0].file_hash.is_empty());
        assert!(persisted.pages[1].file_hash.is_empty());
        assert!(!persisted.pages[2].file_hash.is_empty());
        assert!(
            storage
                .list_page_hash_targets(&book.id, 1, RGB_PAGE_HASH_ALGORITHM)
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn blank_pdf(page_sizes: &[(f32, f32)]) -> Vec<u8> {
        let mut pdf = pdf_writer::Pdf::new();
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page_refs = (0..page_sizes.len())
            .map(|index| Ref::new(i32::try_from(index + 3).unwrap()))
            .collect::<Vec<_>>();
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages)
            .kids(page_refs.iter().copied())
            .count(i32::try_from(page_refs.len()).unwrap());
        for (page_ref, (width, height)) in page_refs.into_iter().zip(page_sizes.iter().copied()) {
            let mut page = pdf.page(page_ref);
            page.parent(pages);
            page.media_box(Rect::new(0.0, 0.0, width, height));
            page.resources();
            page.finish();
        }
        pdf.finish()
    }
}
