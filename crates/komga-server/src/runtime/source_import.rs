use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Local};
use komga_core::{
    Book, BookEvent, BookImportEvent, CopyMode, DomainEvent, HistoricalEvent, Library, ScannedSidecar, Series,
    SidecarSource, SidecarType, scan_book_file, scan_book_sidecars,
};
use komga_storage::TaskRecord;
use serde::Deserialize;
use url::Url;

use crate::AppState;

use super::TaskOutcome;

const UPGRADE_DELETE_REASON: &str = "File was deleted to import an upgrade";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportBookPayload {
    source_file: String,
    series_id: String,
    copy_mode: CopyMode,
    destination_name: Option<String>,
    upgrade_book_id: Option<String>,
}

pub(super) async fn process_import_book_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<ImportBookPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid ImportBook payload");
            return TaskOutcome::Completed;
        }
    };
    let series = match state.storage.find_series_by_id(&payload.series_id).await {
        Ok(Some(series)) => series,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, series_id = %payload.series_id, "could not load import series");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&series.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %series.library_id, "could not load import library");
            return TaskOutcome::Retry;
        }
    };
    if series.oneshot && payload.upgrade_book_id.as_deref().is_none_or(str::is_empty) {
        return import_failure(
            state,
            &payload.source_file,
            "Destination series is oneshot but upgradeBookId is missing",
            TaskOutcome::Completed,
        );
    }
    let upgraded_book = if let Some(id) = &payload.upgrade_book_id {
        match state.storage.find_book_by_id(id).await {
            Ok(Some(book)) if book.series_id == series.id => Some(book),
            Ok(_) => {
                return import_failure(state, &payload.source_file, "ERR_1020", TaskOutcome::Completed);
            }
            Err(error) => {
                tracing::error!(%error, book_id = %id, "could not load import upgrade target");
                return TaskOutcome::Retry;
            }
        }
    } else {
        None
    };
    let libraries = match state.storage.list_libraries().await {
        Ok(libraries) => libraries,
        Err(error) => {
            tracing::error!(%error, "could not load library roots for import validation");
            return TaskOutcome::Retry;
        }
    };

    let source_file = payload.source_file.clone();
    let destination_name = payload.destination_name.clone();
    let work_series = series.clone();
    let work_library = library.clone();
    let work_upgrade = upgraded_book.clone();
    let copy_mode = payload.copy_mode;
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_import(
            Path::new(&source_file),
            &work_series,
            &work_library,
            &libraries,
            copy_mode,
            destination_name.as_deref(),
            work_upgrade.as_ref(),
        )
    })
    .await;
    let mut prepared = match prepared {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => {
            return import_failure(state, &payload.source_file, &error.message, error.outcome);
        }
        Err(error) => {
            tracing::error!(%error, source_file = %payload.source_file, "book import filesystem worker panicked");
            return import_failure(
                state,
                &payload.source_file,
                "Filesystem import failed",
                TaskOutcome::Retry,
            );
        }
    };

    if let Err(error) = state
        .enqueue_task(&TaskRecord::analyze_book(
            &prepared.book.id,
            &series.id,
            task.priority.saturating_add(1),
        ))
        .await
    {
        tracing::error!(%error, book_id = %prepared.book.id, "could not persist imported-book analysis task");
        prepared.rollback();
        return import_failure(
            state,
            &payload.source_file,
            "Could not enqueue book analysis",
            TaskOutcome::Retry,
        );
    }

    let mut events = Vec::new();
    if prepared.old_file_deleted
        && let Some(previous) = upgraded_book.as_ref()
    {
        events.push(HistoricalEvent::book_file_deleted_for_reason(
            previous,
            UPGRADE_DELETE_REASON,
        ));
    }
    events.push(HistoricalEvent::book_imported(
        &prepared.book,
        Path::new(&payload.source_file),
        upgraded_book.is_some(),
    ));
    let committed = state
        .storage
        .commit_book_import(
            &prepared.book,
            &series,
            upgraded_book.as_ref(),
            &prepared.sidecars,
            &prepared.removed_sidecar_urls,
            &events,
        )
        .await;
    let result = match committed {
        Ok(result) => result,
        Err(error) => {
            tracing::error!(%error, book_id = %prepared.book.id, "could not persist book import");
            prepared.rollback();
            return import_failure(state, &payload.source_file, &error.to_string(), TaskOutcome::Retry);
        }
    };
    prepared.finish();

    for target in &result.numbering_refreshes {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::refresh_book_numbering(
                &target.book_id,
                &target.series_id,
                4,
            ))
            .await
        {
            tracing::error!(%error, book_id = %target.book_id, "could not enqueue numbering metadata refresh");
        }
    }
    if !prepared.sidecars.is_empty()
        && let Err(error) = state
            .enqueue_task(&TaskRecord::refresh_book_local_artwork(&result.book.id, 4))
            .await
    {
        tracing::error!(%error, book_id = %result.book.id, "could not enqueue imported local artwork refresh");
    }

    state.publish_domain_event(DomainEvent::BookAdded(BookEvent::from(&result.book)));
    if let Some(previous) = &result.deleted_book {
        state.publish_domain_event(DomainEvent::BookDeleted(BookEvent::from(previous)));
    }
    state.publish_domain_event(DomainEvent::BookImported(BookImportEvent {
        book_id: Some(result.book.id.clone()),
        source_file: payload.source_file.clone(),
        success: true,
        message: None,
    }));
    let mut book_ids = vec![result.book.id.clone()];
    if let Some(previous) = result.deleted_book {
        book_ids.push(previous.id);
    }
    if !state.sync_books(&book_ids).await || !state.sync_series(std::slice::from_ref(&series.id)).await {
        state.recover_search_after_http_failure().await;
    }
    tracing::info!(book_id = %result.book.id, source_file = %payload.source_file, "book import completed");
    TaskOutcome::Completed
}

