use std::{fs, path::PathBuf};

use komga_core::{ScanInterval, scan_library_files};
use komga_storage::{Storage, TaskStorage};
use url::Url;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/kotlin-4ef86df")
        .join(name)
}

#[tokio::test]
async fn opens_kotlin_flyway_databases_and_maps_current_rows() {
    let temporary = tempfile::tempdir().unwrap();
    let main_database = temporary.path().join("database.sqlite");
    let tasks_database = temporary.path().join("tasks.sqlite");
    std::fs::copy(fixture("database.sqlite"), &main_database).unwrap();
    std::fs::copy(fixture("tasks.sqlite"), &tasks_database).unwrap();

    let storage = Storage::connect_path(&main_database).await.unwrap();
    let mut library = storage.find_library_by_id("kotlin-library").await.unwrap().unwrap();
    assert_eq!(library.name, "Kotlin fixture");
    assert_eq!(library.scan_interval, ScanInterval::Daily);
    assert_eq!(library.oneshots_directory.as_deref(), Some("One Shots"));
    assert!(library.scan_directory_exclusions.contains("@eaDir"));
    assert!(!library.hash_koreader);
    assert!(library.unavailable_date.is_none());

    let settings = storage.list_global_client_settings(false).await.unwrap();
    let fixture_setting = &settings["komga.fixture"];
    assert_eq!(fixture_setting.value, "kotlin");
    assert_eq!(fixture_setting.allow_unauthorized, Some(true));
    assert_eq!(storage.remember_me_duration_days().await.unwrap(), 365);

    let content_root = temporary.path().join("library-root");
    fs::create_dir_all(content_root.join("Kotlin Series")).unwrap();
    fs::write(content_root.join("Kotlin Series/Volume 1.cbz"), [1_u8]).unwrap();
    library.root = Url::from_directory_path(&content_root).unwrap();
    storage.update_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    let stats = storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    assert_eq!(stats.series_added, 1);
    assert_eq!(stats.books_added, 1);
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    assert_eq!(series[0].name, "Kotlin Series");
    assert_eq!(
        storage.list_books_by_series_id(&series[0].id).await.unwrap()[0].name,
        "Volume 1"
    );

    let tasks = TaskStorage::connect_path(&tasks_database).await.unwrap();
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, "SCAN_LIBRARY_kotlin-library_DEEP_true");
    assert_eq!(queued[0].class, "org.gotson.komga.application.tasks.Task$ScanLibrary");
    assert_eq!(queued[0].simple_type, "ScanLibrary");
    let payload: serde_json::Value = serde_json::from_str(&queued[0].payload).unwrap();
    assert_eq!(payload["libraryId"], "kotlin-library");
    assert_eq!(payload["scanDeep"], true);
    assert_eq!(payload["priority"], 8);
}
