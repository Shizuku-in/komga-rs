use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Local};
use komga_core::{
    Book, EPUB_MEDIA_TYPE, HashedPageTarget, Library, Media, MediaStatus, PDF_MEDIA_TYPE, PageHashAction,
    RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, ZIP_MEDIA_TYPE,
};
use komga_media::{
    FileHashError, PageRemovalError, PageRemovalRewrite, hash_file, rewrite_hashed_epub_pages,
    rewrite_hashed_pdf_pages, rewrite_hashed_zip_pages,
};
use serde::{Deserialize, Serialize};

use super::*;

const PAGE_REMOVAL_JOURNAL_VERSION: u32 = 1;
const MAX_PAGE_REMOVAL_JOURNAL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageRemovalJournal {
    version: u32,
    book_id: String,
    source_file_name: String,
    media_type: String,
    old: PageRemovalJournalSnapshot,
    new: PageRemovalJournalSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PageRemovalJournalSnapshot {
    file_size: i64,
    file_last_modified: chrono::NaiveDateTime,
    page_count: i32,
    fingerprint: String,
}

#[derive(Debug)]
struct PageRemovalPaths {
    root: PathBuf,
    source: PathBuf,
    parent: PathBuf,
    temporary: PathBuf,
    backup: PathBuf,
    journal: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalDatabaseState {
    Old,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JournalFileState {
    Missing,
    Old,
    New,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageRemovalRecovery {
    pub(super) deferred: bool,
}

pub(super) async fn recover_library_page_removal_artifacts(
    state: &AppState,
    library: &Library,
    phase: &'static str,
    allow_legacy_deferred: bool,
) -> Result<bool, TaskOutcome> {
    let books = state
        .storage
        .list_active_books_by_library_id(&library.id)
        .await
        .map_err(|error| {
            tracing::error!(%error, library_id = %library.id, phase, "could not list books for page-removal recovery");
            TaskOutcome::Retry
        })?;
    let mut deferred = false;
    for book in books {
        let has_artifacts = page_removal_artifacts_exist(&book).map_err(|error| {
            tracing::error!(message = %error.message, book_id = %book.id, phase, "could not inspect page-removal artifacts");
            TaskOutcome::Retry
        })?;
        if !has_artifacts {
            continue;
        }
        let media = state
            .storage
            .find_media_by_book_id(&book.id)
            .await
            .map_err(|error| {
                tracing::error!(%error, book_id = %book.id, phase, "could not load media for page-removal recovery");
                TaskOutcome::Retry
            })?
            .ok_or_else(|| {
                tracing::error!(book_id = %book.id, phase, "page-removal artifacts have no database media snapshot");
                TaskOutcome::Retry
            })?;
        let recovery_book = book.clone();
        let recovery_library = library.clone();
        let recovered = tokio::task::spawn_blocking(move || {
            recover_page_removal_artifacts(&recovery_book, &media, &recovery_library, phase, allow_legacy_deferred)
        })
        .await;
        match recovered {
            Ok(Ok(recovery)) => deferred |= recovery.deferred,
            Ok(Err(error)) => {
                tracing::error!(
                    message = %error.message,
                    book_id = %book.id,
                    phase,
                    "page-removal recovery blocked the deep scan"
                );
                return Err(TaskOutcome::Retry);
            }
            Err(error) => {
                tracing::error!(%error, book_id = %book.id, phase, "page-removal recovery panicked");
                return Err(TaskOutcome::Retry);
            }
        }
    }
    Ok(deferred)
}

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
    let source_mutation_guard = state.source_mutation_lock(&payload.book_id).lock().await;
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
    if media.status != MediaStatus::Ready
        || !(matches!(
            media.media_type.as_deref(),
            Some(ZIP_MEDIA_TYPE | RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE | PDF_MEDIA_TYPE)
        ) || (media.media_type.as_deref() == Some(EPUB_MEDIA_TYPE) && media.epub_divina_compatible))
    {
        tracing::warn!(book_id = %book.id, media_type = ?media.media_type, "page removal is unsupported for media");
        return TaskOutcome::Completed;
    }
    if matches!(media.media_type.as_deref(), Some(RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE)) {
        let removal =
            match super::conversion::perform_rar_page_removal(state, &book, &media, &library, &payload.pages).await {
                Ok(removal) => removal,
                Err(outcome) => return outcome,
            };
        drop(source_mutation_guard);
        return complete_page_removal(state, task, &book, &library, &removal).await;
    }
    let recovery_book = book.clone();
    let recovery_media = media.clone();
    let recovery_library = library.clone();
    let recovered = tokio::task::spawn_blocking(move || {
        recover_page_removal_artifacts(&recovery_book, &recovery_media, &recovery_library, "task-start", false)
    })
    .await;
    match recovered {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            enqueue_recovery_scan(state, &book.library_id).await;
            return work_failure("page-removal recovery", &book.id, error);
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "page-removal recovery panicked");
            return TaskOutcome::Retry;
        }
    }
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
    let prepared = match result {
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
            if let Err(error) =
                recover_page_removal_from_current_database(state, &book.id, &library, "database-mismatch").await
            {
                tracing::error!(message = %error.message, book_id = %book.id, "could not roll back page-removal source");
            }
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist duplicate-page removal");
            if let Err(error) =
                recover_page_removal_from_current_database(state, &book.id, &library, "database-error").await
            {
                tracing::error!(message = %error.message, book_id = %book.id, "could not roll back page-removal source");
            }
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Retry;
        }
    }
    let cleanup_book = prepared.rewrite.book.clone();
    let cleanup_media = prepared.rewrite.media.clone();
    let cleanup_library = library.clone();
    match tokio::task::spawn_blocking(move || {
        recover_page_removal_artifacts(&cleanup_book, &cleanup_media, &cleanup_library, "commit", false)
    })
    .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(message = %error.message, book_id = %book.id, "could not clean committed page-removal artifacts");
            enqueue_recovery_scan(state, &book.library_id).await;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "committed page-removal cleanup panicked");
            enqueue_recovery_scan(state, &book.library_id).await;
        }
    }
    let removal = prepared.rewrite;
    drop(source_mutation_guard);
    complete_page_removal(state, task, &book, &library, &removal).await
}

