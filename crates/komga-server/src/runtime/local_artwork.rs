use std::fs;

use komga_core::{
    BookThumbnailEvent, DomainEvent, SeriesThumbnailEvent, ThumbnailBook, ThumbnailSeries, new_id, scan_book_sidecars,
    scan_series_sidecars,
};
use komga_media::{detect_image_media_type, image_dimension};
use komga_storage::TaskRecord;
use url::Url;

use crate::AppState;

use super::{BookTaskPayload, SeriesTaskPayload, TaskOutcome};

pub(super) async fn process_refresh_book_local_artwork_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid RefreshBookLocalArtwork payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for local artwork refresh");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) if library.import_local_artwork => library,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load local artwork settings");
            return TaskOutcome::Retry;
        }
    };
    let Some(root) = library.root_path().and_then(|path| fs::canonicalize(path).ok()) else {
        tracing::warn!(library_id = %library.id, "local artwork library root is unavailable");
        return TaskOutcome::Completed;
    };
    let Ok(book_path) = book.url.to_file_path() else {
        tracing::warn!(book_id = %book.id, "local artwork book URL is not local");
        return TaskOutcome::Completed;
    };
    let paths = match scan_book_sidecars(&book_path) {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(%error, book_id = %book.id, "could not scan local book artwork");
            return TaskOutcome::Completed;
        }
    };
    let mut accepted = 0usize;
    for path in paths {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let Ok(real) = fs::canonicalize(&path) else {
            continue;
        };
        if !real.starts_with(&root) {
            tracing::warn!(book_id = %book.id, path = %path.display(), "refusing local artwork outside library root");
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not read local artwork");
                continue;
            }
        };
        let Some(media_type) = detect_image_media_type(&bytes) else {
            tracing::warn!(path = %path.display(), "local artwork is not a supported image");
            continue;
        };
        let thumbnail = ThumbnailBook {
            id: new_id(),
            thumbnail: None,
            url: Some(
                Url::from_file_path(&path)
                    .expect("canonical local artwork path must convert to URL")
                    .to_string(),
            ),
            selected: false,
            thumbnail_type: "SIDECAR".into(),
            media_type,
            file_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            dimension: image_dimension(&bytes).unwrap_or(komga_core::Dimension { width: 0, height: 0 }),
            book_id: book.id.clone(),
        };
        match state
            .storage
            .upsert_book_sidecar_thumbnail(&thumbnail, accepted == 0)
            .await
        {
            Ok(saved) => {
                accepted += 1;
                state.publish_domain_event(DomainEvent::ThumbnailBookAdded(BookThumbnailEvent {
                    book_id: book.id.clone(),
                    series_id: book.series_id.clone(),
                    selected: saved.selected,
                }));
            }
            Err(error) => {
                tracing::error!(%error, book_id = %book.id, path = %path.display(), "could not persist local artwork");
                return TaskOutcome::Retry;
            }
        }
    }
    TaskOutcome::Completed
}

