use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use komga_core::{
    AlternateTitle, AuthenticationActivity, Author, BookPage, BookSearch, CollectionCreation, ContentRestrictions,
    Dimension, EPUB_MEDIA_TYPE, HistoricalEvent, Media, MediaExtension, MediaExtensionEpub, MediaFile, MediaStatus,
    PageHashAction, PageRequest, ReadListCreation, ReadProgress, ScanInterval, SeriesSearch, SeriesStatus,
    ThumbnailBook, ThumbnailSeries, User, UserRole, WebLink, scan_library_files,
};
use komga_storage::{ContainerThumbnail, DeleteLibraryResult, Storage, StorageError, TaskRecord, TaskStorage};
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};
use url::Url;

const KOTLIN_MAIN_TABLES: [&str; 51] = [
    "ANNOUNCEMENTS_READ",
    "AUTHENTICATION_ACTIVITY",
    "BOOK",
    "BOOK_METADATA",
    "BOOK_METADATA_AGGREGATION",
    "BOOK_METADATA_AGGREGATION_AUTHOR",
    "BOOK_METADATA_AGGREGATION_TAG",
    "BOOK_METADATA_AUTHOR",
    "BOOK_METADATA_LINK",
    "BOOK_METADATA_TAG",
    "CLIENT_SETTINGS_GLOBAL",
    "CLIENT_SETTINGS_USER",
    "COLLECTION",
    "COLLECTION_SERIES",
    "HISTORICAL_EVENT",
    "HISTORICAL_EVENT_PROPERTIES",
    "LIBRARY",
    "LIBRARY_EXCLUSIONS",
    "MEDIA",
    "MEDIA_FILE",
    "MEDIA_PAGE",
    "PAGE_HASH",
    "PAGE_HASH_THUMBNAIL",
    "READLIST",
    "READLIST_BOOK",
    "READ_PROGRESS",
    "READ_PROGRESS_SERIES",
    "SERIES",
    "SERIES_METADATA",
    "SERIES_METADATA_ALTERNATE_TITLE",
    "SERIES_METADATA_GENRE",
    "SERIES_METADATA_LINK",
    "SERIES_METADATA_SHARING",
    "SERIES_METADATA_TAG",
    "SERVER_SETTINGS",
    "SIDECAR",
    "SYNC_POINT",
    "SYNC_POINT_BOOK",
    "SYNC_POINT_BOOK_REMOVED_SYNCED",
    "SYNC_POINT_READLIST",
    "SYNC_POINT_READLIST_BOOK",
    "SYNC_POINT_READLIST_REMOVED_SYNCED",
    "THUMBNAIL_BOOK",
    "THUMBNAIL_COLLECTION",
    "THUMBNAIL_READLIST",
    "THUMBNAIL_SERIES",
    "USER",
    "USER_API_KEY",
    "USER_LIBRARY_SHARING",
    "USER_ROLE",
    "USER_SHARING",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnContract {
    affinity: &'static str,
    primary_key_position: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexContract {
    unique: bool,
    partial: bool,
    columns: Vec<(Option<String>, String, bool)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ForeignKeyContract {
    table: String,
    from: String,
    target_table: String,
    target_column: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaContract {
    tables: BTreeMap<String, BTreeMap<String, ColumnContract>>,
    indexes: BTreeMap<String, IndexContract>,
    foreign_keys: BTreeSet<ForeignKeyContract>,
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/kotlin-4ef86df")
        .join(name)
}

fn quoted_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sqlite_affinity(declared_type: &str) -> &'static str {
    let declared_type = declared_type.to_ascii_uppercase();
    if declared_type.contains("INT") {
        "INTEGER"
    } else if declared_type.contains("CHAR") || declared_type.contains("CLOB") || declared_type.contains("TEXT") {
        "TEXT"
    } else if declared_type.contains("BLOB") || declared_type.is_empty() {
        "BLOB"
    } else if declared_type.contains("REAL") || declared_type.contains("FLOA") || declared_type.contains("DOUB") {
        "REAL"
    } else {
        "NUMERIC"
    }
}

async fn schema_contract(path: &Path) -> SchemaContract {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap();
    let table_names = sqlx::query_scalar::<_, String>(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .filter(|name| name != "flyway_schema_history" && name != "_sqlx_migrations")
    .collect::<Vec<_>>();

    let mut tables = BTreeMap::new();
    let mut indexes = BTreeMap::new();
    let mut foreign_keys = BTreeSet::new();
    for table_name in table_names {
        let table_key = table_name.to_ascii_uppercase();
        let table = quoted_identifier(&table_name);
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                let name = row.get::<String, _>("name").to_ascii_uppercase();
                let contract = ColumnContract {
                    affinity: sqlite_affinity(&row.get::<String, _>("type")),
                    primary_key_position: row.get("pk"),
                };
                (name, contract)
            })
            .collect();
        tables.insert(table_key.clone(), columns);

        for row in sqlx::query(&format!("PRAGMA index_list({table})"))
            .fetch_all(&pool)
            .await
            .unwrap()
        {
            let name = row.get::<String, _>("name");
            if name.starts_with("sqlite_") {
                continue;
            }
            let index = quoted_identifier(&name);
            let columns = sqlx::query(&format!("PRAGMA index_xinfo({index})"))
                .fetch_all(&pool)
                .await
                .unwrap()
                .into_iter()
                .filter(|column| column.get::<i64, _>("key") == 1)
                .map(|column| {
                    (
                        column
                            .try_get::<String, _>("name")
                            .ok()
                            .map(|name| name.to_ascii_uppercase()),
                        column.get::<String, _>("coll").to_ascii_uppercase(),
                        column.get::<bool, _>("desc"),
                    )
                })
                .collect();
            indexes.insert(
                name.to_ascii_uppercase(),
                IndexContract {
                    unique: row.get("unique"),
                    partial: row.get("partial"),
                    columns,
                },
            );
        }

        for row in sqlx::query(&format!("PRAGMA foreign_key_list({table})"))
            .fetch_all(&pool)
            .await
            .unwrap()
        {
            foreign_keys.insert(ForeignKeyContract {
                table: table_key.clone(),
                from: row.get::<String, _>("from").to_ascii_uppercase(),
                target_table: row.get::<String, _>("table").to_ascii_uppercase(),
                target_column: row.get::<String, _>("to").to_ascii_uppercase(),
            });
        }
    }
    pool.close().await;
    SchemaContract {
        tables,
        indexes,
        foreign_keys,
    }
}

fn assert_supported_schema(baseline: &SchemaContract, candidate: &SchemaContract) {
    for (table, baseline_columns) in &baseline.tables {
        let candidate_columns = candidate
            .tables
            .get(table)
            .unwrap_or_else(|| panic!("missing table {table}"));
        for (column, expected) in baseline_columns {
            let actual = candidate_columns
                .get(column)
                .unwrap_or_else(|| panic!("missing column {table}.{column}"));
            assert_eq!(
                actual.affinity, expected.affinity,
                "incompatible affinity for {table}.{column}"
            );
            assert_eq!(
                actual.primary_key_position, expected.primary_key_position,
                "incompatible primary key for {table}.{column}"
            );
        }
    }
    for (name, expected) in &baseline.indexes {
        assert_eq!(
            candidate.indexes.get(name),
            Some(expected),
            "incompatible named index {name}"
        );
    }
    assert!(
        baseline.foreign_keys.is_subset(&candidate.foreign_keys),
        "candidate foreign keys do not preserve the Kotlin baseline"
    );
}

async fn scalar_i64(path: &Path, query: &str) -> i64 {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap();
    let value = sqlx::query_scalar(query).fetch_one(&pool).await.unwrap();
    pool.close().await;
    value
}

async fn execute_sql(path: &Path, query: &str) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path))
        .await
        .unwrap();
    sqlx::query(query).execute(&pool).await.unwrap();
    pool.close().await;
}

