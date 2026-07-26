use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Local};
use komga_core::{Book, HashedPageTarget, Library, Media, PDF_MEDIA_TYPE, PageHashAction, ZIP_MEDIA_TYPE};
use komga_media::{PageRemovalError, PageRemovalRewrite, rewrite_hashed_pdf_pages, rewrite_hashed_zip_pages};

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
    let recovery_book = book.clone();
    let recovery_library = library.clone();
    let recovered =
        tokio::task::spawn_blocking(move || recover_page_removal_artifacts(&recovery_book, &recovery_library)).await;
    match recovered {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            enqueue_recovery_scan(state, &book.library_id).await;
            return work_failure("page-removal recovery", &book.id, error);
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "page-removal recovery panicked");
            return TaskOutcome::Retry;
        }
    }
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
    let work_library = library.clone();
    let analyze_dimensions = library.analyze_dimensions;
    let cancellation = state.shutdown.clone();
    let targets = payload.pages;
    let result = tokio::task::spawn_blocking(move || {
        prepare_page_removal(
            &work_book,
            &work_media,
            &work_library,
            &targets,
            analyze_dimensions,
            || cancellation.is_cancelled(),
        )
    })
    .await;
    let mut prepared = match result {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => {
            if matches!(error.outcome, TaskOutcome::Completed) {
                enqueue_recovery_scan(state, &book.library_id).await;
            }
            return work_failure("duplicate-page removal", &book.id, error);
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "duplicate-page rewrite panicked");
            return TaskOutcome::Retry;
        }
    };

    match state
        .storage
        .apply_page_removal_if_current(
            &book,
            &media,
            &prepared.rewrite.book,
            &prepared.rewrite.media,
            &prepared.rewrite.removed,
            &prepared.rewrite.retained_original_indices,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(book_id = %book.id, "database changed after duplicate-page source replacement");
            if let Err(error) = prepared.rollback() {
                tracing::error!(message = %error.message, book_id = %book.id, "could not roll back page-removal source");
            }
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist duplicate-page removal");
            if let Err(error) = prepared.rollback() {
                tracing::error!(message = %error.message, book_id = %book.id, "could not roll back page-removal source");
            }
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Retry;
        }
    }
    prepared.finish();
    let removal = &prepared.rewrite;
    state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(&removal.book)));

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
    if library.hash_koreader
        && let Err(error) = state
            .enqueue_task(&TaskRecord::hash_book_koreader(
                &book.id,
                task.priority.saturating_add(1),
            ))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue KOReader hashing after page removal");
        enqueue_recovery_scan(state, &book.library_id).await;
        return TaskOutcome::Retry;
    }
    if !state.sync_books(std::slice::from_ref(&book.id)).await {
        state.recover_search_after_http_failure().await;
    }
    tracing::info!(book_id = %book.id, pages = removal.removed.len(), "duplicate pages removed");
    TaskOutcome::Completed
}

#[derive(Debug)]
struct PreparedPageRemoval {
    rewrite: PageRemovalRewrite,
    source: PathBuf,
    temporary: PathBuf,
    backup: PathBuf,
}

impl PreparedPageRemoval {
    fn rollback(&mut self) -> Result<(), WorkError> {
        if path_exists(&self.source)? {
            validate_regular_file(&self.source, None)?;
            fs::remove_file(&self.source).map_err(WorkError::retry)?;
        }
        if !path_exists(&self.backup)? {
            return Err(WorkError::terminal("page-removal backup is missing during rollback"));
        }
        fs::rename(&self.backup, &self.source).map_err(WorkError::retry)?;
        if let Some(parent) = self.source.parent() {
            sync_directory(parent).map_err(WorkError::retry)?;
        }
        remove_regular_file(&self.temporary)?;
        Ok(())
    }

    fn finish(&mut self) {
        if let Err(error) = remove_regular_file(&self.backup) {
            tracing::warn!(message = %error.message, path = %self.backup.display(), "could not remove page-removal backup");
        }
        if let Err(error) = remove_regular_file(&self.temporary) {
            tracing::warn!(message = %error.message, path = %self.temporary.display(), "could not remove page-removal temporary file");
        }
        if let Some(parent) = self.source.parent()
            && let Err(error) = sync_directory(parent)
        {
            tracing::warn!(%error, path = %parent.display(), "could not sync page-removal directory");
        }
    }
}

