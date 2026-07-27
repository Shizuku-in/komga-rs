use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use komga_core::{
    Book, BookEvent, DomainEvent, EPUB_MEDIA_TYPE, HashedPageTarget, HistoricalEvent, Library, Media, PDF_MEDIA_TYPE,
    RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, SeriesEvent, ZIP_MEDIA_TYPE, scan_book_file,
};
use komga_media::{
    PageRemovalError, PageRemovalRewrite, RarToCbzError, convert_rar_to_cbz, rewrite_hashed_rar_pages_to_cbz,
};
use komga_storage::{StorageError, TaskRecord};

use super::{BookTaskPayload, LibraryTaskPayload, TaskOutcome};
use crate::AppState;

const CONVERSION_DELETE_REASON: &str = "File was deleted after conversion to CBZ";

pub(crate) async fn enqueue_extension_repairs(
    state: &AppState,
    library_id: &str,
    priority: i32,
) -> Result<(), StorageError> {
    for (book_id, series_id) in state.storage.list_books_with_mismatched_extensions(library_id).await? {
        state
            .enqueue_task(&TaskRecord::repair_extension(&book_id, &series_id, priority))
            .await?;
    }
    Ok(())
}

pub(super) async fn process_find_books_to_convert_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<LibraryTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid FindBooksToConvert payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load conversion library");
            return TaskOutcome::Retry;
        }
    };
    if !library.convert_to_cbz {
        return TaskOutcome::Completed;
    }
    let targets = match state.storage.list_books_convertible_to_cbz(&library.id).await {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, library_id = %library.id, "could not find books to convert");
            return TaskOutcome::Retry;
        }
    };
    for (book_id, series_id) in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::convert_book(
                &book_id,
                &series_id,
                task.priority.saturating_add(1),
            ))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue book conversion");
            return TaskOutcome::Retry;
        }
    }
    TaskOutcome::Completed
}

pub(super) async fn process_convert_book_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid ConvertBook payload");
            return TaskOutcome::Completed;
        }
    };
    let source_mutation_guard = state.source_mutation_lock(&payload.book_id).lock().await;
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load conversion book");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load conversion settings");
            return TaskOutcome::Retry;
        }
    };

    let recovery_book = book.clone();
    let recovery_library = library.clone();
    match tokio::task::spawn_blocking(move || recover_conversion_artifacts(&recovery_book, &recovery_library)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return work_failure("conversion recovery", &book.id, error),
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "conversion recovery panicked");
            return TaskOutcome::Retry;
        }
    }
    if !library.convert_to_cbz {
        return TaskOutcome::Completed;
    }
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) => media,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load conversion media");
            return TaskOutcome::Retry;
        }
    };

    let work_book = book.clone();
    let work_media = media.clone();
    let work_library = library.clone();
    let cancellation = state.shutdown.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_conversion(&work_book, &work_media, &work_library, || cancellation.is_cancelled())
    })
    .await;
    let mut prepared = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => return work_failure("book conversion", &book.id, error),
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "book conversion worker panicked");
            return TaskOutcome::Retry;
        }
    };

    let events = [
        HistoricalEvent::book_file_deleted_for_reason(&book, CONVERSION_DELETE_REASON),
        HistoricalEvent::book_converted(&prepared.book, &book),
    ];
    match state
        .storage
        .commit_book_conversion_if_current(&book, &media, &prepared.book, &prepared.media, &events)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(book_id = %book.id, "book or media changed during conversion commit");
            prepared.rollback();
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist book conversion");
            prepared.rollback();
            return TaskOutcome::Retry;
        }
    }
    prepared.finish();
    drop(source_mutation_guard);
    publish_file_change(state, &prepared.book, &library, task.priority).await;
    tracing::info!(book_id = %book.id, path = %prepared.target.display(), "book converted to CBZ");
    TaskOutcome::Completed
}

