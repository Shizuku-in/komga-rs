use std::{fs, fs::File, io::Write, path::PathBuf, time::Duration};

use komga_core::{BookMetadataPatch, Library, MetadataUpdate, PageRequest, ScanInterval, User, scan_library_files};
use komga_search::{EntityKind, IndexState, SearchIndex};
use komga_server::AppState;
use komga_storage::{Storage, TaskRecord, TaskStorage};
use url::Url;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[tokio::test]
async fn file_search_startup_enqueues_only_non_current_indexes() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let empty_directory = tempfile::tempdir().unwrap();
    let empty = SearchIndex::open(empty_directory.path()).unwrap();
    assert_eq!(empty.state(), IndexState::Empty);

    AppState::with_file_search(storage.clone(), tasks.clone(), empty.clone())
        .await
        .unwrap();
    AppState::with_file_search(storage.clone(), tasks.clone(), empty)
        .await
        .unwrap();
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, "REBUILD_INDEX");
    assert_eq!(queued[0].priority, 8);

    let current_tasks = TaskStorage::connect_memory().await.unwrap();
    let current_directory = tempfile::tempdir().unwrap();
    let current = SearchIndex::open(current_directory.path()).unwrap();
    current
        .replace(
            &[
                EntityKind::Book,
                EntityKind::Series,
                EntityKind::Collection,
                EntityKind::ReadList,
            ],
            [],
        )
        .unwrap();
    assert_eq!(current.state(), IndexState::Current);

    AppState::with_file_search(storage, current_tasks.clone(), current)
        .await
        .unwrap();
    assert!(current_tasks.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn empty_trash_tasks_permanently_delete_soft_deleted_content_and_missing_libraries_complete() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Book 1.cbz"), [1]).unwrap();
    fs::write(root.join("Series/Book 2.cbz"), [2]).unwrap();
    let library = Library::new("Trash".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    let removed = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
    fs::remove_file(root.join("Series/Book 1.cbz")).unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    tasks.save(&TaskRecord::empty_trash(&library.id, 6)).await.unwrap();
    tasks
        .save(&TaskRecord::empty_trash("missing-library", 6))
        .await
        .unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tasks.list().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("empty-trash tasks did not complete");
    assert!(storage.find_book_by_id(&removed.id).await.unwrap().is_none());
    assert_eq!(storage.list_books_by_series_id(&series.id).await.unwrap()[0].number, 1);
    runtime.shutdown().await;
}

#[tokio::test]
async fn scan_tasks_honor_empty_trash_after_scan() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Keep.cbz"), [1]).unwrap();
    fs::write(root.join("Series/Remove.cbz"), [2]).unwrap();
    let mut library = Library::new("Auto trash".into(), Url::from_directory_path(&root).unwrap());
    library.empty_trash_after_scan = true;
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    let removed = storage
        .list_books_by_series_id(&series.id)
        .await
        .unwrap()
        .into_iter()
        .find(|book| book.name == "Remove")
        .unwrap();
    fs::remove_file(root.join("Series/Remove.cbz")).unwrap();
    tasks
        .save(&TaskRecord::scan_library(&library.id, false, 8))
        .await
        .unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !tasks.list().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scan-triggered empty trash did not complete");
    assert!(storage.find_book_by_id(&removed.id).await.unwrap().is_none());
    assert_eq!(storage.list_books_by_series_id(&series.id).await.unwrap().len(), 1);
    runtime.shutdown().await;
}

#[tokio::test]
async fn scan_tasks_hash_missing_book_files_with_kotlin_xxh3() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Book.cbz"), b"hello").unwrap();
    let mut library = Library::new("Hashes".into(), Url::from_directory_path(&root).unwrap());
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    tasks
        .save(&TaskRecord::scan_library(&library.id, false, 8))
        .await
        .unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let books = storage.list_active_books_by_library_id(&library.id).await.unwrap();
            if books
                .first()
                .is_some_and(|book| book.file_hash == "b5e9c1ad071b3e7fc779cfaa5e523818")
                && tasks.list().await.unwrap().is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scan-triggered file hashing did not complete");
    runtime.shutdown().await;
}