fn prepare_page_removal(
    book: &Book,
    media: &Media,
    library: &Library,
    targets: &[HashedPageTarget],
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<PreparedPageRemoval, WorkError> {
    let (root, source, parent, temporary, backup) = validated_page_removal_paths(book, library)?;
    validate_regular_file(&source, Some(&root))?;
    if !matches_book_snapshot(&source, book)? {
        return Err(WorkError::terminal("book source snapshot has changed"));
    }
    ensure_absent(&temporary, "page-removal temporary file")?;
    ensure_absent(&backup, "page-removal backup")?;

    let rewritten = match media.media_type.as_deref() {
        Some(ZIP_MEDIA_TYPE) => {
            rewrite_hashed_zip_pages(book, media, targets, &temporary, analyze_dimensions, &cancelled)
        }
        Some(PDF_MEDIA_TYPE) => {
            rewrite_hashed_pdf_pages(book, media, targets, &temporary, analyze_dimensions, &cancelled)
        }
        _ => {
            return Err(WorkError::terminal("only ready ZIP and PDF media support page removal"));
        }
    };
    let rewrite = match rewritten {
        Ok(rewrite) => rewrite,
        Err(error) => {
            let _ = remove_regular_file(&temporary);
            return Err(WorkError::from(error));
        }
    };
    validate_regular_file(&temporary, Some(&root))?;
    if cancelled() {
        remove_regular_file(&temporary)?;
        return Err(WorkError::cancelled("page removal cancelled before file replacement"));
    }

    fs::rename(&source, &backup).map_err(WorkError::retry)?;
    sync_directory(&parent).map_err(WorkError::retry)?;
    if let Err(error) = fs::rename(&temporary, &source) {
        let _ = fs::rename(&backup, &source);
        let _ = sync_directory(&parent);
        return Err(WorkError::retry(error));
    }
    sync_directory(&parent).map_err(WorkError::retry)?;
    if !matches_book_snapshot(&source, &rewrite.book)? {
        let _ = fs::remove_file(&source);
        let _ = fs::rename(&backup, &source);
        let _ = sync_directory(&parent);
        return Err(WorkError::terminal(
            "rewritten source does not match its validated snapshot",
        ));
    }
    Ok(PreparedPageRemoval {
        rewrite,
        source,
        temporary,
        backup,
    })
}

fn recover_page_removal_artifacts(book: &Book, library: &Library) -> Result<(), WorkError> {
    let (root, source, parent, temporary, backup) = validated_page_removal_paths(book, library)?;
    let source_exists = path_exists(&source)?;
    let backup_exists = path_exists(&backup)?;
    let temporary_exists = path_exists(&temporary)?;
    if source_exists {
        validate_regular_file(&source, Some(&root))?;
    }
    if backup_exists {
        validate_regular_file(&backup, Some(&root))?;
    }
    if temporary_exists {
        validate_regular_file(&temporary, Some(&root))?;
    }

    if backup_exists {
        let source_matches = source_exists && matches_book_snapshot(&source, book)?;
        let backup_matches = matches_book_snapshot(&backup, book)?;
        match (source_exists, source_matches, backup_matches) {
            (true, true, false) => {
                fs::remove_file(&backup).map_err(WorkError::retry)?;
                remove_regular_file(&temporary)?;
            }
            (true, false, true) => {
                fs::remove_file(&source).map_err(WorkError::retry)?;
                fs::rename(&backup, &source).map_err(WorkError::retry)?;
                remove_regular_file(&temporary)?;
            }
            (false, false, true) => {
                fs::rename(&backup, &source).map_err(WorkError::retry)?;
                remove_regular_file(&temporary)?;
            }
            _ => {
                return Err(WorkError::terminal(
                    "page-removal artifacts cannot be matched safely to the database snapshot",
                ));
            }
        }
        sync_directory(&parent).map_err(WorkError::retry)?;
        return Ok(());
    }

    if !source_exists || !matches_book_snapshot(&source, book)? {
        return Err(WorkError::terminal(
            "current page-removal source does not match the database snapshot",
        ));
    }
    if temporary_exists {
        fs::remove_file(&temporary).map_err(WorkError::retry)?;
        sync_directory(&parent).map_err(WorkError::retry)?;
    }
    Ok(())
}

fn validated_page_removal_paths(
    book: &Book,
    library: &Library,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf, PathBuf), WorkError> {
    let source = book
        .url
        .to_file_path()
        .map_err(|()| WorkError::terminal("book URL is not a local file"))?;
    let root_path = library
        .root_path()
        .ok_or_else(|| WorkError::terminal("library root is not a local directory"))?;
    let root = fs::canonicalize(root_path).map_err(WorkError::retry)?;
    if !root.is_dir() {
        return Err(WorkError::terminal("library root is not a directory"));
    }
    let parent = source
        .parent()
        .ok_or_else(|| WorkError::terminal("book source has no parent directory"))?
        .to_owned();
    let parent_metadata = fs::symlink_metadata(&parent).map_err(WorkError::retry)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(WorkError::terminal("book parent is not a non-symlink directory"));
    }
    let real_parent = fs::canonicalize(&parent).map_err(WorkError::retry)?;
    if !real_parent.starts_with(&root) {
        return Err(WorkError::terminal("book parent resolves outside the library root"));
    }
    let temporary = parent.join(format!(".komga-page-remove-{}.tmp", book.id));
    let backup = parent.join(format!(".komga-page-remove-{}.bak", book.id));
    ensure_managed_path(&source, &parent, &root)?;
    ensure_managed_path(&temporary, &parent, &root)?;
    ensure_managed_path(&backup, &parent, &root)?;
    Ok((root, source, parent, temporary, backup))
}