async fn complete_page_removal(
    state: &AppState,
    task: &TaskRecord,
    book: &Book,
    library: &Library,
    removal: &PageRemovalRewrite,
) -> TaskOutcome {
    state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(&removal.book)));
    let series_changed = removal.book.oneshot && removal.book.url != book.url;
    if series_changed {
        state.publish_domain_event(DomainEvent::SeriesUpdated(SeriesEvent {
            series_id: removal.book.series_id.clone(),
            library_id: removal.book.library_id.clone(),
        }));
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
    let books_synced = state.sync_books(std::slice::from_ref(&book.id)).await;
    let series_synced = !series_changed || state.sync_series(std::slice::from_ref(&book.series_id)).await;
    if !books_synced || !series_synced {
        state.recover_search_after_http_failure().await;
    }
    tracing::info!(book_id = %book.id, pages = removal.removed.len(), "duplicate pages removed");
    TaskOutcome::Completed
}

async fn recover_page_removal_from_current_database(
    state: &AppState,
    book_id: &str,
    library: &Library,
    phase: &'static str,
) -> Result<(), WorkError> {
    let book = state
        .storage
        .find_book_by_id(book_id)
        .await
        .map_err(WorkError::retry)?
        .ok_or_else(|| WorkError::terminal("page-removal database book disappeared"))?;
    let media = state
        .storage
        .find_media_by_book_id(book_id)
        .await
        .map_err(WorkError::retry)?
        .ok_or_else(|| WorkError::terminal("page-removal database media disappeared"))?;
    let library = library.clone();
    tokio::task::spawn_blocking(move || {
        recover_page_removal_artifacts(&book, &media, &library, phase, false).map(|_| ())
    })
    .await
    .map_err(WorkError::retry)?
}

#[derive(Debug)]
struct PreparedPageRemoval {
    rewrite: PageRemovalRewrite,
}

