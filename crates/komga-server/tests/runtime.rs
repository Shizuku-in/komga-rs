use std::{fs, path::PathBuf, time::Duration};

use komga_core::{Library, ScanInterval};
use komga_server::AppState;
use komga_storage::{Storage, TaskRecord, TaskStorage};
use url::Url;

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