#[tokio::test]
async fn hash_tasks_skip_books_when_library_hashing_is_disabled() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Book.cbz"), b"hello").unwrap();
    let mut library = Library::new("Hashes disabled".into(), Url::from_directory_path(&root).unwrap());
    library.hash_files = false;
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
    tasks.save(&TaskRecord::hash_book(&book.id, 0)).await.unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tasks.list().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("disabled hash task did not complete");

    assert!(
        storage
            .find_book_by_id(&book.id)
            .await
            .unwrap()
            .unwrap()
            .file_hash
            .is_empty()
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn scan_imports_comic_info_collections_and_read_lists_idempotently() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/archives/zip.zip");
    let mut source = ZipArchive::new(File::open(fixture).unwrap()).unwrap();
    let mut image = Vec::new();
    std::io::Read::read_to_end(&mut source.by_name("komga.png").unwrap(), &mut image).unwrap();
    let file = File::create(root.join("Series/Volume.cbz")).unwrap();
    let mut archive = ZipWriter::new(file);
    archive.start_file("page.png", SimpleFileOptions::default()).unwrap();
    archive.write_all(&image).unwrap();
    archive
        .start_file("ComicInfo.xml", SimpleFileOptions::default())
        .unwrap();
    archive
        .write_all(b"<ComicInfo><SeriesGroup>Universe, Event</SeriesGroup><AlternateSeries>Run</AlternateSeries><AlternateNumber>4</AlternateNumber><StoryArc>Arc One, Arc Two</StoryArc><StoryArcNumber>1,2</StoryArcNumber></ComicInfo>")
        .unwrap();
    archive.finish().unwrap();

    let mut library = Library::new("ComicInfo".into(), Url::from_directory_path(&root).unwrap());
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    tasks
        .save(&TaskRecord::scan_library(&library.id, false, 8))
        .await
        .unwrap();
    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !tasks.list().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("ComicInfo import did not complete");

    let user = User::new("reader@example.org".into(), "hash".into());
    let collections = storage
        .list_collections(None, None, None, &PageRequest::default(), &user)
        .await
        .unwrap();
    assert_eq!(
        collections
            .content
            .iter()
            .map(|value| value.name.as_str())
            .collect::<Vec<_>>(),
        ["Event", "Universe"]
    );
    assert!(
        collections
            .content
            .iter()
            .all(|value| !value.ordered && value.series_ids.len() == 1)
    );
    let read_lists = storage
        .list_read_lists(None, None, None, &PageRequest::default(), &user)
        .await
        .unwrap();
    assert_eq!(
        read_lists
            .content
            .iter()
            .map(|value| value.name.as_str())
            .collect::<Vec<_>>(),
        ["Arc One", "Arc Two", "Run"]
    );
    assert!(
        read_lists
            .content
            .iter()
            .all(|value| value.ordered && value.book_ids.len() == 1)
    );

    runtime.shutdown().await;
    tasks
        .save(&TaskRecord::scan_library(&library.id, true, 8))
        .await
        .unwrap();
    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !tasks.list().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("deep ComicInfo backfill did not complete");
    assert_eq!(
        storage
            .list_collections(None, None, None, &PageRequest::default(), &user)
            .await
            .unwrap()
            .total_elements,
        2
    );
    assert_eq!(
        storage
            .list_read_lists(None, None, None, &PageRequest::default(), &user)
            .await
            .unwrap()
            .total_elements,
        3
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn startup_scheduler_analyzes_supported_media_generates_thumbnails_and_executes_empty_trash() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let zip_fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/archives/zip.zip");
    let rar_fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/archives/rar4-solid.rar");
    let pdf_fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/pdf/komga.pdf");
    let epub_fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/archives/epub3.epub");
    fs::copy(zip_fixture, root.join("Series/Volume 1.cbz")).unwrap();
    fs::copy(rar_fixture, root.join("Series/Volume 2.cbr")).unwrap();
    fs::copy(pdf_fixture, root.join("Series/Volume 3.pdf")).unwrap();
    fs::copy(epub_fixture, root.join("Series/Volume 4.epub")).unwrap();
    let mut library = Library::new("Runtime".into(), Url::from_directory_path(&root).unwrap());
    library.scan_on_startup = true;
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    tasks.save(&TaskRecord::empty_trash(&library.id, 8)).await.unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let series = storage.list_series_by_library_id(&library.id).await.unwrap();
            if let Some(series) = series.first() {
                let books = storage.list_books_by_series_id(&series.id).await.unwrap();
                if books.len() == 4 {
                    let mut complete = true;
                    for book in &books {
                        complete &= storage
                            .find_media_by_book_id(&book.id)
                            .await
                            .unwrap()
                            .is_some_and(|media| media.status == komga_core::MediaStatus::Ready);
                        complete &= storage.find_selected_book_thumbnail(&book.id).await.unwrap().is_some();
                    }
                    if complete {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("startup scan did not complete");

    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    assert_eq!(series.len(), 1);
    let books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
    assert_eq!(books.len(), 4);
    let zip_media = storage.find_media_by_book_id(&books[0].id).await.unwrap().unwrap();
    assert_eq!(zip_media.pages[0].file_name, "komga.png");
    assert_eq!(
        zip_media.pages[0].dimension,
        Some(komga_core::Dimension { width: 48, height: 48 })
    );
    let rar_media = storage.find_media_by_book_id(&books[1].id).await.unwrap().unwrap();
    assert_eq!(
        rar_media.media_type.as_deref(),
        Some("application/x-rar-compressed; version=4")
    );
    assert_eq!(rar_media.page_count, 3);
    assert_eq!(rar_media.pages[2].file_name, "komga-3.png");
    let pdf_media = storage.find_media_by_book_id(&books[2].id).await.unwrap().unwrap();
    assert_eq!(pdf_media.media_type.as_deref(), Some("application/pdf"));
    assert_eq!(pdf_media.page_count, 1);
    assert_eq!(pdf_media.pages[0].file_name, "1");
    assert_eq!(
        pdf_media.pages[0].dimension,
        Some(komga_core::Dimension {
            width: 512,
            height: 512
        })
    );
    let epub_media = storage.find_media_by_book_id(&books[3].id).await.unwrap().unwrap();
    assert_eq!(epub_media.media_type.as_deref(), Some("application/epub+zip"));
    assert_eq!(epub_media.page_count, 2);
    assert!(epub_media.epub_divina_compatible);
    assert!(matches!(
        epub_media.extension,
        Some(komga_core::MediaExtension::Epub(_))
    ));
    for book in &books {
        let thumbnail = storage.find_selected_book_thumbnail(&book.id).await.unwrap().unwrap();
        assert_eq!(thumbnail.thumbnail_type, "GENERATED");
        assert_eq!(thumbnail.media_type, "image/jpeg");
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let queued = tasks.list().await.unwrap();
            if queued.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("metadata refresh pipeline did not complete");
    let queued = tasks.list().await.unwrap();
    assert!(queued.is_empty());

    runtime.shutdown().await;
}

#[tokio::test]
async fn aggregate_series_metadata_tasks_rebuild_persisted_projections() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Volume 1.cbz"), [1]).unwrap();
    fs::write(root.join("Series/Volume 2.cbz"), [2]).unwrap();
    let mut library = Library::new("Metadata".into(), Url::from_directory_path(&root).unwrap());
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    let books = storage.list_books_by_series_id(&series.id).await.unwrap();
    let existing = storage.find_book_metadata(&books[0].id).await.unwrap().unwrap();
    let updated = existing
        .apply_patch(BookMetadataPatch {
            summary: MetadataUpdate::Set("Runtime summary".into()),
            tags: MetadataUpdate::Set(["runtime".into()].into()),
            ..Default::default()
        })
        .unwrap();
    storage.save_book_metadata(&updated).await.unwrap();

    let task = TaskRecord::aggregate_series_metadata(&series.id, 5);
    assert_eq!(task.id, format!("AGGREGATE_SERIES_METADATA_{}", series.id));
    assert_eq!(task.group_id.as_deref(), Some(series.id.as_str()));
    assert_eq!(
        task.class,
        "org.gotson.komga.application.tasks.Task$AggregateSeriesMetadata"
    );
    assert_eq!(task.simple_type, "AggregateSeriesMetadata");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&task.payload).unwrap()["seriesId"],
        series.id
    );
    tasks.save(&task).await.unwrap();

    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let aggregation = storage
                .find_book_metadata_aggregation(&series.id)
                .await
                .unwrap()
                .unwrap();
            if aggregation.summary == "Runtime summary" && tasks.list().await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("metadata aggregation task did not complete");
    let aggregation = storage
        .find_book_metadata_aggregation(&series.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(aggregation.summary_number, "1");
    assert_eq!(aggregation.tags, ["runtime".into()].into());

    runtime.shutdown().await;
}