fn directory_entries(path: &Path) -> Vec<String> {
    let mut entries = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

async fn assert_main_database_rejected_without_modification(path: &Path) {
    let bytes = fs::read(path).unwrap();
    let entries = directory_entries(path);
    let error = match Storage::connect_path(path).await {
        Ok(_) => panic!("unsupported main database was accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, StorageError::IncompatibleDatabase { .. }));
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(directory_entries(path), entries);
}

async fn assert_task_database_rejected_without_modification(path: &Path) {
    let bytes = fs::read(path).unwrap();
    let entries = directory_entries(path);
    let error = match TaskStorage::connect_path(path).await {
        Ok(_) => panic!("unsupported task database was accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, StorageError::IncompatibleDatabase { .. }));
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert_eq!(directory_entries(path), entries);
}

#[tokio::test]
async fn fresh_and_kotlin_databases_share_the_supported_schema_contract() {
    let baseline = schema_contract(&fixture("database.sqlite")).await;
    assert_eq!(
        baseline.tables.keys().map(String::as_str).collect::<Vec<_>>(),
        KOTLIN_MAIN_TABLES
    );
    let task_baseline = schema_contract(&fixture("tasks.sqlite")).await;
    assert_eq!(
        task_baseline.tables.keys().map(String::as_str).collect::<Vec<_>>(),
        ["TASK"]
    );

    let temporary = tempfile::tempdir().unwrap();
    let fresh_main = temporary.path().join("fresh-main.sqlite");
    let fresh_tasks = temporary.path().join("fresh-tasks.sqlite");
    let kotlin_main = temporary.path().join("kotlin-main.sqlite");
    let kotlin_tasks = temporary.path().join("kotlin-tasks.sqlite");
    fs::copy(fixture("database.sqlite"), &kotlin_main).unwrap();
    fs::copy(fixture("tasks.sqlite"), &kotlin_tasks).unwrap();

    let fresh_storage = Storage::connect_path(&fresh_main).await.unwrap();
    let fresh_task_storage = TaskStorage::connect_path(&fresh_tasks).await.unwrap();
    let kotlin_storage = Storage::connect_path(&kotlin_main).await.unwrap();
    let kotlin_task_storage = TaskStorage::connect_path(&kotlin_tasks).await.unwrap();
    fresh_storage.health_check().await.unwrap();
    kotlin_storage.health_check().await.unwrap();

    let fresh_contract = schema_contract(&fresh_main).await;
    let fresh_task_contract = schema_contract(&fresh_tasks).await;
    let kotlin_contract = schema_contract(&kotlin_main).await;
    let kotlin_task_contract = schema_contract(&kotlin_tasks).await;
    assert_supported_schema(&baseline, &fresh_contract);
    assert_supported_schema(&baseline, &kotlin_contract);
    assert_supported_schema(&task_baseline, &fresh_task_contract);
    assert_supported_schema(&task_baseline, &kotlin_task_contract);

    drop((fresh_storage, fresh_task_storage, kotlin_storage, kotlin_task_storage));
    let fresh_before_reopen = schema_contract(&fresh_main).await;
    let tasks_before_reopen = schema_contract(&fresh_tasks).await;
    Storage::connect_path(&fresh_main)
        .await
        .unwrap()
        .health_check()
        .await
        .unwrap();
    TaskStorage::connect_path(&fresh_tasks)
        .await
        .unwrap()
        .list()
        .await
        .unwrap();
    assert_eq!(schema_contract(&fresh_main).await, fresh_before_reopen);
    assert_eq!(schema_contract(&fresh_tasks).await, tasks_before_reopen);

    assert_eq!(
        scalar_i64(
            &kotlin_main,
            "SELECT COUNT(*) FROM flyway_schema_history WHERE success=1"
        )
        .await,
        90
    );
    assert_eq!(
        scalar_i64(
            &kotlin_tasks,
            "SELECT COUNT(*) FROM flyway_schema_history WHERE success=1"
        )
        .await,
        1
    );
    assert_eq!(
        scalar_i64(&fresh_main, "SELECT COUNT(*) FROM _sqlx_migrations WHERE success=1").await,
        16
    );
    assert_eq!(
        scalar_i64(&kotlin_main, "SELECT COUNT(*) FROM _sqlx_migrations WHERE success=1").await,
        16
    );
    assert_eq!(
        scalar_i64(&fresh_tasks, "SELECT COUNT(*) FROM _sqlx_migrations WHERE success=1").await,
        1
    );
    assert_eq!(
        scalar_i64(&kotlin_tasks, "SELECT COUNT(*) FROM _sqlx_migrations WHERE success=1").await,
        1
    );
}

#[tokio::test]
async fn unsupported_database_baselines_are_rejected_without_modification() {
    let temporary = tempfile::tempdir().unwrap();

    let old_main_directory = temporary.path().join("old-main");
    fs::create_dir(&old_main_directory).unwrap();
    let old_main = old_main_directory.join("database.sqlite");
    fs::copy(fixture("database.sqlite"), &old_main).unwrap();
    execute_sql(&old_main, "DELETE FROM flyway_schema_history WHERE installed_rank=90").await;
    assert_main_database_rejected_without_modification(&old_main).await;

    let future_main_directory = temporary.path().join("future-main");
    fs::create_dir(&future_main_directory).unwrap();
    let future_main = future_main_directory.join("database.sqlite");
    fs::copy(fixture("database.sqlite"), &future_main).unwrap();
    execute_sql(
        &future_main,
        r#"INSERT INTO flyway_schema_history
           (installed_rank,version,description,type,script,checksum,installed_by,execution_time,success)
           VALUES (91,'20990101000000','future','SQL','V20990101000000__future.sql',1,'test',1,1)"#,
    )
    .await;
    assert_main_database_rejected_without_modification(&future_main).await;

    let modified_main_directory = temporary.path().join("modified-main");
    fs::create_dir(&modified_main_directory).unwrap();
    let modified_main = modified_main_directory.join("database.sqlite");
    fs::copy(fixture("database.sqlite"), &modified_main).unwrap();
    execute_sql(
        &modified_main,
        "UPDATE flyway_schema_history SET checksum=0 WHERE installed_rank=90",
    )
    .await;
    assert_main_database_rejected_without_modification(&modified_main).await;

    let failed_main_directory = temporary.path().join("failed-main");
    fs::create_dir(&failed_main_directory).unwrap();
    let failed_main = failed_main_directory.join("database.sqlite");
    fs::copy(fixture("database.sqlite"), &failed_main).unwrap();
    execute_sql(
        &failed_main,
        "UPDATE flyway_schema_history SET success=0 WHERE installed_rank=90",
    )
    .await;
    assert_main_database_rejected_without_modification(&failed_main).await;

    let wal_main_directory = temporary.path().join("wal-main");
    fs::create_dir(&wal_main_directory).unwrap();
    let wal_main = wal_main_directory.join("database.sqlite");
    let wal_sidecar = wal_main.with_extension("sqlite-wal");
    fs::copy(fixture("database.sqlite"), &wal_main).unwrap();
    let wal_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&wal_main)
                .journal_mode(SqliteJournalMode::Wal),
        )
        .await
        .unwrap();
    sqlx::query("UPDATE flyway_schema_history SET checksum=0 WHERE installed_rank=90")
        .execute(&wal_pool)
        .await
        .unwrap();
    let database_bytes = fs::read(&wal_main).unwrap();
    let wal_bytes = fs::read(&wal_sidecar).unwrap();
    let entries = directory_entries(&wal_main);
    let error = match Storage::connect_path(&wal_main).await {
        Ok(_) => panic!("unsupported WAL database was accepted"),
        Err(error) => error,
    };
    assert!(matches!(error, StorageError::IncompatibleDatabase { .. }));
    assert_eq!(fs::read(&wal_main).unwrap(), database_bytes);
    assert_eq!(fs::read(&wal_sidecar).unwrap(), wal_bytes);
    assert_eq!(directory_entries(&wal_main), entries);
    wal_pool.close().await;

    let modified_sqlx_directory = temporary.path().join("modified-sqlx");
    fs::create_dir(&modified_sqlx_directory).unwrap();
    let modified_sqlx = modified_sqlx_directory.join("database.sqlite");
    fs::copy(fixture("database.sqlite"), &modified_sqlx).unwrap();
    drop(Storage::connect_path(&modified_sqlx).await.unwrap());
    execute_sql(
        &modified_sqlx,
        "UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=16",
    )
    .await;
    assert_main_database_rejected_without_modification(&modified_sqlx).await;

    let wrong_kind_directory = temporary.path().join("wrong-kind");
    fs::create_dir(&wrong_kind_directory).unwrap();
    let wrong_kind = wrong_kind_directory.join("tasks.sqlite");
    fs::copy(fixture("database.sqlite"), &wrong_kind).unwrap();
    assert_task_database_rejected_without_modification(&wrong_kind).await;

    let old_tasks_directory = temporary.path().join("old-tasks");
    fs::create_dir(&old_tasks_directory).unwrap();
    let old_tasks = old_tasks_directory.join("tasks.sqlite");
    fs::copy(fixture("tasks.sqlite"), &old_tasks).unwrap();
    execute_sql(&old_tasks, "DELETE FROM flyway_schema_history").await;
    assert_task_database_rejected_without_modification(&old_tasks).await;
}

#[tokio::test]
async fn kotlin_database_round_trips_every_supported_storage_domain() {
    let temporary = tempfile::tempdir().unwrap();
    let main_database = temporary.path().join("database.sqlite");
    let tasks_database = temporary.path().join("tasks.sqlite");
    fs::copy(fixture("database.sqlite"), &main_database).unwrap();
    fs::copy(fixture("tasks.sqlite"), &tasks_database).unwrap();
    let storage = Storage::connect_path(&main_database).await.unwrap();

    let content_root = temporary.path().join("library-root");
    fs::create_dir_all(content_root.join("Series")).unwrap();
    fs::write(content_root.join("Series/Volume 1.epub"), b"fixture").unwrap();
    let mut library = storage.find_library_by_id("kotlin-library").await.unwrap().unwrap();
    library.root = Url::from_directory_path(&content_root).unwrap();
    storage.update_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);

    let mut policy_user = User::new("policy@example.org".into(), "$2b$10$fixture".into());
    policy_user.id = "policy-user".into();
    policy_user.roles = BTreeSet::from([UserRole::FileDownload, UserRole::PageStreaming]);
    policy_user.shared_all_libraries = false;
    policy_user.shared_library_ids = BTreeSet::from([library.id.clone()]);
    policy_user.restrictions = ContentRestrictions::new(None, ["family".into(), "blocked".into()], ["blocked".into()]);
    storage.create_user(&policy_user).await.unwrap();
    assert_eq!(
        storage.find_user_by_id(&policy_user.id).await.unwrap(),
        Some(policy_user.clone())
    );

    let reader = User::new("reader@example.org".into(), "$2b$10$fixture".into());
    storage.create_user(&reader).await.unwrap();
    let api_key = storage
        .create_api_key("fixture-key", &policy_user.id, "sha512-fixture", "Kobo")
        .await
        .unwrap();
    assert_eq!(
        storage.find_api_key_by_hash("sha512-fixture").await.unwrap(),
        Some(api_key)
    );

    storage
        .save_global_client_settings(&[("komga.fixture.roundtrip".into(), "global".into(), true)])
        .await
        .unwrap();
    storage
        .save_user_client_settings(&policy_user.id, &[("komga.fixture.user".into(), "private".into())])
        .await
        .unwrap();
    assert_eq!(
        storage.list_global_client_settings(false).await.unwrap()["komga.fixture.roundtrip"].value,
        "global"
    );
    assert_eq!(
        storage.list_user_client_settings(&policy_user.id).await.unwrap()["komga.fixture.user"].value,
        "private"
    );

    storage
        .insert_authentication_activity(&AuthenticationActivity {
            user_id: Some(policy_user.id.clone()),
            email: Some(policy_user.email.clone()),
            api_key_id: Some("fixture-key".into()),
            api_key_comment: Some("Kobo".into()),
            ip: Some("192.0.2.2".into()),
            user_agent: Some("fixture".into()),
            success: true,
            error: None,
            date_time: Utc::now().naive_utc(),
            source: Some("API_KEY".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        storage
            .latest_authentication_activity(&policy_user, Some("fixture-key"))
            .await
            .unwrap()
            .unwrap()
            .api_key_comment
            .as_deref(),
        Some("Kobo")
    );

    let mut series_metadata = storage.find_series_metadata(&series.id).await.unwrap().unwrap();
    series_metadata.genres = BTreeSet::from(["science fiction".into()]);
    series_metadata.tags = BTreeSet::from(["fixture".into()]);
    series_metadata.sharing_labels = BTreeSet::from(["family".into()]);
    series_metadata.links = vec![WebLink {
        label: "Source".into(),
        url: "https://example.org/series".into(),
    }];
    series_metadata.alternate_titles = vec![AlternateTitle {
        label: "en".into(),
        title: "Fixture Series".into(),
    }];
    storage.save_series_metadata(&series_metadata).await.unwrap();
    let saved_series_metadata = storage.find_series_metadata(&series.id).await.unwrap().unwrap();
    assert_eq!(saved_series_metadata.genres, series_metadata.genres);
    assert_eq!(saved_series_metadata.tags, series_metadata.tags);
    assert_eq!(saved_series_metadata.sharing_labels, series_metadata.sharing_labels);
    assert_eq!(saved_series_metadata.links, series_metadata.links);
    assert_eq!(saved_series_metadata.alternate_titles, series_metadata.alternate_titles);

    let mut book_metadata = storage.find_book_metadata(&book.id).await.unwrap().unwrap();
    book_metadata.authors = vec![Author {
        name: "Fixture Author".into(),
        role: "writer".into(),
    }];
    book_metadata.tags = BTreeSet::from(["fixture".into()]);
    book_metadata.links = vec![WebLink {
        label: "Source".into(),
        url: "https://example.org/book".into(),
    }];
    storage.save_book_metadata(&book_metadata).await.unwrap();
    let saved_book_metadata = storage.find_book_metadata(&book.id).await.unwrap().unwrap();
    assert_eq!(saved_book_metadata.authors, book_metadata.authors);
    assert_eq!(saved_book_metadata.tags, book_metadata.tags);
    assert_eq!(saved_book_metadata.links, book_metadata.links);
    assert!(storage.rebuild_book_metadata_aggregation(&series.id).await.unwrap());

    storage
        .create_collection(
            "fixture-collection",
            &CollectionCreation {
                name: "Fixture Collection".into(),
                ordered: true,
                series_ids: vec![series.id.clone()],
            },
        )
        .await
        .unwrap();
    storage
        .create_read_list(
            "fixture-read-list",
            &ReadListCreation {
                name: "Fixture Read List".into(),
                summary: "Fixture summary".into(),
                ordered: true,
                book_ids: vec![book.id.clone()],
            },
        )
        .await
        .unwrap();
    assert!(
        storage
            .find_collection("fixture-collection", &reader)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        storage
            .find_read_list("fixture-read-list", &reader)
            .await
            .unwrap()
            .is_some()
    );

    for (read_list, parent_id, thumbnail_id) in [
        (false, "fixture-collection", "fixture-collection-thumbnail"),
        (true, "fixture-read-list", "fixture-read-list-thumbnail"),
    ] {
        let thumbnail = ContainerThumbnail {
            id: thumbnail_id.into(),
            thumbnail: Some(vec![1, 2, 3]),
            url: None,
            selected: false,
            thumbnail_type: "USER_UPLOADED".into(),
            media_type: "image/jpeg".into(),
            file_size: 3,
            width: 1,
            height: 1,
            parent_id: parent_id.into(),
        };
        storage
            .insert_container_thumbnail(read_list, &thumbnail, true)
            .await
            .unwrap();
        assert_eq!(
            storage
                .selected_container_thumbnail(read_list, parent_id)
                .await
                .unwrap()
                .unwrap()
                .id,
            thumbnail_id
        );
    }

    let mut media = Media::result(&book.id, MediaStatus::Ready, Some(EPUB_MEDIA_TYPE.into()), None);
    media.page_count = 1;
    media.pages = vec![BookPage {
        file_name: "page.xhtml".into(),
        media_type: "application/xhtml+xml".into(),
        dimension: Some(Dimension { width: 1, height: 1 }),
        file_hash: "fixture-page-hash".into(),
        file_size: Some(42),
    }];
    media.files = vec![MediaFile {
        file_name: "cover.jpg".into(),
        media_type: Some("image/jpeg".into()),
        sub_type: Some("EPUB_ASSET".into()),
        file_size: Some(3),
    }];
    media.extension = Some(MediaExtension::Epub(MediaExtensionEpub::default()));
    storage.replace_media_analysis(&media).await.unwrap();
    let loaded_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
    assert_eq!(loaded_media.status, MediaStatus::Ready);
    assert_eq!(loaded_media.pages, media.pages);
    assert_eq!(loaded_media.files, media.files);
    assert_eq!(loaded_media.extension, media.extension);

    storage
        .insert_book_thumbnail(
            &ThumbnailBook {
                id: "fixture-book-thumbnail".into(),
                thumbnail: Some(vec![1, 2, 3]),
                url: None,
                selected: false,
                thumbnail_type: "USER_UPLOADED".into(),
                media_type: "image/jpeg".into(),
                file_size: 3,
                dimension: Dimension { width: 1, height: 1 },
                book_id: book.id.clone(),
            },
            true,
        )
        .await
        .unwrap();
    storage
        .insert_series_thumbnail(
            &ThumbnailSeries {
                id: "fixture-series-thumbnail".into(),
                thumbnail: Some(vec![1, 2, 3]),
                url: None,
                selected: false,
                thumbnail_type: "USER_UPLOADED".into(),
                media_type: "image/jpeg".into(),
                file_size: 3,
                dimension: Dimension { width: 1, height: 1 },
                series_id: series.id.clone(),
            },
            true,
        )
        .await
        .unwrap();
    assert_eq!(storage.list_book_thumbnails(&book.id).await.unwrap().len(), 1);
    assert_eq!(storage.list_series_thumbnails(&series.id).await.unwrap().len(), 1);

    storage
        .create_or_update_page_hash(
            "fixture-page-hash",
            Some(42),
            PageHashAction::Ignore,
            Some(b"thumbnail"),
        )
        .await
        .unwrap();
    assert_eq!(
        storage
            .find_known_page_hash("fixture-page-hash")
            .await
            .unwrap()
            .unwrap()
            .match_count,
        1
    );
    assert_eq!(
        storage
            .page_hash_thumbnail("fixture-page-hash")
            .await
            .unwrap()
            .as_deref(),
        Some(&b"thumbnail"[..])
    );

    let read_date = Utc::now().naive_utc();
    storage
        .save_read_progress(&ReadProgress {
            book_id: book.id.clone(),
            user_id: reader.id.clone(),
            page: 1,
            completed: true,
            read_date,
            device_id: "fixture-device".into(),
            device_name: "Fixture Reader".into(),
            locator: None,
        })
        .await
        .unwrap();
    assert_eq!(
        storage
            .find_read_progress(&book.id, &reader.id)
            .await
            .unwrap()
            .unwrap()
            .read_date,
        read_date
    );

    let sync_point = storage.create_sync_point(&reader, Some("fixture-key")).await.unwrap();
    assert_eq!(
        storage
            .find_sync_point_for_user(&sync_point.id, &reader.id)
            .await
            .unwrap()
            .unwrap()
            .api_key_id
            .as_deref(),
        Some("fixture-key")
    );

    storage
        .mark_announcements_read(&reader.id, &BTreeSet::from(["fixture-announcement".into()]))
        .await
        .unwrap();
    assert_eq!(
        storage.announcement_ids_read(&reader.id).await.unwrap(),
        BTreeSet::from(["fixture-announcement".into()])
    );
    let event = HistoricalEvent::new(
        "BookFileDeleted",
        Some(book.id.clone()),
        Some(series.id.clone()),
        BTreeMap::from([("reason".into(), "fixture".into())]),
    );
    storage.insert_historical_event(&event).await.unwrap();
    assert_eq!(
        storage
            .list_historical_events(&PageRequest::default())
            .await
            .unwrap()
            .content
            .into_iter()
            .find(|candidate| candidate.id == event.id)
            .unwrap(),
        event
    );

    let tasks = TaskStorage::connect_path(&tasks_database).await.unwrap();
    let task = TaskRecord::aggregate_series_metadata(&series.id, 9);
    tasks.save(&task).await.unwrap();
    let claimed = tasks
        .take_first_of_type("fixture-owner", "AggregateSeriesMetadata")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, task.id);
    assert!(tasks.release(&task.id, "fixture-owner").await.unwrap());
    assert!(tasks.delete(&task.id).await.unwrap());
    assert!(
        tasks
            .list()
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.id == "SCAN_LIBRARY_kotlin-library_DEEP_true")
    );
}

#[tokio::test]
async fn page_hash_migrations_reset_versioned_hashes_and_rules() {
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
    sqlx::query(
        "INSERT INTO MEDIA (BOOK_ID,STATUS,MEDIA_TYPE,PAGE_COUNT) VALUES ('pdf', 'READY', 'application/pdf', 1), ('zip', 'READY', 'application/zip', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO MEDIA_PAGE (BOOK_ID,NUMBER,FILE_NAME,MEDIA_TYPE,FILE_HASH) VALUES ('pdf',0,'1','','old-pdf'), ('zip',0,'1.png','image/png','raw-zip')",
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
    assert_eq!(
        sqlx::query_as::<_, (String, String)>(
            "SELECT BOOK_ID,FILE_HASH FROM MEDIA_PAGE WHERE BOOK_ID IN ('pdf','zip') ORDER BY BOOK_ID",
        )
        .fetch_all(&pool)
        .await
        .unwrap(),
        [("pdf".into(), String::new()), ("zip".into(), "raw-zip".into())]
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx__media_page__file_hash'",
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
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