fn import_failure(state: &AppState, source_file: &str, message: &str, outcome: TaskOutcome) -> TaskOutcome {
    state.publish_domain_event(DomainEvent::BookImported(BookImportEvent {
        book_id: None,
        source_file: source_file.to_owned(),
        success: false,
        message: Some(message.to_owned()),
    }));
    outcome
}

#[derive(Debug)]
struct ImportFailure {
    message: String,
    outcome: TaskOutcome,
}

impl ImportFailure {
    fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome: TaskOutcome::Completed,
        }
    }

    fn retry(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome: TaskOutcome::Retry,
        }
    }
}

struct PreparedImport {
    book: Book,
    sidecars: Vec<ScannedSidecar>,
    removed_sidecar_urls: Vec<String>,
    old_file_deleted: bool,
    files: FileTransaction,
}

impl PreparedImport {
    fn rollback(&mut self) {
        self.files.rollback();
    }

    fn finish(&mut self) {
        self.files.finish();
    }
}

struct PlacedFile {
    destination: PathBuf,
    move_source: Option<PathBuf>,
}

struct StagedFile {
    source: PathBuf,
    temporary: PathBuf,
    moved: bool,
}

#[derive(Default)]
struct FileTransaction {
    staged: Vec<StagedFile>,
    placed: Vec<PlacedFile>,
    backups: Vec<(PathBuf, PathBuf)>,
}

impl FileTransaction {
    fn rollback(&mut self) {
        for placed in self.placed.iter().rev() {
            if let Some(source) = &placed.move_source {
                if let Err(error) = move_file(&placed.destination, source) {
                    tracing::error!(%error, from = %placed.destination.display(), to = %source.display(), "could not restore moved import source");
                }
            } else if let Err(error) = fs::remove_file(&placed.destination)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::error!(%error, path = %placed.destination.display(), "could not remove rolled-back import target");
            }
        }
        for staged in self.staged.iter().rev() {
            if staged.moved && !staged.source.exists() {
                if let Err(error) = move_file(&staged.temporary, &staged.source) {
                    tracing::error!(%error, from = %staged.temporary.display(), to = %staged.source.display(), "could not restore staged import source");
                }
            } else if let Err(error) = fs::remove_file(&staged.temporary)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::error!(%error, path = %staged.temporary.display(), "could not remove staged import file");
            }
        }
        for (original, backup) in self.backups.iter().rev() {
            if let Err(error) = fs::rename(backup, original) {
                tracing::error!(%error, from = %backup.display(), to = %original.display(), "could not restore import backup");
            }
        }
        self.placed.clear();
        self.staged.clear();
        self.backups.clear();
    }

    fn finish(&mut self) {
        for (_, backup) in self.backups.drain(..) {
            if let Err(error) = fs::remove_file(&backup)
                && error.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(%error, path = %backup.display(), "could not remove completed import backup");
            }
        }
        self.placed.clear();
        self.staged.clear();
    }
}