fn ensure_managed_path(path: &Path, parent: &Path, root: &Path) -> Result<(), WorkError> {
    let real_parent = fs::canonicalize(parent).map_err(WorkError::retry)?;
    if path.parent() != Some(parent) || path.file_name().is_none() || !real_parent.starts_with(root) {
        return Err(WorkError::terminal("page-removal path is outside the library root"));
    }
    Ok(())
}

fn validate_regular_file(path: &Path, root: Option<&Path>) -> Result<(), WorkError> {
    let metadata = fs::symlink_metadata(path).map_err(WorkError::retry)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkError::terminal(format!(
            "page-removal path is not a non-symlink regular file: {}",
            path.display()
        )));
    }
    if let Some(root) = root {
        let real = fs::canonicalize(path).map_err(WorkError::retry)?;
        if !real.starts_with(root) {
            return Err(WorkError::terminal(format!(
                "page-removal path resolves outside the library root: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn matches_book_snapshot(path: &Path, book: &Book) -> Result<bool, WorkError> {
    let metadata = fs::metadata(path).map_err(WorkError::retry)?;
    Ok(i64::try_from(metadata.len()).unwrap_or(i64::MAX) == book.file_size
        && updated_time(&metadata) == book.file_last_modified)
}

fn updated_time(metadata: &fs::Metadata) -> chrono::NaiveDateTime {
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    DateTime::<Local>::from(modified.max(created)).naive_local()
}

fn ensure_absent(path: &Path, label: &str) -> Result<(), WorkError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WorkError::retry(error)),
        Ok(_) => Err(WorkError::terminal(format!(
            "{label} already exists: {}",
            path.display()
        ))),
    }
}

fn path_exists(path: &Path) -> Result<bool, WorkError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(WorkError::retry(error)),
    }
}

