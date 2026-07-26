use std::fs;

use komga_core::{BookThumbnailEvent, DomainEvent, ThumbnailBook, new_id, scan_book_sidecars};
use komga_media::{detect_image_media_type, image_dimension};
use komga_storage::TaskRecord;
use url::Url;

use crate::AppState;

use super::{BookTaskPayload, TaskOutcome};

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

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{Library, scan_library_files};
    use komga_storage::{Storage, TaskStorage};

    use super::*;

    const PNG_1X1: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0, 0, 0, 181, 28,
        12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1, 39, 24, 227, 102, 0, 0, 0, 0,
        73, 69, 78, 68, 174, 66, 96, 130,
    ];

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
}