fn prepare_import(
    source: &Path,
    series: &Series,
    library: &Library,
    libraries: &[Library],
    copy_mode: CopyMode,
    destination_name: Option<&str>,
    upgraded_book: Option<&Book>,
) -> Result<PreparedImport, ImportFailure> {
    let source_real = validate_regular_file(source).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ImportFailure::terminal("ERR_1018")
        } else {
            ImportFailure::terminal(format!("ERR_1018: {error}"))
        }
    })?;
    let mut roots = Vec::new();
    for candidate in libraries {
        let Some(path) = candidate.root_path() else {
            continue;
        };
        if let Ok(root) = fs::canonicalize(path) {
            roots.push(root);
        }
    }
    if roots.iter().any(|root| source_real.starts_with(root)) {
        return Err(ImportFailure::terminal("ERR_1019"));
    }
    let root = library
        .root_path()
        .and_then(|path| fs::canonicalize(path).ok())
        .ok_or_else(|| ImportFailure::terminal("Target library root is not accessible"))?;
    let series_path = series
        .url
        .to_file_path()
        .map_err(|()| ImportFailure::terminal("Target series URL is not a local file"))?;
    let destination_directory = if series.oneshot {
        series_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ImportFailure::terminal("One-shot series has no parent directory"))?
    } else {
        series_path
    };
    let destination_directory_real = fs::canonicalize(&destination_directory)
        .map_err(|error| ImportFailure::terminal(format!("Target series directory is not accessible: {error}")))?;
    if !destination_directory_real.starts_with(&root) {
        return Err(ImportFailure::terminal(
            "Target series directory is outside its library",
        ));
    }

    let destination_file_name =
        destination_file_name(source, destination_name).ok_or_else(|| ImportFailure::terminal("ERR_1022"))?;
    let destination = destination_directory.join(destination_file_name);
    let previous_path = upgraded_book
        .map(|book| {
            book.url
                .to_file_path()
                .map_err(|()| ImportFailure::terminal("Upgrade book URL is not a local file"))
        })
        .transpose()?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            validate_path_within_if_present(&destination, &root)?;
            if previous_path.as_deref() != Some(destination.as_path()) {
                return Err(ImportFailure::terminal("ERR_1021"));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ImportFailure::retry(format!(
                "Could not inspect destination file: {error}"
            )));
        }
    }

    let source_sidecars = scan_book_sidecars(source)
        .map_err(|error| ImportFailure::retry(format!("Could not scan source sidecars: {error}")))?;
    for path in &source_sidecars {
        let real = validate_regular_file(path)
            .map_err(|error| ImportFailure::terminal(format!("Invalid source sidecar: {error}")))?;
        if roots.iter().any(|root| real.starts_with(root)) {
            return Err(ImportFailure::terminal("ERR_1019"));
        }
    }
    let destination_sidecars = source_sidecars
        .iter()
        .map(|path| sidecar_destination(path, source, &destination, destination_name))
        .collect::<Result<Vec<_>, _>>()?;

    let mut old_paths = Vec::new();
    let mut removed_sidecar_urls = Vec::new();
    if let Some(path) = &previous_path {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ImportFailure::terminal("Upgrade book path is not a regular local file"));
                }
                let real = fs::canonicalize(path)
                    .map_err(|error| ImportFailure::terminal(format!("Could not resolve upgrade file: {error}")))?;
                if !real.starts_with(&root) {
                    return Err(ImportFailure::terminal("Upgrade book path is outside its library"));
                }
                old_paths.push(path.clone());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ImportFailure::retry(format!("Could not inspect upgrade file: {error}"))),
        }
        if let Ok(sidecars) = scan_book_sidecars(path) {
            for sidecar in sidecars {
                validate_path_within_if_present(&sidecar, &root)?;
                removed_sidecar_urls.push(
                    Url::from_file_path(&sidecar)
                        .map_err(|()| ImportFailure::terminal("Upgrade sidecar is not a local path"))?
                        .to_string(),
                );
                old_paths.push(sidecar);
            }
        }
    }
    for path in &destination_sidecars {
        validate_path_within_if_present(path, &root)?;
        if path.exists() {
            old_paths.push(path.clone());
        }
    }

    let token = komga_core::new_id();
    let sources = std::iter::once(source.to_path_buf())
        .chain(source_sidecars)
        .collect::<Vec<_>>();
    let destinations = std::iter::once(destination.clone())
        .chain(destination_sidecars)
        .collect::<Vec<_>>();
    let mut transaction = stage_files(&sources, &destinations, copy_mode, &token)?;
    let mut seen = BTreeSet::new();
    for original in old_paths {
        if !seen.insert(original.clone()) || !original.exists() {
            continue;
        }
        let backup = sibling_temporary_path(&original, &token, transaction.backups.len(), "bak")?;
        if let Err(error) = fs::rename(&original, &backup) {
            transaction.rollback();
            return Err(ImportFailure::retry(format!(
                "Could not back up existing import target: {error}"
            )));
        }
        transaction.backups.push((original, backup));
    }
    let staged_paths = transaction
        .staged
        .iter()
        .map(|value| value.temporary.clone())
        .collect::<Vec<_>>();
    for (index, staged) in staged_paths.into_iter().enumerate() {
        if let Err(error) = fs::rename(&staged, &destinations[index]) {
            transaction.rollback();
            return Err(ImportFailure::retry(format!(
                "Could not publish imported file: {error}"
            )));
        }
        transaction.placed.push(PlacedFile {
            destination: destinations[index].clone(),
            move_source: (copy_mode == CopyMode::Move).then(|| sources[index].clone()),
        });
    }
    transaction.staged.clear();

    let mut book = match scan_book_file(&destination, &library.id, series.oneshot) {
        Some(book) => book,
        None => {
            transaction.rollback();
            return Err(ImportFailure::terminal("ERR_1022"));
        }
    };
    book.series_id = series.id.clone();
    let imported_sidecar_paths = match scan_book_sidecars(&destination) {
        Ok(paths) => paths,
        Err(error) => {
            transaction.rollback();
            return Err(ImportFailure::retry(format!(
                "Could not scan imported sidecars: {error}"
            )));
        }
    };
    let mut sidecars = Vec::new();
    for path in imported_sidecar_paths {
        match scanned_artwork_sidecar(&path, &book) {
            Ok(sidecar) => sidecars.push(sidecar),
            Err(error) => {
                transaction.rollback();
                return Err(error);
            }
        }
    }
    let old_file_deleted =
        previous_path.is_some_and(|path| transaction.backups.iter().any(|(value, _)| value == &path));
    Ok(PreparedImport {
        book,
        sidecars,
        removed_sidecar_urls,
        old_file_deleted,
        files: transaction,
    })
}