fn prepare_page_removal(
    book: &Book,
    media: &Media,
    library: &Library,
    targets: &[HashedPageTarget],
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<PreparedPageRemoval, WorkError> {
    let paths = validated_page_removal_paths(book, library)?;
    validate_regular_file(&paths.source, Some(&paths.root))?;
    if !matches_book_snapshot(&paths.source, book)? {
        return Err(WorkError::terminal("book source snapshot has changed"));
    }
    ensure_absent(&paths.temporary, "page-removal temporary file")?;
    ensure_absent(&paths.backup, "page-removal backup")?;
    ensure_absent(&paths.journal, "page-removal journal")?;

    let rewritten = match media.media_type.as_deref() {
        Some(ZIP_MEDIA_TYPE) => {
            rewrite_hashed_zip_pages(book, media, targets, &paths.temporary, analyze_dimensions, &cancelled)
        }
        Some(PDF_MEDIA_TYPE) => {
            rewrite_hashed_pdf_pages(book, media, targets, &paths.temporary, analyze_dimensions, &cancelled)
        }
        Some(EPUB_MEDIA_TYPE) if media.epub_divina_compatible => {
            rewrite_hashed_epub_pages(book, media, targets, &paths.temporary, analyze_dimensions, &cancelled)
        }
        _ => {
            return Err(WorkError::terminal(
                "only ready ZIP, PDF, and Divina-compatible EPUB media support page removal",
            ));
        }
    };
    let rewrite = match rewritten {
        Ok(rewrite) => rewrite,
        Err(error) => {
            let _ = remove_regular_file(&paths.temporary);
            return Err(WorkError::from(error));
        }
    };
    validate_regular_file(&paths.temporary, Some(&paths.root))?;
    let journal = PageRemovalJournal::new(book, media, &rewrite, &paths)?;
    write_page_removal_journal(&paths, &journal)?;
    if cancelled() {
        remove_regular_file_synced(&paths.temporary, &paths.parent)?;
        remove_regular_file_synced(&paths.journal, &paths.parent)?;
        return Err(WorkError::cancelled("page removal cancelled before file replacement"));
    }

    rename_synced(&paths.source, &paths.backup, &paths.parent)?;
    rename_synced(&paths.temporary, &paths.source, &paths.parent)?;
    if !matches_book_snapshot(&paths.source, &rewrite.book)?
        || journal_file_state(&paths.source, &paths.root, &journal)? != JournalFileState::New
    {
        return Err(WorkError::terminal(
            "rewritten source does not match its validated snapshot",
        ));
    }
    Ok(PreparedPageRemoval { rewrite })
}

pub(super) fn recover_page_removal_artifacts(
    book: &Book,
    media: &Media,
    library: &Library,
    phase: &'static str,
    allow_legacy_deferred: bool,
) -> Result<PageRemovalRecovery, WorkError> {
    let paths = validated_page_removal_paths(book, library)?;
    let backup_exists = path_exists(&paths.backup)?;
    let temporary_exists = path_exists(&paths.temporary)?;
    let journal_exists = path_exists(&paths.journal)?;
    if !backup_exists && !temporary_exists && !journal_exists {
        if path_exists(&paths.source)? {
            validate_regular_file(&paths.source, Some(&paths.root))?;
        }
        return Ok(PageRemovalRecovery { deferred: false });
    }
    let (action, journal_version, deferred) = if journal_exists {
        let journal = read_page_removal_journal(&paths)?;
        let state = journal_database_state(book, media, &journal)?;
        (
            recover_journal_artifacts(&paths, &journal, state)?,
            Some(journal.version),
            false,
        )
    } else {
        let recovery = recover_legacy_artifacts(book, &paths, allow_legacy_deferred)?;
        (recovery.0, None, recovery.1)
    };
    tracing::info!(
        book_id = %book.id,
        phase,
        journal_version,
        recovery_action = action,
        "page-removal artifacts inspected"
    );
    Ok(PageRemovalRecovery { deferred })
}

fn recover_legacy_artifacts(
    book: &Book,
    paths: &PageRemovalPaths,
    allow_deferred: bool,
) -> Result<(&'static str, bool), WorkError> {
    let source_exists = path_exists(&paths.source)?;
    let backup_exists = path_exists(&paths.backup)?;
    let temporary_exists = path_exists(&paths.temporary)?;
    if source_exists {
        validate_regular_file(&paths.source, Some(&paths.root))?;
    }
    if backup_exists {
        validate_regular_file(&paths.backup, Some(&paths.root))?;
    }
    if temporary_exists {
        validate_regular_file(&paths.temporary, Some(&paths.root))?;
    }

    if backup_exists {
        let source_matches = source_exists && matches_book_snapshot(&paths.source, book)?;
        let backup_matches = matches_book_snapshot(&paths.backup, book)?;
        match (source_exists, source_matches, backup_matches) {
            (true, true, false) => {
                remove_regular_file_synced(&paths.backup, &paths.parent)?;
                remove_regular_file_synced(&paths.temporary, &paths.parent)?;
            }
            (true, false, true) => {
                remove_regular_file_synced(&paths.source, &paths.parent)?;
                rename_synced(&paths.backup, &paths.source, &paths.parent)?;
                remove_regular_file_synced(&paths.temporary, &paths.parent)?;
            }
            (false, false, true) => {
                rename_synced(&paths.backup, &paths.source, &paths.parent)?;
                remove_regular_file_synced(&paths.temporary, &paths.parent)?;
            }
            _ if allow_deferred && source_exists => return Ok(("defer-legacy", true)),
            _ => {
                return Err(WorkError::terminal(
                    "page-removal artifacts cannot be matched safely to the database snapshot",
                ));
            }
        }
        return Ok(("restore-or-clean-legacy", false));
    }

    if !source_exists || !matches_book_snapshot(&paths.source, book)? {
        if allow_deferred && source_exists {
            return Ok(("defer-legacy", true));
        }
        return Err(WorkError::terminal(
            "current page-removal source does not match the database snapshot",
        ));
    }
    if temporary_exists {
        remove_regular_file_synced(&paths.temporary, &paths.parent)?;
    }
    Ok(("clean-legacy-temporary", false))
}

fn validated_page_removal_paths(book: &Book, library: &Library) -> Result<PageRemovalPaths, WorkError> {
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
    let journal = parent.join(format!(".komga-page-remove-{}.json", book.id));
    ensure_managed_path(&source, &parent, &root)?;
    ensure_managed_path(&temporary, &parent, &root)?;
    ensure_managed_path(&backup, &parent, &root)?;
    ensure_managed_path(&journal, &parent, &root)?;
    Ok(PageRemovalPaths {
        root,
        source,
        parent,
        temporary,
        backup,
        journal,
    })
}

fn page_removal_artifacts_exist(book: &Book) -> Result<bool, WorkError> {
    let source = book
        .url
        .to_file_path()
        .map_err(|()| WorkError::terminal("book URL is not a local file"))?;
    let parent = source
        .parent()
        .ok_or_else(|| WorkError::terminal("book source has no parent directory"))?;
    for extension in ["tmp", "bak", "json"] {
        let path = parent.join(format!(".komga-page-remove-{}.{extension}", book.id));
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(WorkError::retry(error)),
        }
    }
    Ok(false)
}

impl PageRemovalJournal {
    fn new(
        book: &Book,
        media: &Media,
        rewrite: &PageRemovalRewrite,
        paths: &PageRemovalPaths,
    ) -> Result<Self, WorkError> {
        let source_file_name = paths
            .source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| WorkError::terminal("page-removal source filename is not valid UTF-8"))?
            .to_owned();
        let media_type = media
            .media_type
            .as_deref()
            .ok_or_else(|| WorkError::terminal("page-removal media type is missing"))?
            .to_owned();
        let journal = Self {
            version: PAGE_REMOVAL_JOURNAL_VERSION,
            book_id: book.id.clone(),
            source_file_name,
            media_type,
            old: PageRemovalJournalSnapshot {
                file_size: book.file_size,
                file_last_modified: book.file_last_modified,
                page_count: media.page_count,
                fingerprint: rewrite.source_fingerprint.clone(),
            },
            new: PageRemovalJournalSnapshot {
                file_size: rewrite.book.file_size,
                file_last_modified: rewrite.book.file_last_modified,
                page_count: rewrite.media.page_count,
                fingerprint: rewrite.output_fingerprint.clone(),
            },
        };
        validate_page_removal_journal(&journal, paths)?;
        Ok(journal)
    }
}

