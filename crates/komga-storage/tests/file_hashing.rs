use chrono::Duration;
use komga_core::{Library, Media, MediaStatus, scan_library_files};
use komga_storage::Storage;
use std::fs;
use url::Url;

async fn scanned_book_fixture() -> (tempfile::TempDir, Storage, Library, komga_core::ScanResult) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Book.cbz"), b"hello").unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    let storage = Storage::connect_memory().await.unwrap();
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    (temporary, storage, library, scan)
}

#[tokio::test]
async fn scan_hash_reconciliation_preserves_or_invalidates_media_by_content() {
    let (_temporary, storage, library, mut scan) = scanned_book_fixture().await;
    scan.series[0].books[0].file_hash = "old-hash".into();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let book = storage
        .list_active_books_by_library_id(&library.id)
        .await
        .unwrap()
        .remove(0);
    storage
        .replace_media_analysis(&Media::result(
            &book.id,
            MediaStatus::Ready,
            Some("application/zip".into()),
            None,
        ))
        .await
        .unwrap();

    let mut unchanged = scan.clone();
    unchanged.series[0].books[0].file_last_modified += Duration::seconds(1);
    storage.apply_library_scan(&library.id, &unchanged, true).await.unwrap();
    assert_eq!(
        storage.find_media_by_book_id(&book.id).await.unwrap().unwrap().status,
        MediaStatus::Ready
    );
    assert_eq!(
        storage.find_book_by_id(&book.id).await.unwrap().unwrap().file_hash,
        "old-hash"
    );

    let mut changed = unchanged.clone();
    changed.series[0].books[0].file_last_modified += Duration::seconds(1);
    changed.series[0].books[0].file_hash = "new-hash".into();
    storage.apply_library_scan(&library.id, &changed, true).await.unwrap();
    assert_eq!(
        storage.find_media_by_book_id(&book.id).await.unwrap().unwrap().status,
        MediaStatus::Outdated
    );
    assert_eq!(
        storage.find_book_by_id(&book.id).await.unwrap().unwrap().file_hash,
        "new-hash"
    );

    let mut resized = changed;
    resized.series[0].books[0].file_size += 1;
    resized.series[0].books[0].file_hash.clear();
    storage.apply_library_scan(&library.id, &resized, true).await.unwrap();
    assert_eq!(storage.find_book_by_id(&book.id).await.unwrap().unwrap().file_hash, "");
    assert_eq!(
        storage.list_books_without_file_hash(&library.id).await.unwrap(),
        [book.id]
    );
}

#[tokio::test]
async fn file_hash_update_rejects_a_stale_source_snapshot() {
    let (_temporary, storage, library, scan) = scanned_book_fixture().await;
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let book = storage
        .list_active_books_by_library_id(&library.id)
        .await
        .unwrap()
        .remove(0);
    let mut stale = book.clone();
    stale.file_size += 1;

    assert!(!storage.update_book_file_hash_if_current(&stale, "stale").await.unwrap());
    assert!(
        storage
            .update_book_file_hash_if_current(&book, "current")
            .await
            .unwrap()
    );
    assert!(!storage.update_book_file_hash_if_current(&book, "second").await.unwrap());
    assert_eq!(
        storage.find_book_by_id(&book.id).await.unwrap().unwrap().file_hash,
        "current"
    );
}