fn validate_regular_file(path: &Path) -> io::Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a non-symlink regular file",
        ));
    }
    fs::canonicalize(path)
}

fn validate_path_within_if_present(path: &Path, root: &Path) -> Result<(), ImportFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ImportFailure::terminal("Existing import target is not a regular file"));
            }
            let real = fs::canonicalize(path)
                .map_err(|error| ImportFailure::terminal(format!("Could not resolve import target: {error}")))?;
            if !real.starts_with(root) {
                return Err(ImportFailure::terminal("Existing import target is outside its library"));
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ImportFailure::retry(format!(
            "Could not inspect import target: {error}"
        ))),
    }
}

fn destination_file_name(source: &Path, destination_name: Option<&str>) -> Option<OsString> {
    if let Some(destination_name) = destination_name {
        let mut name = OsString::from(destination_name);
        name.push(".");
        name.push(source.extension().unwrap_or_else(|| OsStr::new("")));
        Path::new(&name).file_name().map(OsStr::to_os_string)
    } else {
        source.file_name().map(OsStr::to_os_string)
    }
}

fn sidecar_destination(
    sidecar: &Path,
    source: &Path,
    destination: &Path,
    destination_name: Option<&str>,
) -> Result<PathBuf, ImportFailure> {
    let Some(parent) = destination.parent() else {
        return Err(ImportFailure::terminal("ERR_1022"));
    };
    if destination_name.is_none() {
        return sidecar
            .file_name()
            .map(|name| parent.join(name))
            .ok_or_else(|| ImportFailure::terminal("ERR_1022"));
    }
    let source_base = source.file_stem().and_then(|value| value.to_str()).unwrap_or_default();
    let sidecar_base = sidecar.file_stem().and_then(|value| value.to_str()).unwrap_or_default();
    let suffix = sidecar_base
        .get(source_base.len()..)
        .filter(|_| {
            sidecar_base
                .get(..source_base.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(source_base))
        })
        .unwrap_or_default();
    let mut name = OsString::from(destination_name.unwrap_or_default());
    name.push(suffix);
    name.push(".");
    name.push(sidecar.extension().unwrap_or_else(|| OsStr::new("")));
    let name = Path::new(&name)
        .file_name()
        .ok_or_else(|| ImportFailure::terminal("ERR_1022"))?;
    Ok(parent.join(name))
}

