use std::{fs, path::PathBuf};

use chrono::Utc;
use komga_core::{
    AuthenticationActivity, BookSearch, PageRequest, ScanInterval, SeriesSearch, SeriesStatus, User, scan_library_files,
};
use komga_storage::{DeleteLibraryResult, Storage, TaskStorage};
use url::Url;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/kotlin-4ef86df")
        .join(name)
}

#[tokio::test]
async fn page_hash_migration_resets_kotlin_jpeg_hashes_and_rules() {
    let temporary = tempfile::tempdir().unwrap();
    let main_database = temporary.path().join("database.sqlite");
    std::fs::copy(fixture("database.sqlite"), &main_database).unwrap();
    let url = format!("sqlite://{}", main_database.display());
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&main_database)
                .foreign_keys(false),
        )
        .await
        .unwrap();
    sqlx::query("INSERT INTO PAGE_HASH (HASH,SIZE,ACTION) VALUES ('legacy',10,'DELETE')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO PAGE_HASH_THUMBNAIL (HASH,THUMBNAIL) VALUES ('legacy',X'01')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO MEDIA_PAGE (BOOK_ID,NUMBER,FILE_NAME,MEDIA_TYPE,FILE_HASH) VALUES ('orphan',0,'page.jpg','image/jpeg','legacy')",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let _storage = Storage::connect_path(&main_database).await.unwrap();
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM PAGE_HASH")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM PAGE_HASH_THUMBNAIL")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT FILE_HASH,FILE_HASH_ALGORITHM FROM MEDIA_PAGE WHERE BOOK_ID='orphan'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (String::new(), None)
    );
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

    let server_settings = storage.server_settings().await.unwrap();
    assert!(server_settings.delete_empty_collections);
    assert!(server_settings.delete_empty_read_lists);
    assert_eq!(server_settings.remember_me_duration_days, 365);
    assert_eq!(server_settings.thumbnail_size, komga_core::ThumbnailSize::Default);
    assert_eq!(server_settings.task_pool_size, 1);
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
    let user = User::new("fixture-reader@example.org".into(), "$2b$10$fixture".into());
    storage.create_user(&user).await.unwrap();
    storage
        .insert_authentication_activity(&AuthenticationActivity {
            user_id: Some(user.id.clone()),
            email: Some(user.email.clone()),
            api_key_id: None,
            api_key_comment: None,
            ip: Some("192.0.2.1".into()),
            user_agent: Some("fixture".into()),
            success: true,
            error: None,
            date_time: Utc::now().naive_utc(),
            source: Some("Password".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        storage
            .list_authentication_activity(Some(&user), &PageRequest::default())
            .await
            .unwrap()
            .total_elements,
        1
    );
    let series_page = storage
        .query_series(&SeriesSearch::default(), &PageRequest::default(), &user, None)
        .await
        .unwrap();
    assert_eq!(series_page.content[0].metadata.title, "Kotlin Series");
    let book_page = storage
        .query_books(&BookSearch::default(), &PageRequest::default(), &user, None)
        .await
        .unwrap();
    assert_eq!(book_page.content[0].series_title, "Kotlin Series");
    assert_eq!(book_page.content[0].media.status, "UNKNOWN");
    let series_metadata = storage.find_series_metadata(&series[0].id).await.unwrap().unwrap();
    assert_eq!(series_metadata.status, SeriesStatus::Ongoing);
    assert_eq!(series_metadata.title, "Kotlin Series");
    let books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
    let book_metadata = storage.find_book_metadata(&books[0].id).await.unwrap().unwrap();
    assert_eq!(book_metadata.title, "Volume 1");
    assert!(storage.rebuild_book_metadata_aggregation(&series[0].id).await.unwrap());
    assert!(
        storage
            .find_book_metadata_aggregation(&series[0].id)
            .await
            .unwrap()
            .is_some()
    );

    let fixture_pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", main_database.display()))
        .await
        .unwrap();
    sqlx::query("INSERT INTO HISTORICAL_EVENT (ID,TYPE,BOOK_ID,SERIES_ID) VALUES ('rust-delete','BookDeleted',?,?)")
        .bind(&books[0].id)
        .bind(&series[0].id)
        .execute(&fixture_pool)
        .await
        .unwrap();

    let empty_root = temporary.path().join("empty-root");
    fs::create_dir_all(&empty_root).unwrap();
    library.root = Url::from_directory_path(&empty_root).unwrap();
    storage.update_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let emptied = storage.empty_trash(&library.id).await.unwrap().unwrap();
    assert_eq!(emptied.series_deleted, 1);
    assert_eq!(emptied.books_deleted, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM HISTORICAL_EVENT WHERE ID='rust-delete'")
            .fetch_one(&fixture_pool)
            .await
            .unwrap(),
        1
    );
    assert!(matches!(
        storage.delete_library(&library.id).await.unwrap(),
        DeleteLibraryResult::Deleted { .. }
    ));
    assert!(content_root.join("Kotlin Series/Volume 1.cbz").exists());

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