pub(super) async fn perform_rar_page_removal(
    state: &AppState,
    book: &Book,
    media: &Media,
    library: &Library,
    targets: &[HashedPageTarget],
) -> Result<PageRemovalRewrite, TaskOutcome> {
    let recovery_book = book.clone();
    let recovery_library = library.clone();
    match tokio::task::spawn_blocking(move || recover_conversion_artifacts(&recovery_book, &recovery_library)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(work_failure("RAR page-removal recovery", &book.id, error)),
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "RAR page-removal recovery panicked");
            return Err(TaskOutcome::Retry);
        }
    }

    let work_book = book.clone();
    let work_media = media.clone();
    let work_library = library.clone();
    let work_targets = targets.to_vec();
    let cancellation = state.shutdown.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_rar_page_removal(&work_book, &work_media, &work_library, &work_targets, || {
            cancellation.is_cancelled()
        })
    })
    .await;
    let mut prepared = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => return Err(work_failure("RAR duplicate-page removal", &book.id, error)),
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "RAR page-removal worker panicked");
            return Err(TaskOutcome::Retry);
        }
    };

    match state
        .storage
        .commit_rar_page_removal_if_current(
            book,
            media,
            &prepared.rewrite.book,
            &prepared.rewrite.media,
            &prepared.rewrite.removed,
            &prepared.rewrite.retained_original_indices,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(book_id = %book.id, "book or media changed during RAR page-removal commit");
            prepared.rollback();
            enqueue_recovery_scan(state, &book.library_id).await;
            return Err(TaskOutcome::Completed);
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist RAR page removal");
            prepared.rollback();
            enqueue_recovery_scan(state, &book.library_id).await;
            return Err(TaskOutcome::Retry);
        }
    }
    prepared.finish();
    Ok(prepared.rewrite)
}

pub(super) async fn process_repair_extension_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid RepairExtension payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load extension-repair book");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load extension-repair settings");
            return TaskOutcome::Retry;
        }
    };
    if !library.repair_extensions {
        return TaskOutcome::Completed;
    }
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) => media,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load extension-repair media");
            return TaskOutcome::Retry;
        }
    };
    let Some(correct_extension) = media.media_type.as_deref().and_then(correct_extension) else {
        tracing::warn!(book_id = %book.id, media_type = ?media.media_type, "book media type cannot be repaired");
        return TaskOutcome::Completed;
    };

    let recovery_book = book.clone();
    let recovery_library = library.clone();
    let recovered = tokio::task::spawn_blocking(move || {
        recover_repair_artifacts(&recovery_book, &recovery_library, correct_extension)
    })
    .await;
    match recovered {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return work_failure("extension-repair recovery", &book.id, error),
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "extension-repair recovery panicked");
            return TaskOutcome::Retry;
        }
    }

    let source = match book.url.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            tracing::warn!(book_id = %book.id, "cannot repair a non-local book URL");
            return TaskOutcome::Completed;
        }
    };
    let actual_extension = source.extension().and_then(|value| value.to_str()).unwrap_or_default();
    if actual_extension.eq_ignore_ascii_case("epub") && media.media_type.as_deref() == Some(ZIP_MEDIA_TYPE) {
        tracing::info!(book_id = %book.id, "EPUB detected as ZIP is intentionally not renamed");
        return TaskOutcome::Completed;
    }
    if actual_extension.eq_ignore_ascii_case(correct_extension) {
        return TaskOutcome::Completed;
    }

    let work_book = book.clone();
    let work_library = library.clone();
    let prepared =
        tokio::task::spawn_blocking(move || prepare_extension_repair(&work_book, &work_library, correct_extension))
            .await;
    let mut prepared = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => return work_failure("extension repair", &book.id, error),
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "extension-repair worker panicked");
            return TaskOutcome::Retry;
        }
    };

    match state
        .storage
        .commit_book_extension_repair_if_current(&book, &prepared.book)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(book_id = %book.id, "book changed during extension-repair commit");
            prepared.rollback();
            enqueue_recovery_scan(state, &book.library_id).await;
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not persist extension repair");
            prepared.rollback();
            return TaskOutcome::Retry;
        }
    }
    prepared.finish();
    publish_file_change(state, &prepared.book, &library, task.priority).await;
    tracing::info!(book_id = %book.id, path = %prepared.target.display(), "book extension repaired");
    TaskOutcome::Completed
}