fn stage_files(
    sources: &[PathBuf],
    destinations: &[PathBuf],
    copy_mode: CopyMode,
    token: &str,
) -> Result<FileTransaction, ImportFailure> {
    let staged = destinations
        .iter()
        .enumerate()
        .map(|(index, path)| sibling_temporary_path(path, token, index, "tmp"))
        .collect::<Result<Vec<_>, _>>()?;
    let stage = |mode| -> io::Result<()> {
        for ((source, destination), temporary) in sources.iter().zip(destinations).zip(&staged) {
            let _ = destination;
            match mode {
                CopyMode::Copy => {
                    fs::copy(source, temporary)?;
                }
                CopyMode::Hardlink => fs::hard_link(source, temporary)?,
                CopyMode::Move => move_file(source, temporary)?,
            }
        }
        Ok(())
    };
    if let Err(error) = stage(copy_mode) {
        for (index, temporary) in staged.iter().enumerate() {
            if copy_mode == CopyMode::Move && temporary.exists() && !sources[index].exists() {
                let _ = move_file(temporary, &sources[index]);
            } else {
                let _ = fs::remove_file(temporary);
            }
        }
        if copy_mode != CopyMode::Hardlink {
            return Err(ImportFailure::retry(format!("Could not stage imported files: {error}")));
        }
        for (source, temporary) in sources.iter().zip(&staged) {
            if let Err(error) = fs::copy(source, temporary) {
                for path in &staged {
                    let _ = fs::remove_file(path);
                }
                return Err(ImportFailure::retry(format!("Could not copy imported files: {error}")));
            }
        }
    }
    Ok(FileTransaction {
        staged: sources
            .iter()
            .zip(staged)
            .map(|(source, temporary)| StagedFile {
                source: source.clone(),
                temporary,
                moved: copy_mode == CopyMode::Move,
            })
            .collect(),
        ..FileTransaction::default()
    })
}

fn sibling_temporary_path(path: &Path, token: &str, index: usize, suffix: &str) -> Result<PathBuf, ImportFailure> {
    let parent = path.parent().ok_or_else(|| ImportFailure::terminal("ERR_1022"))?;
    Ok(parent.join(format!(".komga-import-{token}-{index}.{suffix}")))
}

fn move_file(source: &Path, destination: &Path) -> io::Result<()> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(source, destination)?;
            fs::remove_file(source)
        }
    }
}

