use std::fs;

use komga_core::{Library, PageRequest, SortDirection, SortOrder, scan_library_files};
use komga_storage::{Storage, StorageError};
use url::Url;

async fn duplicate_fixture() -> (tempfile::TempDir, Storage, Library) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let series = root.join("Series");
    fs::create_dir_all(&series).unwrap();
    fs::write(series.join("A.cbz"), [1_u8]).unwrap();
    fs::write(series.join("B.cbz"), [2_u8]).unwrap();
    fs::write(series.join("C.cbz"), [3_u8, 3]).unwrap();
    fs::write(series.join("D.cbz"), [4_u8, 4, 4]).unwrap();
    fs::write(series.join("E.cbz"), [5_u8, 5, 5]).unwrap();
    fs::write(series.join("F.cbz"), [6_u8, 6, 6, 6]).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    let storage = Storage::connect_memory().await.unwrap();
    storage.create_library(&library).await.unwrap();
    let mut scan = scan_library_files(&library, || false).unwrap();
    for book in &mut scan.series[0].books {
        book.file_hash = match book.name.as_str() {
            "A" | "B" | "C" => "shared".into(),
            "D" | "E" => "other".into(),
            _ => String::new(),
        };
    }
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    (temporary, storage, library)
}

#[tokio::test]
async fn duplicate_query_groups_hash_and_size_pages_and_includes_deleted_books() {
    let (temporary, storage, library) = duplicate_fixture().await;
    let sorted = PageRequest {
        size: 2,
        sort: vec![SortOrder {
            property: "name".into(),
            direction: SortDirection::Ascending,
        }],
        ..PageRequest::default()
    };

    let first = storage.query_duplicate_books(&sorted, "admin").await.unwrap();
    assert_eq!(first.total_elements, 4);
    assert_eq!(
        first.content.iter().map(|book| book.name.as_str()).collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert!(first.content.iter().all(|book| !book.url.is_empty()));

    let second = storage
        .query_duplicate_books(
            &PageRequest {
                page: 1,
                ..sorted.clone()
            },
            "admin",
        )
        .await
        .unwrap();
    assert_eq!(
        second.content.iter().map(|book| book.name.as_str()).collect::<Vec<_>>(),
        ["D", "E"]
    );

    fs::remove_file(temporary.path().join("library/Series/B.cbz")).unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let all = storage
        .query_duplicate_books(
            &PageRequest {
                unpaged: true,
                ..sorted
            },
            "admin",
        )
        .await
        .unwrap();
    assert_eq!(all.total_elements, 4);
    assert_eq!(all.size, 20);
    assert!(all.content.iter().any(|book| book.name == "B" && book.deleted));
    assert!(!all.content.iter().any(|book| book.name == "C" || book.name == "F"));
}

#[tokio::test]
async fn duplicate_query_rejects_unknown_storage_sorts() {
    let (_temporary, storage, _library) = duplicate_fixture().await;
    let error = storage
        .query_duplicate_books(
            &PageRequest {
                sort: vec![SortOrder {
                    property: "unknown".into(),
                    direction: SortDirection::Ascending,
                }],
                ..PageRequest::default()
            },
            "admin",
        )
        .await
        .unwrap_err();
    assert!(matches!(error, StorageError::InvalidQuery(_)));
}