async fn publish_file_change(state: &AppState, book: &Book, library: &Library, priority: i32) {
    state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(book)));
    if book.oneshot {
        state.publish_domain_event(DomainEvent::SeriesUpdated(SeriesEvent {
            series_id: book.series_id.clone(),
            library_id: book.library_id.clone(),
        }));
    }
    if library.hash_files
        && let Err(error) = state
            .enqueue_task(&TaskRecord::hash_book(&book.id, priority.saturating_add(1)))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue post-file-change hash");
    }
    if library.hash_koreader
        && let Err(error) = state
            .enqueue_task(&TaskRecord::hash_book_koreader(&book.id, priority.saturating_add(1)))
            .await
    {
        tracing::error!(%error, book_id = %book.id, "could not enqueue post-file-change KOReader hash");
    }
    let books_synced = state.sync_books(std::slice::from_ref(&book.id)).await;
    let series_synced = !book.oneshot || state.sync_series(std::slice::from_ref(&book.series_id)).await;
    if !books_synced || !series_synced {
        state.recover_search_after_http_failure().await;
    }
}

fn correct_extension(media_type: &str) -> Option<&'static str> {
    match media_type {
        RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE => Some("cbr"),
        ZIP_MEDIA_TYPE => Some("cbz"),
        PDF_MEDIA_TYPE => Some("pdf"),
        EPUB_MEDIA_TYPE => Some("epub"),
        _ => None,
    }
}

#[derive(Debug)]
struct PreparedConversion {
    book: Book,
    media: komga_core::Media,
    source: PathBuf,
    target: PathBuf,
    temporary: PathBuf,
    backup: PathBuf,
}

#[derive(Debug)]
struct PreparedRarPageRemoval {
    rewrite: PageRemovalRewrite,
    source: PathBuf,
    target: PathBuf,
    temporary: PathBuf,
    backup: PathBuf,
}

impl PreparedRarPageRemoval {
    fn rollback(&mut self) {
        remove_regular_file(&self.target, "RAR page-removal target");
        if self.backup.exists()
            && let Err(error) = fs::rename(&self.backup, &self.source)
        {
            tracing::error!(%error, from = %self.backup.display(), to = %self.source.display(), "could not restore RAR page-removal source");
        }
        remove_regular_file(&self.temporary, "RAR page-removal temporary file");
    }

    fn finish(&mut self) {
        remove_regular_file(&self.backup, "RAR page-removal backup");
        remove_regular_file(&self.temporary, "RAR page-removal temporary file");
    }
}

impl PreparedConversion {
    fn rollback(&mut self) {
        remove_regular_file(&self.target, "converted target");
        if self.backup.exists()
            && let Err(error) = fs::rename(&self.backup, &self.source)
        {
            tracing::error!(%error, from = %self.backup.display(), to = %self.source.display(), "could not restore conversion source");
        }
        remove_regular_file(&self.temporary, "conversion temporary file");
    }

    fn finish(&mut self) {
        remove_regular_file(&self.backup, "conversion backup");
        remove_regular_file(&self.temporary, "conversion temporary file");
    }
}

fn prepare_conversion(
    book: &Book,
    media: &komga_core::Media,
    library: &Library,
    cancelled: impl Fn() -> bool,
) -> Result<PreparedConversion, WorkError> {
    let (root, source, parent) = validated_source(book, library)?;
    let target = source.with_extension("cbz");
    let (temporary, backup) = conversion_artifacts(&parent, &book.id);
    ensure_within(&target, &parent, &root)?;
    ensure_within(&temporary, &parent, &root)?;
    ensure_within(&backup, &parent, &root)?;
    ensure_absent(&target, "conversion target")?;
    ensure_absent(&temporary, "conversion temporary file")?;
    ensure_absent(&backup, "conversion backup")?;

    let mut converted =
        convert_rar_to_cbz(book, media, &temporary, library.analyze_dimensions, cancelled).map_err(WorkError::from)?;
    converted.book.url = url::Url::from_file_path(&target)
        .map_err(|()| WorkError::terminal("conversion target cannot be represented as a file URL"))?;
    converted.book.name = source
        .file_stem()
        .ok_or_else(|| WorkError::terminal("conversion source has no filename stem"))?
        .to_string_lossy()
        .into_owned();

    fs::rename(&source, &backup).map_err(WorkError::retry)?;
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::rename(&backup, &source);
        return Err(WorkError::retry(error));
    }
    if let Some(scanned) = scan_book_file(&target, &book.library_id, book.oneshot) {
        converted.book.name = scanned.name;
        converted.book.url = scanned.url;
        converted.book.file_size = scanned.file_size;
        converted.book.file_last_modified = scanned.file_last_modified;
    } else {
        remove_regular_file(&target, "invalid converted target");
        let _ = fs::rename(&backup, &source);
        return Err(WorkError::terminal("converted CBZ could not be scanned"));
    }
    Ok(PreparedConversion {
        book: converted.book,
        media: converted.media,
        source,
        target,
        temporary,
        backup,
    })
}