fn scanned_artwork_sidecar(path: &Path, book: &Book) -> Result<ScannedSidecar, ImportFailure> {
    let metadata = fs::metadata(path)
        .map_err(|error| ImportFailure::retry(format!("Could not inspect imported sidecar: {error}")))?;
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    Ok(ScannedSidecar {
        url: Url::from_file_path(path).map_err(|()| ImportFailure::terminal("Imported sidecar is not local"))?,
        parent_url: book.url.clone(),
        last_modified_time: DateTime::<Local>::from(std::cmp::max(modified, created)).naive_local(),
        sidecar_type: SidecarType::Artwork,
        source: SidecarSource::Book,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{Library, PageRequest, scan_library_files};
    use komga_storage::{Storage, TaskStorage};

    use super::*;

    async fn fixture() -> (tempfile::TempDir, AppState, Library, Series, Book) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Volume 1.cbz"), b"old").unwrap();
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        let state = AppState::new(storage, tasks).await.unwrap();
        (temporary, state, library, series, book)
    }

    #[tokio::test]
    async fn copy_import_places_book_and_sidecars_and_persists_follow_up_tasks() {
        let (temporary, state, _library, series, _old) = fixture().await;
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        let source = imports.join("Volume 2.cbz");
        let sidecar = imports.join("Volume 2.jpg");
        fs::write(&source, b"new").unwrap();
        fs::write(&sidecar, b"art").unwrap();
        let task = TaskRecord::import_book(
            source.to_str().unwrap(),
            &series.id,
            CopyMode::Copy,
            Some("Imported 2"),
            None,
        );

        assert_eq!(process_import_book_task(&state, &task).await, TaskOutcome::Completed);
        assert!(source.exists());
        assert!(sidecar.exists());
        let destination = temporary.path().join("library/Series/Imported 2.cbz");
        assert_eq!(fs::read(destination).unwrap(), b"new");
        assert!(temporary.path().join("library/Series/Imported 2.jpg").exists());
        let books = state.storage.list_books_by_series_id(&series.id).await.unwrap();
        assert_eq!(books.len(), 2);
        let imported = books.iter().find(|book| book.name == "Imported 2").unwrap();
        let queued = state.tasks.list().await.unwrap();
        assert!(
            queued
                .iter()
                .any(|task| task.id == format!("ANALYZE_BOOK_{}", imported.id))
        );
        assert!(
            queued
                .iter()
                .any(|task| task.id == format!("REFRESH_BOOK_LOCAL_ARTWORK_{}", imported.id))
        );
        let history = state
            .storage
            .list_historical_events(&PageRequest {
                unpaged: true,
                ..PageRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(history.total_elements, 1);
        assert_eq!(history.content[0].event_type, "BookImported");
    }

    #[tokio::test]
    async fn move_upgrade_replaces_same_path_and_removes_old_identity() {
        let (temporary, state, _library, series, old) = fixture().await;
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        let source = imports.join("replacement.cbz");
        fs::write(&source, b"replacement").unwrap();
        let task = TaskRecord::import_book(
            source.to_str().unwrap(),
            &series.id,
            CopyMode::Move,
            Some("Volume 1"),
            Some(&old.id),
        );

        assert_eq!(process_import_book_task(&state, &task).await, TaskOutcome::Completed);
        assert!(!source.exists());
        assert_eq!(
            fs::read(temporary.path().join("library/Series/Volume 1.cbz")).unwrap(),
            b"replacement"
        );
        assert!(state.storage.find_book_by_id(&old.id).await.unwrap().is_none());
        let books = state.storage.list_books_by_series_id(&series.id).await.unwrap();
        assert_eq!(books.len(), 1);
        assert_ne!(books[0].id, old.id);
        let history = state
            .storage
            .list_historical_events(&PageRequest {
                unpaged: true,
                ..PageRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(history.total_elements, 2);
        let deletion = history
            .content
            .iter()
            .find(|event| event.event_type == "BookFileDeleted")
            .unwrap();
        assert_eq!(deletion.properties["reason"], UPGRADE_DELETE_REASON);
    }

    #[tokio::test]
    async fn prepared_move_upgrade_rolls_back_source_and_replaced_target() {
        let (temporary, _state, library, series, old) = fixture().await;
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        let source = imports.join("replacement.cbz");
        fs::write(&source, b"replacement").unwrap();

        let mut prepared = prepare_import(
            &source,
            &series,
            &library,
            std::slice::from_ref(&library),
            CopyMode::Move,
            Some("Volume 1"),
            Some(&old),
        )
        .unwrap();
        let target = temporary.path().join("library/Series/Volume 1.cbz");
        assert!(!source.exists());
        assert_eq!(fs::read(&target).unwrap(), b"replacement");

        prepared.rollback();
        assert_eq!(fs::read(source).unwrap(), b"replacement");
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[tokio::test]
    async fn import_rejects_library_sources_and_missing_upgrade_ids_without_mutation() {
        let (temporary, state, _library, series, old) = fixture().await;
        let inside = temporary.path().join("library/Series/Volume 1.cbz");
        let task = TaskRecord::import_book(
            inside.to_str().unwrap(),
            &series.id,
            CopyMode::Copy,
            Some("Other"),
            None,
        );
        assert_eq!(process_import_book_task(&state, &task).await, TaskOutcome::Completed);

        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        let source = imports.join("new.cbz");
        fs::write(&source, b"new").unwrap();
        let missing = TaskRecord::import_book(
            source.to_str().unwrap(),
            &series.id,
            CopyMode::Copy,
            None,
            Some("missing"),
        );
        assert_eq!(process_import_book_task(&state, &missing).await, TaskOutcome::Completed);
        assert!(source.exists());
        assert_eq!(state.storage.list_books_by_series_id(&series.id).await.unwrap(), [old]);
        assert_eq!(
            state
                .storage
                .list_historical_events(&PageRequest {
                    unpaged: true,
                    ..PageRequest::default()
                })
                .await
                .unwrap()
                .total_elements,
            0
        );
    }

    #[tokio::test]
    async fn hardlink_import_preserves_source_and_one_shot_upgrade_moves_series_url() {
        let (temporary, state, _library, series, _old) = fixture().await;
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        let source = imports.join("linked.cbz");
        fs::write(&source, b"linked").unwrap();
        let hardlink = TaskRecord::import_book(source.to_str().unwrap(), &series.id, CopyMode::Hardlink, None, None);
        assert_eq!(
            process_import_book_task(&state, &hardlink).await,
            TaskOutcome::Completed
        );
        assert!(source.exists());
        let destination = temporary.path().join("library/Series/linked.cbz");
        assert_eq!(fs::read(destination).unwrap(), b"linked");

        let one_shot_root = temporary.path().join("oneshot-library");
        let one_shot_directory = one_shot_root.join("One Shots");
        fs::create_dir_all(&one_shot_directory).unwrap();
        fs::write(one_shot_directory.join("Single.cbz"), b"old").unwrap();
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let mut library = Library::new("One shots".into(), Url::from_directory_path(&one_shot_root).unwrap());
        library.oneshots_directory = Some("One Shots".into());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let one_shot_series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let previous = storage
            .list_books_by_series_id(&one_shot_series.id)
            .await
            .unwrap()
            .remove(0);
        let state = AppState::new(storage, tasks).await.unwrap();
        let replacement = imports.join("replacement.cbz");
        fs::write(&replacement, b"replacement").unwrap();
        let task = TaskRecord::import_book(
            replacement.to_str().unwrap(),
            &one_shot_series.id,
            CopyMode::Move,
            Some("Replaced"),
            Some(&previous.id),
        );
        assert_eq!(process_import_book_task(&state, &task).await, TaskOutcome::Completed);
        let saved_series = state
            .storage
            .find_series_by_id(&one_shot_series.id)
            .await
            .unwrap()
            .unwrap();
        assert!(saved_series.url.as_str().ends_with("Replaced.cbz"));
        assert_eq!(saved_series.book_count, 1);
        assert!(one_shot_directory.join("Replaced.cbz").exists());
        assert!(!one_shot_directory.join("Single.cbz").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn import_rejects_source_and_destination_symlinks_before_mutation() {
        use std::os::unix::fs::symlink;

        let (temporary, state, _library, series, old) = fixture().await;
        let imports = temporary.path().join("imports");
        fs::create_dir(&imports).unwrap();
        let external = imports.join("external.cbz");
        fs::write(&external, b"external").unwrap();
        let linked_source = imports.join("linked.cbz");
        symlink(&external, &linked_source).unwrap();
        let source_task = TaskRecord::import_book(
            linked_source.to_str().unwrap(),
            &series.id,
            CopyMode::Copy,
            Some("Linked"),
            None,
        );
        assert_eq!(
            process_import_book_task(&state, &source_task).await,
            TaskOutcome::Completed
        );
        assert!(!temporary.path().join("library/Series/Linked.cbz").exists());

        let source = imports.join("new.cbz");
        fs::write(&source, b"new").unwrap();
        let target = temporary.path().join("library/Series/Symlink.cbz");
        symlink(&external, &target).unwrap();
        let target_task = TaskRecord::import_book(
            source.to_str().unwrap(),
            &series.id,
            CopyMode::Copy,
            Some("Symlink"),
            None,
        );
        assert_eq!(
            process_import_book_task(&state, &target_task).await,
            TaskOutcome::Completed
        );
        assert!(target.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(state.storage.list_books_by_series_id(&series.id).await.unwrap(), [old]);
    }
}
