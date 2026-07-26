use std::{collections::BTreeSet, io, path::PathBuf};

use chrono::Local;
use komga_core::{Book, BookEvent, DomainEvent, HistoricalEvent, Series, SeriesEvent};
use komga_search::EntityKind;
use komga_storage::TaskRecord;
use url::Url;

use crate::AppState;

use super::{BookTaskPayload, SeriesTaskPayload, TaskOutcome};

pub(super) async fn process_delete_book_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid DeleteBook payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) => book,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for source deletion");
            return TaskOutcome::Retry;
        }
    };
    if book.oneshot {
        let series = match state.storage.find_series_by_id(&book.series_id).await {
            Ok(Some(series)) => series,
            Ok(None) => return TaskOutcome::Completed,
            Err(error) => {
                tracing::error!(%error, series_id = %book.series_id, "could not load one-shot series for source deletion");
                return TaskOutcome::Retry;
            }
        };
        return delete_series(state, series).await;
    }
    delete_book(state, book).await
}

pub(super) async fn process_delete_series_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<SeriesTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid DeleteSeries payload");
            return TaskOutcome::Completed;
        }
    };
    let series = match state.storage.find_series_by_id(&payload.series_id).await {
        Ok(Some(series)) => series,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, series_id = %payload.series_id, "could not load series for source deletion");
            return TaskOutcome::Retry;
        }
    };
    delete_series(state, series).await
}

async fn delete_book(state: &AppState, book: Book) -> TaskOutcome {
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load library for book source deletion");
            return TaskOutcome::Retry;
        }
    };
    let book_path = match local_path(&book.url) {
        Some(path) => path,
        None => {
            tracing::warn!(book_id = %book.id, url = %book.url, "refusing to delete a non-local book source");
            return TaskOutcome::Completed;
        }
    };
    let root = match canonical_library_root(&library).await {
        Some(root) => root,
        None => return TaskOutcome::Completed,
    };
    if canonical_path_within(&book_path, &root, "book source").await.is_none() {
        return TaskOutcome::Completed;
    }
    let Some(parent) = book_path.parent() else {
        tracing::warn!(book_id = %book.id, path = %book_path.display(), "book source has no parent directory");
        return TaskOutcome::Completed;
    };
    let canonical_parent = match canonical_path_within(&parent.to_owned(), &root, "book parent").await {
        Some(path) => path,
        None => return TaskOutcome::Completed,
    };

    let thumbnails = match state.storage.list_book_thumbnails(&book.id).await {
        Ok(thumbnails) => thumbnails,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load book sidecars for source deletion");
            return TaskOutcome::Retry;
        }
    };
    let sidecars = match sidecar_paths(
        thumbnails
            .into_iter()
            .filter(|thumbnail| thumbnail.thumbnail_type == "SIDECAR")
            .filter_map(|thumbnail| thumbnail.url),
    ) {
        Some(paths) => paths,
        None => {
            tracing::warn!(book_id = %book.id, "refusing to delete a book with a non-local sidecar URL");
            return TaskOutcome::Completed;
        }
    };
    if !validate_optional_paths(&sidecars, &root).await {
        return TaskOutcome::Completed;
    }

    if let Err(error) = tokio::fs::remove_file(&book_path).await {
        tracing::warn!(%error, book_id = %book.id, path = %book_path.display(), "could not delete book source file");
        return TaskOutcome::Completed;
    }
    tracing::info!(book_id = %book.id, path = %book_path.display(), "deleted book source file");
    delete_sidecars(&sidecars).await;

    let mut events = vec![HistoricalEvent::book_file_deleted(&book)];
    if canonical_parent != root && directory_is_empty(parent).await {
        match tokio::fs::remove_dir(parent).await {
            Ok(()) => events.push(HistoricalEvent::series_folder_deleted(&book.series_id, parent)),
            Err(error) => {
                tracing::warn!(%error, path = %parent.display(), "could not remove empty book parent directory");
            }
        }
    }

    if let Err(error) = state
        .storage
        .commit_source_deletion(std::slice::from_ref(&book.id), &[], &events, Local::now().naive_local())
        .await
    {
        tracing::error!(%error, book_id = %book.id, "could not persist book source deletion");
        return TaskOutcome::Retry;
    }
    state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(&book)));
    if !state
        .delete_search_entities(vec![(EntityKind::Book, book.id.clone())])
        .await
    {
        state.recover_search_after_http_failure().await;
    }
    TaskOutcome::Completed
}