fn prepare_rar_page_removal(
    book: &Book,
    media: &Media,
    library: &Library,
    targets: &[HashedPageTarget],
    cancelled: impl Fn() -> bool,
) -> Result<PreparedRarPageRemoval, WorkError> {
    let (root, source, parent) = validated_source(book, library)?;
    let target = source.with_extension("cbz");
    let (temporary, backup) = conversion_artifacts(&parent, &book.id);
    ensure_within(&target, &parent, &root)?;
    ensure_within(&temporary, &parent, &root)?;
    ensure_within(&backup, &parent, &root)?;
    ensure_absent(&target, "RAR page-removal target")?;
    ensure_absent(&temporary, "RAR page-removal temporary file")?;
    ensure_absent(&backup, "RAR page-removal backup")?;

    let mut rewrite =
        rewrite_hashed_rar_pages_to_cbz(book, media, targets, &temporary, library.analyze_dimensions, &cancelled)
            .map_err(WorkError::from)?;
    rewrite.book.url = url::Url::from_file_path(&target)
        .map_err(|()| WorkError::terminal("RAR page-removal target cannot be represented as a file URL"))?;
    rewrite.book.name = source
        .file_stem()
        .ok_or_else(|| WorkError::terminal("RAR page-removal source has no filename stem"))?
        .to_string_lossy()
        .into_owned();
    if cancelled() {
        remove_regular_file(&temporary, "cancelled RAR page-removal temporary file");
        return Err(WorkError::from(PageRemovalError::Cancelled));
    }

    fs::rename(&source, &backup).map_err(WorkError::retry)?;
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::rename(&backup, &source);
        return Err(WorkError::retry(error));
    }
    if let Some(scanned) = scan_book_file(&target, &book.library_id, book.oneshot) {
        rewrite.book.name = scanned.name;
        rewrite.book.url = scanned.url;
        rewrite.book.file_size = scanned.file_size;
        rewrite.book.file_last_modified = scanned.file_last_modified;
    } else {
        remove_regular_file(&target, "invalid RAR page-removal target");
        let _ = fs::rename(&backup, &source);
        return Err(WorkError::terminal("RAR page-removal CBZ could not be scanned"));
    }
    Ok(PreparedRarPageRemoval {
        rewrite,
        source,
        target,
        temporary,
        backup,
    })
}

#[derive(Debug)]
struct PreparedRepair {
    book: Book,
    source: PathBuf,
    target: PathBuf,
    marker: PathBuf,
}

impl PreparedRepair {
    fn rollback(&mut self) {
        if self.target.exists()
            && let Err(error) = fs::rename(&self.target, &self.source)
        {
            tracing::error!(%error, from = %self.target.display(), to = %self.source.display(), "could not restore repaired filename");
        }
        remove_regular_file(&self.marker, "extension-repair marker");
    }

    fn finish(&mut self) {
        remove_regular_file(&self.marker, "extension-repair marker");
    }
}

