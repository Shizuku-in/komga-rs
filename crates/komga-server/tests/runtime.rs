use std::{fs, time::Duration};

use komga_core::{Library, ScanInterval};
use komga_server::AppState;
use komga_storage::{Storage, TaskRecord, TaskStorage};
use url::Url;

#[tokio::test]
async fn startup_scheduler_drives_scan_worker_and_preserves_unported_tasks() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Volume 1.cbz"), [1_u8, 2, 3]).unwrap();
    let mut library = Library::new("Runtime".into(), Url::from_directory_path(&root).unwrap());
    library.scan_on_startup = true;
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    tasks.save(&TaskRecord::empty_trash(&library.id, 8)).await.unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !storage.list_series_by_library_id(&library.id).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("startup scan did not complete");

    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(storage.list_books_by_series_id(&series[0].id).await.unwrap().len(), 1);
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].simple_type, "EmptyTrash");

    runtime.shutdown().await;
}