async fn delete_series(state: &AppState, series: Series) -> TaskOutcome {
    let library = match state.storage.find_library_by_id(&series.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %series.library_id, "could not load library for series source deletion");
            return TaskOutcome::Retry;
        }
    };
    let series_path = match local_path(&series.url) {
        Some(path) => path,
        None => {
            tracing::warn!(series_id = %series.id, url = %series.url, "refusing to delete a non-local series source");
            return TaskOutcome::Completed;
        }
    };
    let root = match canonical_library_root(&library).await {
        Some(root) => root,
        None => return TaskOutcome::Completed,
    };
    let series_metadata = match tokio::fs::metadata(&series_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::warn!(series_id = %series.id, path = %series_path.display(), "series source path does not exist");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::warn!(%error, series_id = %series.id, path = %series_path.display(), "could not inspect series source path");
            return TaskOutcome::Completed;
        }
    };
    let canonical_series = match canonical_path_within(&series_path, &root, "series source").await {
        Some(path) => path,
        None => return TaskOutcome::Completed,
    };

    let books = match state.storage.list_books_by_series_id(&series.id).await {
        Ok(books) => books,
        Err(error) => {
            tracing::error!(%error, series_id = %series.id, "could not load books for series source deletion");
            return TaskOutcome::Retry;
        }
    };
    let mut book_paths = Vec::with_capacity(books.len());
    let mut sidecars = Vec::new();
    for book in &books {
        let Some(path) = local_path(&book.url) else {
            tracing::warn!(book_id = %book.id, url = %book.url, "refusing to delete a series with a non-local book source");
            return TaskOutcome::Completed;
        };
        book_paths.push(path);
        let thumbnails = match state.storage.list_book_thumbnails(&book.id).await {
            Ok(thumbnails) => thumbnails,
            Err(error) => {
                tracing::error!(%error, book_id = %book.id, "could not load book sidecars for series deletion");
                return TaskOutcome::Retry;
            }
        };
        let Some(paths) = sidecar_paths(
            thumbnails
                .into_iter()
                .filter(|thumbnail| thumbnail.thumbnail_type == "SIDECAR")
                .filter_map(|thumbnail| thumbnail.url),
        ) else {
            tracing::warn!(book_id = %book.id, "refusing to delete a series with a non-local book sidecar URL");
            return TaskOutcome::Completed;
        };
        sidecars.extend(paths);
    }
    let series_thumbnails = match state.storage.list_series_thumbnails(&series.id).await {
        Ok(thumbnails) => thumbnails,
        Err(error) => {
            tracing::error!(%error, series_id = %series.id, "could not load series sidecars for source deletion");
            return TaskOutcome::Retry;
        }
    };
    let Some(series_sidecars) = sidecar_paths(
        series_thumbnails
            .into_iter()
            .filter(|thumbnail| thumbnail.thumbnail_type == "SIDECAR")
            .filter_map(|thumbnail| thumbnail.url),
    ) else {
        tracing::warn!(series_id = %series.id, "refusing to delete a series with a non-local series sidecar URL");
        return TaskOutcome::Completed;
    };
    sidecars.extend(series_sidecars);

    if !validate_optional_paths(&book_paths, &root).await || !validate_optional_paths(&sidecars, &root).await {
        return TaskOutcome::Completed;
    }

    let mut events = Vec::new();
    for (book, path) in books.iter().zip(&book_paths) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                tracing::info!(book_id = %book.id, path = %path.display(), "deleted book source file");
                events.push(HistoricalEvent::book_file_deleted(book));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, book_id = %book.id, path = %path.display(), "could not delete series book source");
            }
        }
    }
    delete_sidecars(&sidecars).await;

    let removable_directory = if series_metadata.is_dir() {
        Some((series_path.as_path(), canonical_series))
    } else {
        series_path
            .parent()
            .map(|parent| (parent, canonical_series.parent().unwrap_or(&root).to_owned()))
    };
    if let Some((directory, canonical_directory)) = removable_directory
        && canonical_directory != root
        && directory_is_empty(directory).await
    {
        match tokio::fs::remove_dir(directory).await {
            Ok(()) => events.push(HistoricalEvent::series_folder_deleted(&series.id, directory)),
            Err(error) => {
                tracing::warn!(%error, series_id = %series.id, path = %directory.display(), "could not remove empty series directory");
            }
        }
    }

    let book_ids = books.iter().map(|book| book.id.clone()).collect::<Vec<_>>();
    if let Err(error) = state
        .storage
        .commit_source_deletion(
            &book_ids,
            std::slice::from_ref(&series.id),
            &events,
            Local::now().naive_local(),
        )
        .await
    {
        tracing::error!(%error, series_id = %series.id, "could not persist series source deletion");
        return TaskOutcome::Retry;
    }
    for book in &books {
        state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(book)));
    }
    state.publish_domain_event(DomainEvent::SeriesUpdated(SeriesEvent::from(&series)));
    let search_entities = book_ids
        .into_iter()
        .map(|id| (EntityKind::Book, id))
        .chain(std::iter::once((EntityKind::Series, series.id.clone())))
        .collect();
    if !state.delete_search_entities(search_entities).await {
        state.recover_search_after_http_failure().await;
    }
    TaskOutcome::Completed
}