fn prepare_extension_repair(book: &Book, library: &Library, extension: &str) -> Result<PreparedRepair, WorkError> {
    let (root, source, parent) = validated_source(book, library)?;
    let target = source.with_extension(extension);
    let marker = repair_marker(&parent, &book.id);
    ensure_within(&target, &parent, &root)?;
    ensure_within(&marker, &parent, &root)?;
    ensure_absent(&target, "extension-repair target")?;
    ensure_absent(&marker, "extension-repair marker")?;
    let marker_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .map_err(WorkError::retry)?;
    marker_file.sync_all().map_err(WorkError::retry)?;
    if let Err(error) = fs::rename(&source, &target) {
        remove_regular_file(&marker, "extension-repair marker");
        return Err(WorkError::retry(error));
    }
    let Some(scanned) = scan_book_file(&target, &book.library_id, book.oneshot) else {
        let _ = fs::rename(&target, &source);
        remove_regular_file(&marker, "extension-repair marker");
        return Err(WorkError::terminal("repaired book could not be scanned"));
    };
    let mut repaired = book.clone();
    repaired.name = scanned.name;
    repaired.url = scanned.url;
    repaired.file_size = scanned.file_size;
    repaired.file_last_modified = scanned.file_last_modified;
    repaired.file_hash.clear();
    repaired.file_hash_koreader.clear();
    Ok(PreparedRepair {
        book: repaired,
        source,
        target,
        marker,
    })
}

fn recover_conversion_artifacts(book: &Book, library: &Library) -> Result<(), WorkError> {
    let (root, source, parent) = validated_parent(book, library)?;
    let target = source.with_extension("cbz");
    let (temporary, backup) = conversion_artifacts(&parent, &book.id);
    ensure_within(&target, &parent, &root)?;
    ensure_within(&temporary, &parent, &root)?;
    ensure_within(&backup, &parent, &root)?;
    if path_exists(&backup)? {
        validate_artifact(&backup, &root)?;
        if path_exists(&source)? {
            validate_regular_source(&source, &root)?;
            if source
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("cbz"))
            {
                fs::remove_file(&backup).map_err(WorkError::retry)?;
            } else {
                return Err(WorkError::terminal(
                    "conversion backup conflicts with the current source",
                ));
            }
        } else {
            if path_exists(&target)? {
                validate_artifact(&target, &root)?;
                fs::remove_file(&target).map_err(WorkError::retry)?;
            }
            fs::rename(&backup, &source).map_err(WorkError::retry)?;
        }
    }
    if path_exists(&temporary)? {
        validate_artifact(&temporary, &root)?;
        fs::remove_file(&temporary).map_err(WorkError::retry)?;
    }
    Ok(())
}

fn recover_repair_artifacts(book: &Book, library: &Library, extension: &str) -> Result<(), WorkError> {
    let (root, source, parent) = validated_parent(book, library)?;
    let target = source.with_extension(extension);
    let marker = repair_marker(&parent, &book.id);
    ensure_within(&target, &parent, &root)?;
    ensure_within(&marker, &parent, &root)?;
    if !path_exists(&marker)? {
        return Ok(());
    }
    validate_artifact(&marker, &root)?;
    if path_exists(&source)? {
        validate_regular_source(&source, &root)?;
        if path_exists(&target)? && target != source {
            return Err(WorkError::terminal(
                "extension-repair marker conflicts with an existing target",
            ));
        }
    } else if path_exists(&target)? {
        validate_artifact(&target, &root)?;
        fs::rename(&target, &source).map_err(WorkError::retry)?;
    } else {
        return Err(WorkError::terminal(
            "extension-repair marker exists but neither source nor target is present",
        ));
    }
    fs::remove_file(&marker).map_err(WorkError::retry)?;
    Ok(())
}

fn validated_source(book: &Book, library: &Library) -> Result<(PathBuf, PathBuf, PathBuf), WorkError> {
    let (root, source, parent) = validated_parent(book, library)?;
    validate_regular_source(&source, &root)?;
    let metadata = fs::metadata(&source).map_err(WorkError::retry)?;
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    let scanned = scan_book_file(&source, &book.library_id, book.oneshot)
        .ok_or_else(|| WorkError::terminal("book source could not be scanned"))?;
    if size != book.file_size || scanned.file_last_modified != book.file_last_modified {
        return Err(WorkError::terminal("book source snapshot has changed"));
    }
    Ok((root, source, parent))
}

