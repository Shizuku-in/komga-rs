use std::{fs, fs::File, io::Write, path::PathBuf, time::Duration};

use komga_core::{BookMetadataPatch, Library, MetadataUpdate, PageRequest, ScanInterval, User, scan_library_files};
use komga_server::AppState;
use komga_storage::{Storage, TaskRecord, TaskStorage};
use url::Url;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

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
        .list_collections(None, None, &PageRequest::default(), &user)
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
        .list_read_lists(None, &PageRequest::default(), &user)
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
            .list_collections(None, None, &PageRequest::default(), &user)
            .await
            .unwrap()
            .total_elements,
        2
    );
    assert_eq!(
        storage
            .list_read_lists(None, &PageRequest::default(), &user)
            .await
            .unwrap()
            .total_elements,
        3
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn startup_scheduler_analyzes_supported_media_generates_thumbnails_and_preserves_unported_tasks() {
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
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].simple_type, "EmptyTrash");

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