fn local_path(url: &Url) -> Option<PathBuf> {
    url.to_file_path().ok()
}

fn sidecar_paths(urls: impl IntoIterator<Item = String>) -> Option<Vec<PathBuf>> {
    urls.into_iter()
        .map(|value| Url::parse(&value).ok()?.to_file_path().ok())
        .collect()
}

async fn canonical_library_root(library: &komga_core::Library) -> Option<PathBuf> {
    let root = library.root_path()?;
    match tokio::fs::canonicalize(&root).await {
        Ok(root) => Some(root),
        Err(error) => {
            tracing::warn!(%error, library_id = %library.id, path = %root.display(), "could not canonicalize library root for source deletion");
            None
        }
    }
}

async fn canonical_path_within(path: &PathBuf, root: &PathBuf, kind: &str) -> Option<PathBuf> {
    match tokio::fs::canonicalize(path).await {
        Ok(canonical) if canonical.starts_with(root) => Some(canonical),
        Ok(canonical) => {
            tracing::warn!(path = %path.display(), real_path = %canonical.display(), root = %root.display(), %kind, "refusing source deletion outside library root");
            None
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), %kind, "could not canonicalize required source-deletion path");
            None
        }
    }
}

async fn validate_optional_paths(paths: &[PathBuf], root: &PathBuf) -> bool {
    for path in paths {
        match tokio::fs::canonicalize(path).await {
            Ok(canonical) if canonical.starts_with(root) => {}
            Ok(canonical) => {
                tracing::warn!(path = %path.display(), real_path = %canonical.display(), root = %root.display(), "refusing source deletion outside library root");
                return false;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not canonicalize source-deletion path");
                return false;
            }
        }
    }
    true
}

async fn delete_sidecars(paths: &[PathBuf]) {
    let mut deleted = BTreeSet::new();
    for path in paths {
        if !deleted.insert(path.clone()) {
            continue;
        }
        match tokio::fs::remove_file(path).await {
            Ok(()) => tracing::info!(path = %path.display(), "deleted source sidecar"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, path = %path.display(), "could not delete source sidecar"),
        }
    }
}