fn validated_parent(book: &Book, library: &Library) -> Result<(PathBuf, PathBuf, PathBuf), WorkError> {
    let source = book
        .url
        .to_file_path()
        .map_err(|()| WorkError::terminal("book URL is not a local file"))?;
    let root_path = library
        .root_path()
        .ok_or_else(|| WorkError::terminal("library root is not a local directory"))?;
    let root = fs::canonicalize(&root_path).map_err(WorkError::retry)?;
    if !root.is_dir() {
        return Err(WorkError::terminal("library root is not a directory"));
    }
    let parent = source
        .parent()
        .ok_or_else(|| WorkError::terminal("book source has no parent directory"))?
        .to_owned();
    let real_parent = fs::canonicalize(&parent).map_err(WorkError::retry)?;
    if !real_parent.starts_with(&root) {
        return Err(WorkError::terminal("book parent resolves outside the library root"));
    }
    Ok((root, source, parent))
}

fn validate_regular_source(path: &Path, root: &Path) -> Result<(), WorkError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(WorkError::terminal("book source does not exist"));
        }
        Err(error) => return Err(WorkError::retry(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkError::terminal("book source is not a non-symlink regular file"));
    }
    let real = fs::canonicalize(path).map_err(WorkError::retry)?;
    if !real.starts_with(root) {
        return Err(WorkError::terminal("book source resolves outside the library root"));
    }
    Ok(())
}