fn remove_regular_file(path: &Path) -> Result<(), WorkError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path).map_err(WorkError::retry)
        }
        Ok(_) => Err(WorkError::terminal(format!(
            "refusing to remove non-regular page-removal artifact: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(WorkError::retry(error)),
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[derive(Debug)]
struct WorkError {
    message: String,
    outcome: TaskOutcome,
}

impl WorkError {
    fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome: TaskOutcome::Completed,
        }
    }

    fn retry(error: impl std::fmt::Display) -> Self {
        Self {
            message: error.to_string(),
            outcome: TaskOutcome::Retry,
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome: TaskOutcome::Cancelled,
        }
    }
}

impl From<PageRemovalError> for WorkError {
    fn from(error: PageRemovalError) -> Self {
        match error {
            PageRemovalError::Cancelled => Self::cancelled(error.to_string()),
            PageRemovalError::Io(_) => Self::retry(error),
            PageRemovalError::Stale(_)
            | PageRemovalError::Unsupported(_)
            | PageRemovalError::Archive(_)
            | PageRemovalError::Pdf(_)
            | PageRemovalError::Hash(_)
            | PageRemovalError::Validation(_) => Self::terminal(error.to_string()),
        }
    }
}

fn work_failure(operation: &str, book_id: &str, error: WorkError) -> TaskOutcome {
    match error.outcome {
        TaskOutcome::Retry => {
            tracing::error!(message = %error.message, %book_id, %operation, "file operation will be retried")
        }
        TaskOutcome::Cancelled => tracing::info!(%book_id, %operation, "file operation was cancelled"),
        TaskOutcome::Completed => {
            tracing::warn!(message = %error.message, %book_id, %operation, "file operation was rejected")
        }
    }
    error.outcome
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
    use pdf_writer::{Content, Finish, Rect, Ref};
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
        library.hash_koreader = true;
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
        assert!(queued.iter().any(|value| value == "HashBookKoreader"));
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

    #[tokio::test]
    async fn pdf_removal_reorders_pages_preserves_hashes_and_enqueues_followups() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let path = series.join("Volume.pdf");
        fs::write(&path, colored_pdf(2)).unwrap();

        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.hash_files = true;
        library.hash_pages = true;
        library.hash_koreader = true;
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
        let analyzed = analyze_book(&book, true, || false).unwrap();
        storage.replace_media_analysis(&analyzed).await.unwrap();
        let book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let updates = media
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let content = extract_page(&book, &media, i32::try_from(index + 1).unwrap()).unwrap();
                let computed = hash_page(&content.bytes, &content.media_type).unwrap();
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
        let removed_hash = media.pages[0].file_hash.clone();
        let retained_hash = media.pages[1].file_hash.clone();
        storage
            .create_or_update_page_hash(&removed_hash, None, PageHashAction::DeleteManual, None)
            .await
            .unwrap();
        let task = TaskRecord::remove_hashed_pages(
            &book.id,
            &[HashedPageTarget {
                file_name: "1".into(),
                media_type: komga_core::JPEG_MEDIA_TYPE.into(),
                file_hash: removed_hash.clone(),
                file_size: 0,
                page_number: 1,
            }],
            4,
        );
        let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();

        assert_eq!(
            process_remove_hashed_pages_task(&state, &task).await,
            TaskOutcome::Completed
        );

        let saved = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved.pages.len(), 1);
        assert_eq!(saved.pages[0].file_name, "1");
        assert_eq!(saved.pages[0].file_hash, retained_hash);
        assert_eq!(analyze_book(&saved_book, true, || false).unwrap().page_count, 1);
        assert_eq!(
            storage
                .find_known_page_hash(&removed_hash)
                .await
                .unwrap()
                .unwrap()
                .delete_count,
            1
        );
        let queued = tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|task| task.simple_type)
            .collect::<Vec<_>>();
        for expected in ["GenerateBookThumbnail", "HashBookPages", "HashBook", "HashBookKoreader"] {
            assert!(queued.iter().any(|value| value == expected), "missing {expected}");
        }
        assert!(!series.join(format!(".komga-page-remove-{}.tmp", book.id)).exists());
        assert!(!series.join(format!(".komga-page-remove-{}.bak", book.id)).exists());
    }

    #[test]
    fn recovery_restores_or_finishes_artifacts_from_the_database_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());

        let source = root.join("only-backup.cbz");
        fs::write(&source, b"original").unwrap();
        let book = book_for_path(&source, &library);
        let (temporary_path, backup) = artifact_paths(&book);
        fs::rename(&source, &backup).unwrap();
        fs::write(&temporary_path, b"new").unwrap();
        recover_page_removal_artifacts(&book, &library).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert!(!temporary_path.exists() && !backup.exists());

        let source = root.join("switched.cbz");
        fs::write(&source, b"old-source").unwrap();
        let book = book_for_path(&source, &library);
        let (_, backup) = artifact_paths(&book);
        fs::rename(&source, &backup).unwrap();
        fs::write(&source, b"uncommitted-new-source").unwrap();
        recover_page_removal_artifacts(&book, &library).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"old-source");
        assert!(!backup.exists());

        let source = root.join("committed.cbz");
        fs::write(&source, b"old").unwrap();
        let old_book = book_for_path(&source, &library);
        let (temporary_path, backup) = artifact_paths(&old_book);
        fs::rename(&source, &backup).unwrap();
        fs::write(&source, b"committed-new-source").unwrap();
        fs::write(&temporary_path, b"leftover").unwrap();
        let committed_book = book_for_path_with_id(&source, &library, &old_book.id);
        recover_page_removal_artifacts(&committed_book, &library).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"committed-new-source");
        assert!(!temporary_path.exists() && !backup.exists());
    }

    #[test]
    fn recovery_preserves_conflicts_and_rejects_paths_outside_the_library() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        let source = root.join("conflict.cbz");
        fs::write(&source, b"database-snapshot").unwrap();
        let book = book_for_path(&source, &library);
        let (_, backup) = artifact_paths(&book);
        fs::rename(&source, &backup).unwrap();
        fs::write(&backup, b"changed-backup").unwrap();
        fs::write(&source, b"changed-source-with-another-size").unwrap();

        assert!(recover_page_removal_artifacts(&book, &library).is_err());
        assert!(source.exists() && backup.exists());

        let outside = temporary.path().join("outside.cbz");
        fs::write(&outside, b"outside").unwrap();
        let outside_book = book_for_path(&outside, &library);
        assert!(recover_page_removal_artifacts(&outside_book, &library).is_err());
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlink_sources_and_artifacts_without_removing_them() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        let real = root.join("real.cbz");
        let source = root.join("linked.cbz");
        fs::write(&real, b"source").unwrap();
        symlink(&real, &source).unwrap();
        let book = book_for_path(&source, &library);
        assert!(recover_page_removal_artifacts(&book, &library).is_err());
        assert!(fs::symlink_metadata(&source).unwrap().file_type().is_symlink());

        let regular = root.join("regular.cbz");
        fs::write(&regular, b"regular").unwrap();
        let book = book_for_path(&regular, &library);
        let (temporary_path, _) = artifact_paths(&book);
        symlink(&real, &temporary_path).unwrap();
        assert!(recover_page_removal_artifacts(&book, &library).is_err());
        assert!(fs::symlink_metadata(&temporary_path).unwrap().file_type().is_symlink());
    }

    fn colored_pdf(page_count: usize) -> Vec<u8> {
        let mut output = pdf_writer::Pdf::new();
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page_refs = (0..page_count)
            .map(|index| Ref::new(i32::try_from(index * 2 + 3).unwrap()))
            .collect::<Vec<_>>();
        let content_refs = (0..page_count)
            .map(|index| Ref::new(i32::try_from(index * 2 + 4).unwrap()))
            .collect::<Vec<_>>();
        output.catalog(catalog).pages(pages);
        output
            .pages(pages)
            .kids(page_refs.iter().copied())
            .count(i32::try_from(page_count).unwrap());
        for (index, (page_ref, content_ref)) in page_refs.into_iter().zip(content_refs).enumerate() {
            let mut page = output.page(page_ref);
            page.parent(pages);
            page.media_box(Rect::new(0.0, 0.0, 120.0 + index as f32 * 10.0, 180.0));
            page.contents(content_ref);
            page.resources();
            page.finish();
            let mut content = Content::new();
            content.set_fill_rgb(index as f32, 0.2, 1.0 - index as f32);
            content.rect(10.0, 10.0, 80.0, 120.0);
            content.fill_nonzero();
            output.stream(content_ref, &content.finish());
        }
        output.finish()
    }

    fn artifact_paths(book: &Book) -> (PathBuf, PathBuf) {
        let source = book.url.to_file_path().unwrap();
        let parent = source.parent().unwrap();
        (
            parent.join(format!(".komga-page-remove-{}.tmp", book.id)),
            parent.join(format!(".komga-page-remove-{}.bak", book.id)),
        )
    }

    fn book_for_path(path: &Path, library: &Library) -> Book {
        book_for_path_with_id(path, library, &komga_core::new_id())
    }

    fn book_for_path_with_id(path: &Path, library: &Library, id: &str) -> Book {
        let metadata = fs::metadata(path).unwrap();
        Book {
            id: id.into(),
            name: "Book".into(),
            url: Url::from_file_path(path).unwrap(),
            file_last_modified: updated_time(&metadata),
            file_size: i64::try_from(metadata.len()).unwrap(),
            file_hash: String::new(),
            file_hash_koreader: String::new(),
            number: 1,
            series_id: "series".into(),
            library_id: library.id.clone(),
            deleted_date: None,
            oneshot: false,
            created_date: None,
            last_modified_date: None,
        }
    }
}
