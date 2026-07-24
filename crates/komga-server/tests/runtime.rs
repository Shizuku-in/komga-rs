use std::{fs, path::PathBuf, time::Duration};

use komga_core::{Library, ScanInterval};
use komga_server::AppState;
use komga_storage::{Storage, TaskRecord, TaskStorage};
use url::Url;

#[tokio::test]
async fn startup_scheduler_analyzes_cbz_generates_thumbnail_and_preserves_unported_tasks() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/archives/zip.zip");
    fs::copy(fixture, root.join("Series/Volume 1.cbz")).unwrap();
    let mut library = Library::new("Runtime".into(), Url::from_directory_path(&root).unwrap());
    library.scan_on_startup = true;
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    tasks.save(&TaskRecord::empty_trash(&library.id, 8)).await.unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let series = storage.list_series_by_library_id(&library.id).await.unwrap();
            if let Some(series) = series.first()
                && let Some(book) = storage.list_books_by_series_id(&series.id).await.unwrap().first()
                && storage
                    .find_media_by_book_id(&book.id)
                    .await
                    .unwrap()
                    .is_some_and(|media| media.status == komga_core::MediaStatus::Ready && media.page_count == 1)
                && storage.find_selected_book_thumbnail(&book.id).await.unwrap().is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("startup scan did not complete");

    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    assert_eq!(series.len(), 1);
    let books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
    assert_eq!(books.len(), 1);
    let media = storage.find_media_by_book_id(&books[0].id).await.unwrap().unwrap();
    assert_eq!(media.pages[0].file_name, "komga.png");
    assert_eq!(
        media.pages[0].dimension,
        Some(komga_core::Dimension { width: 48, height: 48 })
    );
    let thumbnail = storage
        .find_selected_book_thumbnail(&books[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thumbnail.thumbnail_type, "GENERATED");
    assert_eq!(thumbnail.media_type, "image/jpeg");
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].simple_type, "EmptyTrash");

    runtime.shutdown().await;
}