fn validate_artifact(path: &Path, root: &Path) -> Result<(), WorkError> {
    let metadata = fs::symlink_metadata(path).map_err(WorkError::retry)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(WorkError::terminal(format!(
            "managed path is not a non-symlink regular file: {}",
            path.display()
        )));
    }
    let real = fs::canonicalize(path).map_err(WorkError::retry)?;
    if !real.starts_with(root) {
        return Err(WorkError::terminal(format!(
            "managed path resolves outside the library root: {}",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_within(path: &Path, parent: &Path, root: &Path) -> Result<(), WorkError> {
    let real_parent = fs::canonicalize(parent).map_err(WorkError::retry)?;
    if !real_parent.starts_with(root) || path.parent() != Some(parent) || path.file_name().is_none() {
        return Err(WorkError::terminal("managed path is outside the library root"));
    }
    Ok(())
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

fn conversion_artifacts(parent: &Path, book_id: &str) -> (PathBuf, PathBuf) {
    (
        parent.join(format!(".komga-convert-{book_id}.tmp")),
        parent.join(format!(".komga-convert-{book_id}.bak")),
    )
}

fn repair_marker(parent: &Path, book_id: &str) -> PathBuf {
    parent.join(format!(".komga-repair-{book_id}.pending"))
}

fn remove_regular_file(path: &Path, label: &str) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if let Err(error) = fs::remove_file(path) {
                tracing::warn!(%error, path = %path.display(), "could not remove {label}");
            }
        }
        Ok(_) => tracing::warn!(path = %path.display(), "refusing to remove non-regular {label}"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, path = %path.display(), "could not inspect {label}"),
    }
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
}

impl From<RarToCbzError> for WorkError {
    fn from(error: RarToCbzError) -> Self {
        match error {
            RarToCbzError::Cancelled => Self {
                message: error.to_string(),
                outcome: TaskOutcome::Cancelled,
            },
            RarToCbzError::Io(_) => Self::retry(error),
            RarToCbzError::Stale(_)
            | RarToCbzError::Unsupported(_)
            | RarToCbzError::Archive(_)
            | RarToCbzError::Validation(_) => Self::terminal(error.to_string()),
        }
    }
}

impl From<PageRemovalError> for WorkError {
    fn from(error: PageRemovalError) -> Self {
        match error {
            PageRemovalError::Cancelled => Self {
                message: error.to_string(),
                outcome: TaskOutcome::Cancelled,
            },
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
        tracing::error!(%error, %library_id, "could not enqueue recovery scan after file change");
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use komga_core::{Library, scan_library_files};
    use komga_media::{analyze_book, extract_page, hash_page};
    use komga_storage::Storage;
    use url::Url;

    use super::*;

    async fn conversion_fixture() -> (tempfile::TempDir, Library, Book, komga_core::Media, Vec<u8>) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/archives/rar4-solid.rar");
        let source = series.join("Volume.cbr");
        fs::copy(fixture, &source).unwrap();
        let original = fs::read(&source).unwrap();
        let storage = Storage::connect_memory().await.unwrap();
        let mut library = Library::new("Conversion".into(), Url::from_directory_path(&root).unwrap());
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
        let media = analyze_book(&book, true, || false).unwrap();
        (temporary, library, book, media, original)
    }

    #[tokio::test]
    async fn prepared_conversion_rollback_restores_the_original_source() {
        let (_temporary, library, book, media, original) = conversion_fixture().await;
        let source = book.url.to_file_path().unwrap();
        let mut prepared = prepare_conversion(&book, &media, &library, || false).unwrap();

        assert!(!source.exists());
        assert!(prepared.target.exists());
        assert!(prepared.backup.exists());
        prepared.rollback();

        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(!prepared.target.exists());
        assert!(!prepared.temporary.exists());
        assert!(!prepared.backup.exists());
    }

    #[tokio::test]
    async fn rar_page_removal_preserves_an_existing_cbz_target() {
        let (_temporary, library, book, mut media, original) = conversion_fixture().await;
        let source = book.url.to_file_path().unwrap();
        let target = source.with_extension("cbz");
        fs::write(&target, b"existing target").unwrap();
        let content = extract_page(&book, &media, 1).unwrap();
        media.pages[0].file_hash = hash_page(&content.bytes, &content.media_type).unwrap().hash;
        let page = &media.pages[0];
        let removed = HashedPageTarget {
            file_name: page.file_name.clone(),
            media_type: page.media_type.clone(),
            file_hash: page.file_hash.clone(),
            file_size: page.file_size.unwrap(),
            page_number: 1,
        };

        let error = prepare_rar_page_removal(&book, &media, &library, &[removed], || false).unwrap_err();
        assert_eq!(error.outcome, TaskOutcome::Completed);
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::read(&target).unwrap(), b"existing target");
        let (conversion_temporary, backup) = conversion_artifacts(source.parent().unwrap(), &book.id);
        assert!(!conversion_temporary.exists());
        assert!(!backup.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn conversion_rejects_source_and_parent_symlinks_without_mutating_external_files() {
        use std::os::unix::fs::symlink;

        let (temporary, library, book, media, original) = conversion_fixture().await;
        let source = book.url.to_file_path().unwrap();
        let external = temporary.path().join("external.cbr");
        fs::rename(&source, &external).unwrap();
        symlink(&external, &source).unwrap();

        let error = prepare_conversion(&book, &media, &library, || false).unwrap_err();
        assert_eq!(error.outcome, TaskOutcome::Completed);
        assert_eq!(fs::read(&external).unwrap(), original);
        assert!(fs::symlink_metadata(&source).unwrap().file_type().is_symlink());
        assert!(!source.with_extension("cbz").exists());

        fs::remove_file(&source).unwrap();
        let series = source.parent().unwrap();
        fs::remove_dir(series).unwrap();
        let external_series = temporary.path().join("external-series");
        fs::create_dir(&external_series).unwrap();
        let external_source = external_series.join("Volume.cbr");
        fs::write(&external_source, &original).unwrap();
        symlink(&external_series, series).unwrap();

        let error = prepare_conversion(&book, &media, &library, || false).unwrap_err();
        assert_eq!(error.outcome, TaskOutcome::Completed);
        assert_eq!(fs::read(external_source).unwrap(), original);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn conversion_rejects_an_existing_target_symlink_before_creating_artifacts() {
        use std::os::unix::fs::symlink;

        let (temporary, library, book, media, original) = conversion_fixture().await;
        let source = book.url.to_file_path().unwrap();
        let external = temporary.path().join("external.cbz");
        fs::write(&external, b"external").unwrap();
        let target = source.with_extension("cbz");
        symlink(&external, &target).unwrap();

        let error = prepare_conversion(&book, &media, &library, || false).unwrap_err();
        assert_eq!(error.outcome, TaskOutcome::Completed);
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::read(&external).unwrap(), b"external");
        let parent = source.parent().unwrap();
        let (conversion_temporary, backup) = conversion_artifacts(parent, &book.id);
        assert!(!conversion_temporary.exists());
        assert!(!backup.exists());
    }
}