pub(super) async fn process_refresh_series_local_artwork_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<SeriesTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid RefreshSeriesLocalArtwork payload");
            return TaskOutcome::Completed;
        }
    };
    let series = match state.storage.find_series_by_id(&payload.series_id).await {
        Ok(Some(series)) if series.deleted_date.is_none() && !series.oneshot => series,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, series_id = %payload.series_id, "could not load series for local artwork refresh");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&series.library_id).await {
        Ok(Some(library)) if library.import_local_artwork => library,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %series.library_id, "could not load local artwork settings");
            return TaskOutcome::Retry;
        }
    };
    let Some(root) = library.root_path().and_then(|path| fs::canonicalize(path).ok()) else {
        tracing::warn!(library_id = %library.id, "local artwork library root is unavailable");
        return TaskOutcome::Completed;
    };
    let Ok(series_path) = series.url.to_file_path() else {
        tracing::warn!(series_id = %series.id, "local artwork series URL is not local");
        return TaskOutcome::Completed;
    };
    let Ok(real_series_path) = fs::canonicalize(&series_path) else {
        tracing::warn!(series_id = %series.id, path = %series_path.display(), "local artwork series directory is unavailable");
        return TaskOutcome::Completed;
    };
    if !real_series_path.is_dir() || !real_series_path.starts_with(&root) {
        tracing::warn!(series_id = %series.id, path = %series_path.display(), "refusing local artwork series directory outside library root");
        return TaskOutcome::Completed;
    }
    let paths = match scan_series_sidecars(&series_path) {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(%error, series_id = %series.id, "could not scan local series artwork");
            return TaskOutcome::Completed;
        }
    };
    let mut accepted = 0usize;
    for path in paths {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            tracing::warn!(series_id = %series.id, path = %path.display(), "local series artwork is not a regular file");
            continue;
        }
        let Ok(real) = fs::canonicalize(&path) else {
            continue;
        };
        if !real.starts_with(&root) {
            tracing::warn!(series_id = %series.id, path = %path.display(), "refusing local artwork outside library root");
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not read local artwork");
                continue;
            }
        };
        let Some(media_type) = detect_image_media_type(&bytes) else {
            tracing::warn!(path = %path.display(), "local artwork is not a supported image");
            continue;
        };
        let thumbnail = ThumbnailSeries {
            id: new_id(),
            thumbnail: None,
            url: Some(
                Url::from_file_path(&path)
                    .expect("local series artwork path must convert to URL")
                    .to_string(),
            ),
            selected: false,
            thumbnail_type: "SIDECAR".into(),
            media_type,
            file_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            dimension: image_dimension(&bytes).unwrap_or(komga_core::Dimension { width: 0, height: 0 }),
            series_id: series.id.clone(),
        };
        match state
            .storage
            .upsert_series_sidecar_thumbnail(&thumbnail, accepted == 0)
            .await
        {
            Ok(saved) => {
                accepted += 1;
                state.publish_domain_event(DomainEvent::ThumbnailSeriesAdded(SeriesThumbnailEvent {
                    series_id: series.id.clone(),
                    selected: saved.selected,
                }));
            }
            Err(error) => {
                tracing::error!(%error, series_id = %series.id, path = %path.display(), "could not persist local series artwork");
                return TaskOutcome::Retry;
            }
        }
    }
    TaskOutcome::Completed
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{DomainEvent, Library, ScanInterval, scan_library_files};
    use komga_storage::{Storage, TaskStorage};

    use super::*;

    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0, 0, 0, 181, 28,
        12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1, 39, 24, 227, 102, 0, 0, 0, 0,
        73, 69, 78, 68, 174, 66, 96, 130,
    ];
    const GIF_1X1: &[u8] = &[
        71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255, 33, 249, 4, 1, 0, 0, 0, 0, 44, 0, 0, 0,
        0, 1, 0, 1, 0, 0, 2, 2, 68, 1, 0, 59,
    ];

    async fn scanned_state(root: &std::path::Path, mut library: Library) -> (AppState, komga_core::Series) {
        library.scan_interval = ScanInterval::Disabled;
        let storage = Storage::connect_memory().await.unwrap();
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let state = AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap();
        assert!(root.exists());
        (state, series)
    }

    #[tokio::test]
    async fn local_artwork_refresh_persists_sidecar_thumbnail_and_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("Book.cbz"), b"book").unwrap();
        fs::write(series.join("Book.jpg"), PNG_1X1).unwrap();
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

        assert_eq!(
            process_refresh_book_local_artwork_task(&state, &TaskRecord::refresh_book_local_artwork(&book.id, 4)).await,
            TaskOutcome::Completed
        );
        let thumbnails = state.storage.list_book_thumbnails(&book.id).await.unwrap();
        assert_eq!(thumbnails.len(), 1);
        assert_eq!(thumbnails[0].thumbnail_type, "SIDECAR");
        assert_eq!(thumbnails[0].media_type, "image/png");
        assert_eq!(thumbnails[0].dimension, komga_core::Dimension { width: 1, height: 1 });
        assert!(thumbnails[0].selected);
    }

    #[tokio::test]
    async fn series_local_artwork_imports_valid_images_and_publishes_one_event_each() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Book.cbz"), b"book").unwrap();
        fs::write(series_path.join("cover.jpg"), b"not an image").unwrap();
        fs::write(series_path.join("default.png"), PNG_1X1).unwrap();
        fs::write(series_path.join("folder.gif"), GIF_1X1).unwrap();
        fs::write(series_path.join("poster.jpg"), PNG_1X1).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        let (state, series) = scanned_state(&root, library).await;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(series_path.join("poster.jpg"), fs::Permissions::from_mode(0o000)).unwrap();
        }
        let poster_unreadable = fs::read(series_path.join("poster.jpg")).is_err();
        let mut events = state.subscribe_domain_events();
        assert_eq!(
            process_refresh_series_local_artwork_task(&state, &TaskRecord::refresh_series_local_artwork(&series.id, 4))
                .await,
            TaskOutcome::Completed
        );

        let thumbnails = state.storage.list_series_thumbnails(&series.id).await.unwrap();
        assert_eq!(thumbnails.len(), if poster_unreadable { 2 } else { 3 });
        let default = thumbnails
            .iter()
            .find(|thumbnail| thumbnail.url.as_deref().is_some_and(|url| url.ends_with("default.png")))
            .unwrap();
        assert_eq!(default.media_type, "image/png");
        assert_eq!(default.dimension, komga_core::Dimension { width: 1, height: 1 });
        assert!(default.selected);
        let folder = thumbnails
            .iter()
            .find(|thumbnail| thumbnail.url.as_deref().is_some_and(|url| url.ends_with("folder.gif")))
            .unwrap();
        assert_eq!(folder.media_type, "image/gif");
        assert_eq!(folder.dimension, komga_core::Dimension { width: 1, height: 1 });
        assert!(!folder.selected);
        for index in 0..thumbnails.len() {
            let event = events.try_recv().unwrap();
            let DomainEvent::ThumbnailSeriesAdded(event) = event.as_ref() else {
                panic!("unexpected event {index}: {event:?}");
            };
            assert_eq!(event.series_id, series.id);
            assert_eq!(event.selected, index == 0);
        }
        assert!(events.try_recv().is_err());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(series_path.join("poster.jpg"), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[tokio::test]
    async fn series_local_artwork_silently_skips_ineligible_series_and_unsafe_paths() {
        let missing_state = AppState::new(
            Storage::connect_memory().await.unwrap(),
            TaskStorage::connect_memory().await.unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            process_refresh_series_local_artwork_task(
                &missing_state,
                &TaskRecord::refresh_series_local_artwork("missing", 4)
            )
            .await,
            TaskOutcome::Completed
        );

        let deleted_temporary = tempfile::tempdir().unwrap();
        let deleted_root = deleted_temporary.path().join("library");
        let deleted_series_path = deleted_root.join("Series");
        fs::create_dir_all(&deleted_series_path).unwrap();
        let deleted_book = deleted_series_path.join("Book.cbz");
        fs::write(&deleted_book, b"book").unwrap();
        fs::write(deleted_series_path.join("cover.jpg"), PNG_1X1).unwrap();
        let deleted_library = Library::new("Deleted".into(), Url::from_directory_path(&deleted_root).unwrap());
        let storage = Storage::connect_memory().await.unwrap();
        storage.create_library(&deleted_library).await.unwrap();
        storage
            .apply_library_scan(
                &deleted_library.id,
                &scan_library_files(&deleted_library, || false).unwrap(),
                false,
            )
            .await
            .unwrap();
        let deleted_series = storage
            .list_series_by_library_id(&deleted_library.id)
            .await
            .unwrap()
            .remove(0);
        fs::remove_file(deleted_book).unwrap();
        storage
            .apply_library_scan(
                &deleted_library.id,
                &scan_library_files(&deleted_library, || false).unwrap(),
                false,
            )
            .await
            .unwrap();
        let deleted_state = AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap();
        assert_eq!(
            process_refresh_series_local_artwork_task(
                &deleted_state,
                &TaskRecord::refresh_series_local_artwork(&deleted_series.id, 4)
            )
            .await,
            TaskOutcome::Completed
        );
        assert!(
            deleted_state
                .storage
                .list_series_thumbnails(&deleted_series.id)
                .await
                .unwrap()
                .is_empty()
        );

        let disabled_temporary = tempfile::tempdir().unwrap();
        let disabled_root = disabled_temporary.path().join("library");
        let disabled_series_path = disabled_root.join("Series");
        fs::create_dir_all(&disabled_series_path).unwrap();
        fs::write(disabled_series_path.join("Book.cbz"), b"book").unwrap();
        fs::write(disabled_series_path.join("cover.jpg"), PNG_1X1).unwrap();
        let mut disabled_library = Library::new("Disabled".into(), Url::from_directory_path(&disabled_root).unwrap());
        disabled_library.import_local_artwork = false;
        let (disabled_state, disabled_series) = scanned_state(&disabled_root, disabled_library).await;
        assert_eq!(
            process_refresh_series_local_artwork_task(
                &disabled_state,
                &TaskRecord::refresh_series_local_artwork(&disabled_series.id, 4)
            )
            .await,
            TaskOutcome::Completed
        );
        assert!(
            disabled_state
                .storage
                .list_series_thumbnails(&disabled_series.id)
                .await
                .unwrap()
                .is_empty()
        );

        let oneshot_temporary = tempfile::tempdir().unwrap();
        let oneshot_root = oneshot_temporary.path().join("library");
        let oneshot_path = oneshot_root.join("One Shots");
        fs::create_dir_all(&oneshot_path).unwrap();
        fs::write(oneshot_path.join("Book.cbz"), b"book").unwrap();
        fs::write(oneshot_path.join("cover.jpg"), PNG_1X1).unwrap();
        let mut oneshot_library = Library::new("Oneshot".into(), Url::from_directory_path(&oneshot_root).unwrap());
        oneshot_library.oneshots_directory = Some("one shots".into());
        let (oneshot_state, oneshot) = scanned_state(&oneshot_root, oneshot_library).await;
        assert!(oneshot.oneshot);
        assert_eq!(
            process_refresh_series_local_artwork_task(
                &oneshot_state,
                &TaskRecord::refresh_series_local_artwork(&oneshot.id, 4)
            )
            .await,
            TaskOutcome::Completed
        );
        assert!(
            oneshot_state
                .storage
                .list_series_thumbnails(&oneshot.id)
                .await
                .unwrap()
                .is_empty()
        );

        let non_local_temporary = tempfile::tempdir().unwrap();
        let non_local_root = non_local_temporary.path().join("library");
        let non_local_path = non_local_root.join("Series");
        fs::create_dir_all(&non_local_path).unwrap();
        fs::write(non_local_path.join("Book.cbz"), b"book").unwrap();
        let non_local_library = Library::new("Non-local".into(), Url::from_directory_path(&non_local_root).unwrap());
        let storage = Storage::connect_memory().await.unwrap();
        storage.create_library(&non_local_library).await.unwrap();
        let mut scan = scan_library_files(&non_local_library, || false).unwrap();
        scan.series[0].series.url = Url::parse("https://example.org/Series").unwrap();
        storage
            .apply_library_scan(&non_local_library.id, &scan, false)
            .await
            .unwrap();
        let non_local = storage
            .list_series_by_library_id(&non_local_library.id)
            .await
            .unwrap()
            .remove(0);
        let non_local_state = AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap();
        assert_eq!(
            process_refresh_series_local_artwork_task(
                &non_local_state,
                &TaskRecord::refresh_series_local_artwork(&non_local.id, 4)
            )
            .await,
            TaskOutcome::Completed
        );

        #[cfg(unix)]
        {
            let linked_temporary = tempfile::tempdir().unwrap();
            let linked_root = linked_temporary.path().join("library");
            let external_series = linked_temporary.path().join("external-series");
            fs::create_dir_all(&linked_root).unwrap();
            fs::create_dir_all(&external_series).unwrap();
            fs::write(external_series.join("Book.cbz"), b"book").unwrap();
            fs::write(external_series.join("cover.jpg"), PNG_1X1).unwrap();
            std::os::unix::fs::symlink(&external_series, linked_root.join("Linked")).unwrap();
            let linked_library = Library::new("Linked".into(), Url::from_directory_path(&linked_root).unwrap());
            let (linked_state, linked_series) = scanned_state(&linked_root, linked_library).await;
            assert_eq!(
                process_refresh_series_local_artwork_task(
                    &linked_state,
                    &TaskRecord::refresh_series_local_artwork(&linked_series.id, 4)
                )
                .await,
                TaskOutcome::Completed
            );
            assert!(
                linked_state
                    .storage
                    .list_series_thumbnails(&linked_series.id)
                    .await
                    .unwrap()
                    .is_empty()
            );

            let file_temporary = tempfile::tempdir().unwrap();
            let file_root = file_temporary.path().join("library");
            let file_series_path = file_root.join("Series");
            fs::create_dir_all(&file_series_path).unwrap();
            fs::write(file_series_path.join("Book.cbz"), b"book").unwrap();
            let external_artwork = file_temporary.path().join("external.jpg");
            fs::write(&external_artwork, PNG_1X1).unwrap();
            std::os::unix::fs::symlink(&external_artwork, file_series_path.join("cover.jpg")).unwrap();
            fs::write(file_series_path.join("poster.jpg"), PNG_1X1).unwrap();
            let file_library = Library::new("File link".into(), Url::from_directory_path(&file_root).unwrap());
            let (file_state, file_series) = scanned_state(&file_root, file_library).await;
            assert_eq!(
                process_refresh_series_local_artwork_task(
                    &file_state,
                    &TaskRecord::refresh_series_local_artwork(&file_series.id, 4)
                )
                .await,
                TaskOutcome::Completed
            );
            let thumbnails = file_state
                .storage
                .list_series_thumbnails(&file_series.id)
                .await
                .unwrap();
            assert_eq!(thumbnails.len(), 1);
            assert!(thumbnails[0].url.as_deref().unwrap().ends_with("poster.jpg"));
        }
    }
}