async fn directory_is_empty(path: &std::path::Path) -> bool {
    match tokio::fs::read_dir(path).await {
        Ok(mut entries) => match entries.next_entry().await {
            Ok(None) => true,
            Ok(Some(_)) => false,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not inspect source directory after deletion");
                false
            }
        },
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not open source directory after deletion");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use komga_core::{
        Dimension, DomainEvent, Library, PageRequest, ThumbnailBook, ThumbnailSeries, scan_library_files,
    };
    use komga_storage::{Storage, TaskStorage};

    use super::*;

    async fn scanned_state(
        root: &Path,
        configure: impl FnOnce(&mut Library),
    ) -> (AppState, Storage, komga_core::Series, Vec<Book>) {
        let storage = Storage::connect_memory().await.unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(root).unwrap());
        configure(&mut library);
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let books = storage.list_books_by_series_id(&series.id).await.unwrap();
        let state = AppState::new(storage.clone(), TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap();
        (state, storage, series, books)
    }

    async fn history(storage: &Storage) -> Vec<HistoricalEvent> {
        storage
            .list_historical_events(&PageRequest {
                unpaged: true,
                ..PageRequest::default()
            })
            .await
            .unwrap()
            .content
    }

    fn book_sidecar(book_id: &str, id: &str, path: &Path) -> ThumbnailBook {
        ThumbnailBook {
            id: id.into(),
            thumbnail: None,
            url: Some(Url::from_file_path(path).unwrap().to_string()),
            selected: false,
            thumbnail_type: "SIDECAR".into(),
            media_type: "image/jpeg".into(),
            file_size: 1,
            dimension: Dimension { width: 1, height: 1 },
            book_id: book_id.into(),
        }
    }

    fn series_sidecar(series_id: &str, path: &Path) -> ThumbnailSeries {
        ThumbnailSeries {
            id: "series-sidecar".into(),
            thumbnail: None,
            url: Some(Url::from_file_path(path).unwrap().to_string()),
            selected: false,
            thumbnail_type: "SIDECAR".into(),
            media_type: "image/jpeg".into(),
            file_size: 1,
            dimension: Dimension { width: 1, height: 1 },
            series_id: series_id.into(),
        }
    }

    #[tokio::test]
    async fn book_deletion_removes_sidecars_and_empty_parent_but_never_library_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Volume.cbz"), [1]).unwrap();
        let sidecar = series_path.join("cover.jpg");
        fs::write(&sidecar, [2]).unwrap();
        let missing_sidecar = series_path.join("missing.jpg");
        let (state, storage, series, books) = scanned_state(&root, |_| {}).await;
        let book = &books[0];
        storage
            .insert_book_thumbnail(&book_sidecar(&book.id, "sidecar", &sidecar), false)
            .await
            .unwrap();
        storage
            .insert_book_thumbnail(&book_sidecar(&book.id, "missing", &missing_sidecar), false)
            .await
            .unwrap();
        let mut events = state.subscribe_domain_events();

        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&book.id)).await,
            TaskOutcome::Completed
        );
        assert!(!book.url.to_file_path().unwrap().exists());
        assert!(!sidecar.exists());
        assert!(!series_path.exists());
        assert!(root.exists());
        assert!(
            storage
                .find_book_by_id(&book.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date
                .is_some()
        );
        assert_eq!(
            storage
                .find_series_by_id(&series.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date,
            None
        );
        let history = history(&storage).await;
        assert_eq!(history.len(), 2);
        assert!(history.iter().any(|event| event.event_type == "BookFileDeleted"));
        assert!(history.iter().any(|event| event.event_type == "SeriesFolderDeleted"));
        assert!(matches!(
            events.try_recv().unwrap().as_ref(),
            DomainEvent::BookUpdated(event) if event.book_id == book.id
        ));
        assert!(state.search.search(EntityKind::Book, "Volume").is_empty());

        let root_book = root.join("Root.cbz");
        fs::write(&root_book, [3]).unwrap();
        let library = storage.find_library_by_id(&book.library_id).await.unwrap().unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let root_book = storage
            .list_books_by_library_id(&library.id)
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.name == "Root")
            .unwrap();
        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&root_book.id)).await,
            TaskOutcome::Completed
        );
        assert!(root.exists());
    }

    #[tokio::test]
    async fn book_deletion_missing_or_unremovable_sources_leave_database_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Missing.cbz"), [1]).unwrap();
        fs::write(series_path.join("Directory.cbz"), [2]).unwrap();
        let (state, storage, _series, books) = scanned_state(&root, |_| {}).await;
        let missing = books.iter().find(|book| book.name == "Missing").unwrap();
        let directory = books.iter().find(|book| book.name == "Directory").unwrap();
        fs::remove_file(missing.url.to_file_path().unwrap()).unwrap();
        let directory_path = directory.url.to_file_path().unwrap();
        fs::remove_file(&directory_path).unwrap();
        fs::create_dir(&directory_path).unwrap();

        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&missing.id)).await,
            TaskOutcome::Completed
        );
        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&directory.id)).await,
            TaskOutcome::Completed
        );
        assert_eq!(
            storage
                .find_book_by_id(&missing.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date,
            None
        );
        assert_eq!(
            storage
                .find_book_by_id(&directory.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date,
            None
        );
        assert!(history(&storage).await.is_empty());
    }

    #[tokio::test]
    async fn series_deletion_removes_registered_sources_and_preserves_unknown_files() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("First.cbz"), [1]).unwrap();
        fs::write(series_path.join("Second.cbz"), [2]).unwrap();
        let book_sidecar_path = series_path.join("first-cover.jpg");
        let series_sidecar_path = series_path.join("series-cover.jpg");
        let unknown = series_path.join("notes.txt");
        fs::write(&book_sidecar_path, [3]).unwrap();
        fs::write(&series_sidecar_path, [4]).unwrap();
        fs::write(&unknown, [5]).unwrap();
        let (state, storage, series, books) = scanned_state(&root, |_| {}).await;
        storage
            .insert_book_thumbnail(&book_sidecar(&books[0].id, "book-sidecar", &book_sidecar_path), false)
            .await
            .unwrap();
        storage
            .insert_series_thumbnail(&series_sidecar(&series.id, &series_sidecar_path), false)
            .await
            .unwrap();
        let mut events = state.subscribe_domain_events();

        assert_eq!(
            process_delete_series_task(&state, &TaskRecord::delete_series(&series.id)).await,
            TaskOutcome::Completed
        );
        assert!(unknown.exists());
        assert!(series_path.exists());
        assert!(!book_sidecar_path.exists());
        assert!(!series_sidecar_path.exists());
        for book in &books {
            assert!(!book.url.to_file_path().unwrap().exists());
            assert!(
                storage
                    .find_book_by_id(&book.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .deleted_date
                    .is_some()
            );
        }
        assert!(
            storage
                .find_series_by_id(&series.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date
                .is_some()
        );
        let history = history(&storage).await;
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|event| event.event_type == "BookFileDeleted"));
        for book in &books {
            assert!(matches!(
                events.try_recv().unwrap().as_ref(),
                DomainEvent::BookUpdated(event) if event.book_id == book.id
            ));
        }
        assert!(matches!(
            events.try_recv().unwrap().as_ref(),
            DomainEvent::SeriesUpdated(event) if event.series_id == series.id
        ));
        assert!(state.search.search(EntityKind::Series, "Series").is_empty());
    }

    #[tokio::test]
    async fn one_shot_book_deletion_soft_deletes_its_series_and_empty_parent() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let oneshots = root.join("One Shots");
        fs::create_dir_all(&oneshots).unwrap();
        fs::write(oneshots.join("Standalone.cbz"), [1]).unwrap();
        let (state, storage, series, books) = scanned_state(&root, |library| {
            library.oneshots_directory = Some("One Shots".into());
        })
        .await;

        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&books[0].id)).await,
            TaskOutcome::Completed
        );
        assert!(!oneshots.exists());
        assert!(root.exists());
        assert!(
            storage
                .find_book_by_id(&books[0].id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date
                .is_some()
        );
        assert!(
            storage
                .find_series_by_id(&series.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date
                .is_some()
        );
        let history = history(&storage).await;
        assert_eq!(history.len(), 2);
        assert!(history.iter().any(|event| event.event_type == "BookFileDeleted"));
        assert!(history.iter().any(|event| event.event_type == "SeriesFolderDeleted"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn deletion_rejects_external_file_sidecar_and_directory_symlinks_before_unlinking() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Volume.cbz"), [1]).unwrap();
        let (state, storage, series, books) = scanned_state(&root, |_| {}).await;
        let book = &books[0];
        let external_file = external.path().join("outside.cbz");
        fs::write(&external_file, [2]).unwrap();
        let book_path = book.url.to_file_path().unwrap();
        fs::remove_file(&book_path).unwrap();
        symlink(&external_file, &book_path).unwrap();

        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&book.id)).await,
            TaskOutcome::Completed
        );
        assert!(external_file.exists());
        assert!(book_path.exists());
        assert_eq!(
            storage.find_book_by_id(&book.id).await.unwrap().unwrap().deleted_date,
            None
        );

        fs::remove_file(&book_path).unwrap();
        fs::write(&book_path, [3]).unwrap();
        let external_sidecar = external.path().join("outside.jpg");
        fs::write(&external_sidecar, [4]).unwrap();
        let sidecar_link = series_path.join("cover.jpg");
        symlink(&external_sidecar, &sidecar_link).unwrap();
        storage
            .insert_book_thumbnail(&book_sidecar(&book.id, "external-sidecar", &sidecar_link), false)
            .await
            .unwrap();
        assert_eq!(
            process_delete_book_task(&state, &TaskRecord::delete_book(&book.id)).await,
            TaskOutcome::Completed
        );
        assert!(book_path.exists());
        assert!(external_sidecar.exists());

        fs::remove_file(&sidecar_link).unwrap();
        fs::remove_file(&book_path).unwrap();
        fs::remove_dir(&series_path).unwrap();
        let external_series = external.path().join("Series");
        fs::create_dir(&external_series).unwrap();
        fs::write(external_series.join("Volume.cbz"), [5]).unwrap();
        symlink(&external_series, &series_path).unwrap();
        assert_eq!(
            process_delete_series_task(&state, &TaskRecord::delete_series(&series.id)).await,
            TaskOutcome::Completed
        );
        assert!(external_series.join("Volume.cbz").exists());
        assert_eq!(
            storage
                .find_series_by_id(&series.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date,
            None
        );
        assert!(history(&storage).await.is_empty());
    }

    #[tokio::test]
    async fn deletion_rejects_non_local_content_urls() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Volume.cbz"), [1]).unwrap();
        let (state, storage, series, books) = scanned_state(&root, |_| {}).await;
        let mut remote_book = books[0].clone();
        remote_book.url = Url::parse("https://example.org/Volume.cbz").unwrap();
        assert_eq!(delete_book(&state, remote_book).await, TaskOutcome::Completed);
        assert!(books[0].url.to_file_path().unwrap().exists());

        let mut remote_series = series.clone();
        remote_series.url = Url::parse("https://example.org/Series").unwrap();
        assert_eq!(delete_series(&state, remote_series).await, TaskOutcome::Completed);
        assert!(books[0].url.to_file_path().unwrap().exists());
        assert_eq!(
            storage
                .find_book_by_id(&books[0].id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date,
            None
        );
        assert_eq!(
            storage
                .find_series_by_id(&series.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date,
            None
        );
        assert!(history(&storage).await.is_empty());
    }
}