fn validate_page_removal_journal(journal: &PageRemovalJournal, paths: &PageRemovalPaths) -> Result<(), WorkError> {
    let source_file_name = paths.source.file_name().and_then(|value| value.to_str());
    let valid_fingerprint = |value: &str| {
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if journal.version != PAGE_REMOVAL_JOURNAL_VERSION
        || journal.book_id.is_empty()
        || source_file_name != Some(journal.source_file_name.as_str())
        || !matches!(
            journal.media_type.as_str(),
            ZIP_MEDIA_TYPE | PDF_MEDIA_TYPE | EPUB_MEDIA_TYPE
        )
        || journal.old.file_size < 0
        || journal.new.file_size < 0
        || journal.old.page_count <= 0
        || journal.new.page_count <= 0
        || journal.new.page_count >= journal.old.page_count
        || !valid_fingerprint(&journal.old.fingerprint)
        || !valid_fingerprint(&journal.new.fingerprint)
        || journal.old.fingerprint == journal.new.fingerprint
    {
        return Err(WorkError::terminal("page-removal journal is invalid or unsupported"));
    }
    Ok(())
}

fn write_page_removal_journal(paths: &PageRemovalPaths, journal: &PageRemovalJournal) -> Result<(), WorkError> {
    let content = serde_json::to_vec(journal)
        .map_err(|error| WorkError::terminal(format!("could not serialize page-removal journal: {error}")))?;
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > MAX_PAGE_REMOVAL_JOURNAL_BYTES {
        return Err(WorkError::terminal("page-removal journal exceeds its size limit"));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&paths.journal)
        .map_err(WorkError::retry)?;
    file.write_all(&content).map_err(WorkError::retry)?;
    file.sync_all().map_err(WorkError::retry)?;
    sync_directory(&paths.parent).map_err(WorkError::retry)
}

fn read_page_removal_journal(paths: &PageRemovalPaths) -> Result<PageRemovalJournal, WorkError> {
    validate_regular_file(&paths.journal, Some(&paths.root))?;
    let metadata = fs::metadata(&paths.journal).map_err(WorkError::retry)?;
    if metadata.len() > MAX_PAGE_REMOVAL_JOURNAL_BYTES {
        return Err(WorkError::terminal("page-removal journal exceeds its size limit"));
    }
    let mut content = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(&paths.journal)
        .map_err(WorkError::retry)?
        .take(MAX_PAGE_REMOVAL_JOURNAL_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(WorkError::retry)?;
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > MAX_PAGE_REMOVAL_JOURNAL_BYTES {
        return Err(WorkError::terminal("page-removal journal exceeds its size limit"));
    }
    let journal = serde_json::from_slice::<PageRemovalJournal>(&content)
        .map_err(|error| WorkError::terminal(format!("page-removal journal is corrupt: {error}")))?;
    if journal.book_id
        != paths
            .journal
            .file_name()
            .and_then(|value| value.to_str())
            .and_then(|name| name.strip_prefix(".komga-page-remove-"))
            .and_then(|name| name.strip_suffix(".json"))
            .unwrap_or_default()
    {
        return Err(WorkError::terminal(
            "page-removal journal book ID conflicts with its path",
        ));
    }
    validate_page_removal_journal(&journal, paths)?;
    Ok(journal)
}

fn journal_database_state(
    book: &Book,
    media: &Media,
    journal: &PageRemovalJournal,
) -> Result<JournalDatabaseState, WorkError> {
    if book.id != journal.book_id || media.media_type.as_deref() != Some(journal.media_type.as_str()) {
        return Err(WorkError::terminal(
            "page-removal journal conflicts with the database book or media",
        ));
    }
    let old = journal_snapshot_matches_database(book, media, &journal.old);
    let new = journal_snapshot_matches_database(book, media, &journal.new);
    match (old, new) {
        (true, false) => Ok(JournalDatabaseState::Old),
        (false, true) => Ok(JournalDatabaseState::New),
        _ => Err(WorkError::terminal(
            "page-removal journal cannot be matched uniquely to the database snapshot",
        )),
    }
}

fn journal_snapshot_matches_database(book: &Book, media: &Media, snapshot: &PageRemovalJournalSnapshot) -> bool {
    book.file_size == snapshot.file_size
        && book.file_last_modified == snapshot.file_last_modified
        && media.page_count == snapshot.page_count
}

fn journal_file_state(path: &Path, root: &Path, journal: &PageRemovalJournal) -> Result<JournalFileState, WorkError> {
    if !path_exists(path)? {
        return Ok(JournalFileState::Missing);
    }
    validate_regular_file(path, Some(root))?;
    let metadata = fs::metadata(path).map_err(WorkError::retry)?;
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    let timestamp = updated_time(&metadata);
    let fingerprint = fingerprint_file(path)?;
    let matches = |snapshot: &PageRemovalJournalSnapshot| {
        size == snapshot.file_size && timestamp == snapshot.file_last_modified && fingerprint == snapshot.fingerprint
    };
    match (matches(&journal.old), matches(&journal.new)) {
        (true, false) => Ok(JournalFileState::Old),
        (false, true) => Ok(JournalFileState::New),
        _ => Ok(JournalFileState::Unknown),
    }
}

fn recover_journal_artifacts(
    paths: &PageRemovalPaths,
    journal: &PageRemovalJournal,
    database: JournalDatabaseState,
) -> Result<&'static str, WorkError> {
    let source = journal_file_state(&paths.source, &paths.root, journal)?;
    let backup = journal_file_state(&paths.backup, &paths.root, journal)?;
    let temporary = journal_file_state(&paths.temporary, &paths.root, journal)?;
    if source == JournalFileState::Unknown
        || !matches!(backup, JournalFileState::Missing | JournalFileState::Old)
        || !matches!(temporary, JournalFileState::Missing | JournalFileState::New)
    {
        return Err(WorkError::terminal(
            "page-removal artifact fingerprint conflicts with its journal role",
        ));
    }

    let action = match database {
        JournalDatabaseState::Old => match (source, backup) {
            (JournalFileState::Old, _) => "keep-old",
            (JournalFileState::New, JournalFileState::Old) => {
                remove_regular_file_synced(&paths.source, &paths.parent)?;
                rename_synced(&paths.backup, &paths.source, &paths.parent)?;
                "restore-old"
            }
            (JournalFileState::Missing, JournalFileState::Old) => {
                rename_synced(&paths.backup, &paths.source, &paths.parent)?;
                "restore-old"
            }
            _ => {
                return Err(WorkError::terminal(
                    "old database snapshot has no matching page-removal source",
                ));
            }
        },
        JournalDatabaseState::New => match (source, temporary) {
            (JournalFileState::New, _) => "keep-new",
            (JournalFileState::Old, JournalFileState::New) => {
                remove_regular_file_synced(&paths.source, &paths.parent)?;
                rename_synced(&paths.temporary, &paths.source, &paths.parent)?;
                "install-new"
            }
            (JournalFileState::Missing, JournalFileState::New) => {
                rename_synced(&paths.temporary, &paths.source, &paths.parent)?;
                "install-new"
            }
            _ => {
                return Err(WorkError::terminal(
                    "new database snapshot has no matching page-removal source",
                ));
            }
        },
    };

    let expected = match database {
        JournalDatabaseState::Old => JournalFileState::Old,
        JournalDatabaseState::New => JournalFileState::New,
    };
    if journal_file_state(&paths.source, &paths.root, journal)? != expected {
        return Err(WorkError::terminal(
            "recovered page-removal source does not match the database fingerprint",
        ));
    }
    remove_regular_file_synced(&paths.temporary, &paths.parent)?;
    remove_regular_file_synced(&paths.backup, &paths.parent)?;
    remove_regular_file_synced(&paths.journal, &paths.parent)?;
    Ok(action)
}

fn fingerprint_file(path: &Path) -> Result<String, WorkError> {
    hash_file(path, || false).map_err(|error| match error {
        FileHashError::Io(error) => WorkError::retry(error),
        FileHashError::Cancelled => WorkError::retry("page-removal artifact hashing was cancelled"),
        FileHashError::InvalidSource(message) | FileHashError::SourceChanged(message) => WorkError::terminal(message),
    })
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

fn remove_regular_file_synced(path: &Path, parent: &Path) -> Result<(), WorkError> {
    let existed = path_exists(path)?;
    remove_regular_file(path)?;
    if existed {
        sync_directory(parent).map_err(WorkError::retry)?;
    }
    Ok(())
}

fn rename_synced(source: &Path, destination: &Path, parent: &Path) -> Result<(), WorkError> {
    fs::rename(source, destination).map_err(WorkError::retry)?;
    sync_directory(parent).map_err(WorkError::retry)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[derive(Debug)]
pub(super) struct WorkError {
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
    use std::{
        fs,
        fs::File,
        io::Write,
        num::NonZeroUsize,
        path::{Path, PathBuf},
    };

    use chrono::Utc;
    use komga_core::{Library, PageHashAction, User, page_progress, scan_library_files};
    use komga_media::{PdfPageRenderer, analyze_book, extract_page, hash_page};
    use komga_search::{EntityKind, SearchMutation};
    use komga_storage::{PageHashUpdate, Storage, TaskStorage};
    use pdf_writer::{Content, Finish, Rect, Ref};
    use url::Url;
    use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

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
    async fn epub_page_removal_updates_source_database_history_and_followups() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let source = series.join("Volume.epub");
        write_fixed_layout_epub(&source);
        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.hash_pages = true;
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let scanned = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        let mut analyzed = analyze_book(&scanned, true, || false).unwrap();
        for index in 0..analyzed.pages.len() {
            let content = extract_page(&scanned, &analyzed, i32::try_from(index + 1).unwrap()).unwrap();
            analyzed.pages[index].file_hash = hash_page(&content.bytes, &content.media_type).unwrap().hash;
        }
        storage.replace_media_analysis(&analyzed).await.unwrap();
        let book = storage.find_book_by_id(&scanned.id).await.unwrap().unwrap();
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let page = &media.pages[1];
        storage
            .create_or_update_page_hash(&page.file_hash, page.file_size, PageHashAction::DeleteManual, None)
            .await
            .unwrap();
        let task = TaskRecord::remove_hashed_pages(
            &book.id,
            &[HashedPageTarget {
                file_name: page.file_name.clone(),
                media_type: page.media_type.clone(),
                file_hash: page.file_hash.clone(),
                file_size: page.file_size.unwrap(),
                page_number: 2,
            }],
            4,
        );
        let before = fs::read(&source).unwrap();
        let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();

        assert_eq!(
            process_remove_hashed_pages_task(&state, &task).await,
            TaskOutcome::Completed
        );
        assert!(fs::read(&source).unwrap() != before);
        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert!(saved_book.file_hash.is_empty() && saved_book.file_hash_koreader.is_empty());
        let saved_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_media.pages.len(), 2);
        assert_eq!(saved_media.pages[0].file_name, "images/1.png");
        assert_eq!(saved_media.pages[1].file_name, "images/3.png");
        assert!(saved_media.pages.iter().all(|page| !page.file_hash.is_empty()));
        let mut archive = ZipArchive::new(File::open(&source).unwrap()).unwrap();
        assert!(archive.by_name("pages/2.xhtml").is_err());
        assert!(archive.by_name("images/2.png").is_err());
        let nav = {
            let mut entry = archive.by_name("nav.xhtml").unwrap();
            let mut content = String::new();
            entry.read_to_string(&mut content).unwrap();
            content
        };
        assert!(!nav.contains("pages/2.xhtml"));
        assert!(nav.contains("pages/1.xhtml") && nav.contains("pages/3.xhtml"));
        assert_eq!(
            storage
                .find_known_page_hash(&page.file_hash)
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
        assert!(
            tasks
                .list()
                .await
                .unwrap()
                .iter()
                .any(|task| task.simple_type == "HashBookPages")
        );
        for extension in ["tmp", "bak", "json"] {
            assert!(
                !series
                    .join(format!(".komga-page-remove-{}.{extension}", book.id))
                    .exists()
            );
        }
    }

    fn write_fixed_layout_epub(path: &Path) {
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/hashpage");
        let images = [
            fs::read(fixtures.join("tg.png/1.png")).unwrap(),
            fs::read(fixtures.join("xt.png/1.png")).unwrap(),
            fs::read(fixtures.join("tg.png/1.png")).unwrap(),
        ];
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        writer
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(EPUB_MEDIA_TYPE.as_bytes()).unwrap();
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("META-INF/container.xml", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            )
            .unwrap();
        writer.start_file("content.opf", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata><meta property="rendition:layout">pre-paginated</meta></metadata><manifest><item id="p1" href="pages/1.xhtml" media-type="application/xhtml+xml"/><item id="p2" href="pages/2.xhtml" media-type="application/xhtml+xml"/><item id="p3" href="pages/3.xhtml" media-type="application/xhtml+xml"/><item id="i1" href="images/1.png" media-type="image/png"/><item id="i2" href="images/2.png" media-type="image/png"/><item id="i3" href="images/3.png" media-type="image/png"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="p1"/><itemref idref="p2"/><itemref idref="p3"/></spine></package>"#,
            )
            .unwrap();
        writer.start_file("nav.xhtml", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="pages/1.xhtml">One</a></li><li><a href="pages/2.xhtml">Two</a><ol><li><a href="pages/3.xhtml">Three</a></li></ol></li></ol></nav></body></html>"#,
            )
            .unwrap();
        for (index, image) in images.iter().enumerate() {
            let number = index + 1;
            writer.start_file(format!("pages/{number}.xhtml"), options).unwrap();
            writer
                .write_all(
                    format!(
                        r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>{number}</title></head><body><img src="../images/{number}.png"/></body></html>"#
                    )
                    .as_bytes(),
                )
                .unwrap();
            writer.start_file(format!("images/{number}.png"), options).unwrap();
            writer.write_all(image).unwrap();
        }
        writer.finish().unwrap();
    }

    #[tokio::test]
    async fn rar_hash_and_removal_convert_to_cbz_without_library_conversion_enabled() {
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/archives/rar4-solid.rar");
        let source = series_path.join("Volume.cbr");
        fs::copy(fixture, &source).unwrap();

        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.hash_pages = true;
        assert!(!library.convert_to_cbz);
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
        assert_eq!(analyzed.media_type.as_deref(), Some(RAR4_MEDIA_TYPE));
        storage.replace_media_analysis(&analyzed).await.unwrap();
        let state = AppState::new(storage.clone(), tasks.clone())
            .await
            .unwrap()
            .with_page_hashing(NonZeroUsize::new(2).unwrap());

        assert_eq!(
            super::super::page_hashing::process_hash_book_pages_task(
                &state,
                &TaskRecord::hash_book_pages(&book.id, 1),
            )
            .await,
            TaskOutcome::Completed
        );
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(media.pages.len(), 3);
        assert!(media.pages.iter().all(|page| !page.file_hash.is_empty()));
        let removed_page = &media.pages[0];
        storage
            .create_or_update_page_hash(
                &removed_page.file_hash,
                removed_page.file_size,
                PageHashAction::DeleteManual,
                None,
            )
            .await
            .unwrap();
        let removal = TaskRecord::remove_hashed_pages(
            &book.id,
            &[HashedPageTarget {
                file_name: removed_page.file_name.clone(),
                media_type: removed_page.media_type.clone(),
                file_hash: removed_page.file_hash.clone(),
                file_size: removed_page.file_size.unwrap(),
                page_number: 1,
            }],
            4,
        );
        assert_eq!(
            process_remove_hashed_pages_task(&state, &removal).await,
            TaskOutcome::Completed
        );

        let target = series_path.join("Volume.cbz");
        assert!(!source.exists());
        assert!(target.exists());
        assert_eq!(ZipArchive::new(File::open(&target).unwrap()).unwrap().len(), 2);
        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_book.url, Url::from_file_path(&target).unwrap());
        let saved_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_media.media_type.as_deref(), Some(ZIP_MEDIA_TYPE));
        assert_eq!(saved_media.pages.len(), 2);
        assert!(saved_media.pages.iter().all(|page| !page.file_hash.is_empty()));
        let history = storage
            .list_historical_events(&komga_core::PageRequest {
                unpaged: true,
                ..komga_core::PageRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(history.total_elements, 3);
        assert!(history.content.iter().any(|event| event.event_type == "BookConverted"));
        assert!(
            history
                .content
                .iter()
                .any(|event| event.event_type == "DuplicatePageDeleted")
        );
        for extension in ["tmp", "bak"] {
            assert!(
                !series_path
                    .join(format!(".komga-convert-{}.{extension}", book.id))
                    .exists()
            );
        }
        let queued = tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|task| task.simple_type)
            .collect::<Vec<_>>();
        assert!(queued.iter().any(|value| value == "GenerateBookThumbnail"));
        assert!(queued.iter().any(|value| value == "HashBookPages"));
        assert_eq!(state.search.search(EntityKind::Book, "volume")[0].id, book.id);
    }

    #[tokio::test]
    async fn rar_removal_and_regular_conversion_serialize_in_either_order() {
        for removal_first in [true, false] {
            let storage = Storage::connect_memory().await.unwrap();
            let tasks = TaskStorage::connect_memory().await.unwrap();
            let temporary = tempfile::tempdir().unwrap();
            let root = temporary.path().join("library");
            let series_path = root.join("Series");
            fs::create_dir_all(&series_path).unwrap();
            let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../komga/komga/src/test/resources/archives/rar4-solid.rar");
            let source = series_path.join("Volume.cbr");
            fs::copy(fixture, &source).unwrap();

            let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
            library.hash_pages = true;
            library.convert_to_cbz = true;
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
            let state = AppState::new(storage.clone(), tasks)
                .await
                .unwrap()
                .with_page_hashing(NonZeroUsize::new(2).unwrap());
            assert_eq!(
                super::super::page_hashing::process_hash_book_pages_task(
                    &state,
                    &TaskRecord::hash_book_pages(&book.id, 1),
                )
                .await,
                TaskOutcome::Completed
            );
            let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
            let removed = &media.pages[1];
            let removal = TaskRecord::remove_hashed_pages(
                &book.id,
                &[HashedPageTarget {
                    file_name: removed.file_name.clone(),
                    media_type: removed.media_type.clone(),
                    file_hash: removed.file_hash.clone(),
                    file_size: removed.file_size.unwrap(),
                    page_number: 2,
                }],
                4,
            );
            let conversion = TaskRecord::convert_book(&book.id, &book.series_id, 4);

            let (first, second) = if removal_first {
                let first_state = state.clone();
                let first_task = removal.clone();
                let first =
                    tokio::spawn(async move { process_remove_hashed_pages_task(&first_state, &first_task).await });
                tokio::task::yield_now().await;
                let second_state = state.clone();
                let second = tokio::spawn(async move {
                    super::super::conversion::process_convert_book_task(&second_state, &conversion).await
                });
                (first.await.unwrap(), second.await.unwrap())
            } else {
                let first_state = state.clone();
                let first = tokio::spawn(async move {
                    super::super::conversion::process_convert_book_task(&first_state, &conversion).await
                });
                tokio::task::yield_now().await;
                let second_state = state.clone();
                let second_task = removal.clone();
                let second =
                    tokio::spawn(async move { process_remove_hashed_pages_task(&second_state, &second_task).await });
                (first.await.unwrap(), second.await.unwrap())
            };
            assert_eq!(first, TaskOutcome::Completed, "removal_first={removal_first}");
            assert_eq!(second, TaskOutcome::Completed, "removal_first={removal_first}");

            let target = series_path.join("Volume.cbz");
            assert!(!source.exists(), "removal_first={removal_first}");
            assert!(target.exists(), "removal_first={removal_first}");
            assert_eq!(
                ZipArchive::new(File::open(&target).unwrap()).unwrap().len(),
                2,
                "removal_first={removal_first}"
            );
            let saved = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
            assert_eq!(saved.media_type.as_deref(), Some(ZIP_MEDIA_TYPE));
            assert_eq!(saved.pages.len(), 2);
            for extension in ["tmp", "bak"] {
                assert!(
                    !series_path
                        .join(format!(".komga-convert-{}.{extension}", book.id))
                        .exists(),
                    "removal_first={removal_first}"
                );
            }
        }
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
        let renderer = PdfPageRenderer::open(&path).unwrap();
        let updates = media
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let computed = renderer.hash_page(index).unwrap().0;
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
        assert!(!series.join(format!(".komga-page-remove-{}.json", book.id)).exists());
    }

    #[test]
    fn journal_recovery_uses_database_snapshot_and_file_fingerprints() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        let source = root.join("journal.cbz");
        fs::write(&source, b"old-source-snapshot").unwrap();
        let old_book = book_for_path(&source, &library);
        let mut old_media = legacy_media(&old_book);
        old_media.page_count = 3;
        let paths = validated_page_removal_paths(&old_book, &library).unwrap();

        fs::write(&paths.temporary, b"new-source").unwrap();
        let (journal, _, _) = journal_for_temporary(&old_book, &old_media, &paths, 2);
        write_page_removal_journal(&paths, &journal).unwrap();
        recover_page_removal_artifacts(&old_book, &old_media, &library, "test-journal-only", false).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"old-source-snapshot");
        assert!(!paths.backup.exists() && !paths.temporary.exists() && !paths.journal.exists());

        fs::write(&paths.temporary, b"new-source").unwrap();
        let (journal, _, _) = journal_for_temporary(&old_book, &old_media, &paths, 2);
        write_page_removal_journal(&paths, &journal).unwrap();
        fs::rename(&paths.source, &paths.backup).unwrap();
        recover_page_removal_artifacts(&old_book, &old_media, &library, "test-backup-only", false).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"old-source-snapshot");
        assert!(!paths.backup.exists() && !paths.temporary.exists() && !paths.journal.exists());

        fs::write(&paths.temporary, b"new-source").unwrap();
        let (journal, _, _) = journal_for_temporary(&old_book, &old_media, &paths, 2);
        write_page_removal_journal(&paths, &journal).unwrap();
        fs::rename(&paths.source, &paths.backup).unwrap();
        fs::rename(&paths.temporary, &paths.source).unwrap();

        recover_page_removal_artifacts(&old_book, &old_media, &library, "test-old", false).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"old-source-snapshot");
        assert!(!paths.backup.exists() && !paths.temporary.exists() && !paths.journal.exists());

        fs::write(&paths.temporary, b"new-source").unwrap();
        let (journal, new_book, new_media) = journal_for_temporary(&old_book, &old_media, &paths, 2);
        write_page_removal_journal(&paths, &journal).unwrap();
        fs::rename(&paths.source, &paths.backup).unwrap();
        fs::rename(&paths.temporary, &paths.source).unwrap();
        fs::remove_file(&paths.backup).unwrap();

        recover_page_removal_artifacts(&new_book, &new_media, &library, "test-new", false).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"new-source");
        assert!(!paths.backup.exists() && !paths.temporary.exists() && !paths.journal.exists());
    }

    #[test]
    fn corrupt_journal_and_unknown_fingerprints_are_preserved_as_conflicts() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        let source = root.join("corrupt.cbz");
        fs::write(&source, b"old-source").unwrap();
        let book = book_for_path(&source, &library);
        let mut media = legacy_media(&book);
        media.page_count = 2;
        let paths = validated_page_removal_paths(&book, &library).unwrap();
        fs::write(&paths.journal, b"{").unwrap();

        assert!(recover_page_removal_artifacts(&book, &media, &library, "test", false).is_err());
        assert!(paths.journal.exists());

        fs::remove_file(&paths.journal).unwrap();
        fs::write(&paths.temporary, b"new-source").unwrap();
        let (journal, _, _) = journal_for_temporary(&book, &media, &paths, 1);
        write_page_removal_journal(&paths, &journal).unwrap();
        fs::write(&paths.temporary, b"unknown-source-with-different-bytes").unwrap();
        assert!(recover_page_removal_artifacts(&book, &media, &library, "test", false).is_err());
        assert!(paths.journal.exists() && paths.temporary.exists());
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
        recover_page_removal_artifacts(&book, &legacy_media(&book), &library, "test", false).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert!(!temporary_path.exists() && !backup.exists());

        let source = root.join("switched.cbz");
        fs::write(&source, b"old-source").unwrap();
        let book = book_for_path(&source, &library);
        let (_, backup) = artifact_paths(&book);
        fs::rename(&source, &backup).unwrap();
        fs::write(&source, b"uncommitted-new-source").unwrap();
        recover_page_removal_artifacts(&book, &legacy_media(&book), &library, "test", false).unwrap();
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
        recover_page_removal_artifacts(&committed_book, &legacy_media(&committed_book), &library, "test", false)
            .unwrap();
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

        assert!(recover_page_removal_artifacts(&book, &legacy_media(&book), &library, "test", false).is_err());
        assert!(source.exists() && backup.exists());

        let outside = temporary.path().join("outside.cbz");
        fs::write(&outside, b"outside").unwrap();
        let outside_book = book_for_path(&outside, &library);
        assert!(
            recover_page_removal_artifacts(&outside_book, &legacy_media(&outside_book), &library, "test", false,)
                .is_err()
        );
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
        assert!(recover_page_removal_artifacts(&book, &legacy_media(&book), &library, "test", false).is_err());
        assert!(fs::symlink_metadata(&source).unwrap().file_type().is_symlink());

        let regular = root.join("regular.cbz");
        fs::write(&regular, b"regular").unwrap();
        let book = book_for_path(&regular, &library);
        let (temporary_path, _) = artifact_paths(&book);
        symlink(&real, &temporary_path).unwrap();
        assert!(recover_page_removal_artifacts(&book, &legacy_media(&book), &library, "test", false).is_err());
        assert!(fs::symlink_metadata(&temporary_path).unwrap().file_type().is_symlink());
        fs::remove_file(&temporary_path).unwrap();

        let journal = regular
            .parent()
            .unwrap()
            .join(format!(".komga-page-remove-{}.json", book.id));
        symlink(&real, &journal).unwrap();
        assert!(recover_page_removal_artifacts(&book, &legacy_media(&book), &library, "test", false).is_err());
        assert!(fs::symlink_metadata(&journal).unwrap().file_type().is_symlink());
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

    fn legacy_media(book: &Book) -> Media {
        let mut media = Media::result(
            book.id.as_str(),
            komga_core::MediaStatus::Ready,
            Some(ZIP_MEDIA_TYPE.into()),
            None,
        );
        media.page_count = 1;
        media
    }

    fn journal_for_temporary(
        old_book: &Book,
        old_media: &Media,
        paths: &PageRemovalPaths,
        new_page_count: i32,
    ) -> (PageRemovalJournal, Book, Media) {
        let metadata = fs::metadata(&paths.temporary).unwrap();
        let mut new_book = old_book.clone();
        new_book.file_size = i64::try_from(metadata.len()).unwrap();
        new_book.file_last_modified = updated_time(&metadata);
        let mut new_media = old_media.clone();
        new_media.page_count = new_page_count;
        let journal = PageRemovalJournal {
            version: PAGE_REMOVAL_JOURNAL_VERSION,
            book_id: old_book.id.clone(),
            source_file_name: paths.source.file_name().unwrap().to_str().unwrap().into(),
            media_type: old_media.media_type.clone().unwrap(),
            old: PageRemovalJournalSnapshot {
                file_size: old_book.file_size,
                file_last_modified: old_book.file_last_modified,
                page_count: old_media.page_count,
                fingerprint: hash_file(&paths.source, || false).unwrap(),
            },
            new: PageRemovalJournalSnapshot {
                file_size: new_book.file_size,
                file_last_modified: new_book.file_last_modified,
                page_count: new_media.page_count,
                fingerprint: hash_file(&paths.temporary, || false).unwrap(),
            },
        };
        validate_page_removal_journal(&journal, paths).unwrap();
        (journal, new_book, new_media)
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
