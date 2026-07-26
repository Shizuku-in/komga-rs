//! Atomic persistence for source-file imports and upgrades.

use komga_core::{Book, HistoricalEvent, ScannedSidecar, Series};

use crate::{NumberingRefreshTarget, Result, Storage, content::insert_book, history::insert_historical_event};

/// Durable identities and follow-up work produced by one committed import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookImportCommitResult {
    /// Imported book identity.
    pub book: Book,
    /// Permanently removed upgrade target, when applicable.
    pub deleted_book: Option<Book>,
    /// Existing books whose unlocked numbering changed.
    pub numbering_refreshes: Vec<NumberingRefreshTarget>,
}

impl Storage {
    /// Commits a scanned imported book, optional upgrade transfer, sidecars, and audit records atomically.
    ///
    /// Filesystem changes must be completed by the caller before invoking this
    /// method. Sync-point snapshots are intentionally preserved so clients can
    /// observe the removed old identity and added new identity.
    ///
    /// # Errors
    ///
    /// Returns a database error and rolls back all main-database changes when
    /// any relation transfer, reorder, sidecar, or audit statement fails.
    pub async fn commit_book_import(
        &self,
        book: &Book,
        series: &Series,
        upgraded_book: Option<&Book>,
        sidecars: &[ScannedSidecar],
        removed_sidecar_paths: &[String],
        events: &[HistoricalEvent],
    ) -> Result<BookImportCommitResult> {
        let mut tx = self.pool.begin().await?;
        insert_book(&mut tx, book, &series.id, &series.library_id).await?;

        if let Some(previous) = upgraded_book {
            sqlx::query("DELETE FROM MEDIA WHERE BOOK_ID = ?")
                .bind(&book.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM BOOK_METADATA WHERE BOOK_ID = ?")
                .bind(&book.id)
                .execute(&mut *tx)
                .await?;

            for table in ["MEDIA_PAGE", "MEDIA_FILE"] {
                sqlx::query(&format!("UPDATE {table} SET BOOK_ID = ? WHERE BOOK_ID = ?"))
                    .bind(&book.id)
                    .bind(&previous.id)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query(
                "UPDATE MEDIA SET BOOK_ID = ?, STATUS = 'OUTDATED', LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE BOOK_ID = ?",
            )
            .bind(&book.id)
            .bind(&previous.id)
            .execute(&mut *tx)
            .await?;
            for table in ["BOOK_METADATA_AUTHOR", "BOOK_METADATA_TAG", "BOOK_METADATA_LINK"] {
                sqlx::query(&format!("UPDATE {table} SET BOOK_ID = ? WHERE BOOK_ID = ?"))
                    .bind(&book.id)
                    .bind(&previous.id)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query(
                "UPDATE BOOK_METADATA SET BOOK_ID = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE BOOK_ID = ?",
            )
            .bind(&book.id)
            .bind(&previous.id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE THUMBNAIL_BOOK SET BOOK_ID = ? WHERE BOOK_ID = ? AND TYPE = 'USER_UPLOADED'")
                .bind(&book.id)
                .bind(&previous.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE READ_PROGRESS SET BOOK_ID = ? WHERE BOOK_ID = ?")
                .bind(&book.id)
                .bind(&previous.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE READLIST SET LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID IN (SELECT READLIST_ID FROM READLIST_BOOK WHERE BOOK_ID = ?)",
            )
            .bind(&previous.id)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE READLIST_BOOK SET BOOK_ID = ? WHERE BOOK_ID = ?")
                .bind(&book.id)
                .bind(&previous.id)
                .execute(&mut *tx)
                .await?;

            for table in [
                "READ_PROGRESS",
                "READLIST_BOOK",
                "MEDIA_PAGE",
                "MEDIA_FILE",
                "THUMBNAIL_BOOK",
                "BOOK_METADATA_AUTHOR",
                "BOOK_METADATA_TAG",
                "BOOK_METADATA_LINK",
                "MEDIA",
                "BOOK_METADATA",
            ] {
                sqlx::query(&format!("DELETE FROM {table} WHERE BOOK_ID = ?"))
                    .bind(&previous.id)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("DELETE FROM BOOK WHERE ID = ?")
                .bind(&previous.id)
                .execute(&mut *tx)
                .await?;
        }

        if !removed_sidecar_paths.is_empty() {
            sqlx::query("DELETE FROM SIDECAR WHERE URL IN (SELECT value FROM json_each(?))")
                .bind(serde_json::to_string(removed_sidecar_paths).unwrap_or_else(|_| "[]".into()))
                .execute(&mut *tx)
                .await?;
        }
        for sidecar in sidecars {
            sqlx::query(
                r#"INSERT INTO SIDECAR (URL, PARENT_URL, LAST_MODIFIED_TIME, LIBRARY_ID)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT(URL) DO UPDATE SET
                     PARENT_URL=excluded.PARENT_URL,
                     LAST_MODIFIED_TIME=excluded.LAST_MODIFIED_TIME,
                     LIBRARY_ID=excluded.LIBRARY_ID"#,
            )
            .bind(sidecar.url.as_str())
            .bind(sidecar.parent_url.as_str())
            .bind(sidecar.last_modified_time)
            .bind(&series.library_id)
            .execute(&mut *tx)
            .await?;
        }

        if series.oneshot {
            sqlx::query(
                "UPDATE SERIES SET URL = ?, FILE_LAST_MODIFIED = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?",
            )
            .bind(book.url.as_str())
            .bind(book.file_last_modified)
            .bind(&series.id)
            .execute(&mut *tx)
            .await?;
        }
        let mut numbering_refreshes = Vec::new();
        crate::lifecycle::reorder_series(&mut tx, &series.id, &mut numbering_refreshes).await?;
        for event in events {
            insert_historical_event(&mut tx, event).await?;
        }
        tx.commit().await?;

        Ok(BookImportCommitResult {
            book: book.clone(),
            deleted_book: upgraded_book.cloned(),
            numbering_refreshes,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, num::NonZeroUsize, path::Path};

    use chrono::Local;
    use komga_core::{
        Dimension, EPUB_MEDIA_TYPE, Library, Media, MediaStatus, PageRequest, ReadListCreation, ReadProgress,
        SyncChangeKind, ThumbnailBook, User, new_id, scan_book_file, scan_library_files,
    };
    use url::Url;

    use super::*;

    async fn fixture() -> (tempfile::TempDir, Storage, Library, Series, Book) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Volume 1.cbz"), b"old").unwrap();
        let storage = Storage::connect_memory().await.unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        (temporary, storage, library, series, book)
    }

    fn imported(path: &Path, library: &Library, series: &Series) -> Book {
        let mut book = scan_book_file(path, &library.id, series.oneshot).unwrap();
        book.series_id = series.id.clone();
        book
    }

    #[tokio::test]
    async fn import_upgrade_transfers_owned_relations_and_deletes_old_identity() {
        let (temporary, storage, library, series, old) = fixture().await;
        storage
            .replace_media_analysis(&Media::result(
                &old.id,
                MediaStatus::Ready,
                Some("application/zip".into()),
                None,
            ))
            .await
            .unwrap();
        let mut metadata = storage.find_book_metadata(&old.id).await.unwrap().unwrap();
        metadata.summary = "kept summary".into();
        metadata.summary_lock = true;
        storage.save_book_metadata(&metadata).await.unwrap();
        storage
            .insert_book_thumbnail(
                &ThumbnailBook {
                    id: new_id(),
                    thumbnail: Some(vec![1]),
                    url: None,
                    selected: false,
                    thumbnail_type: "USER_UPLOADED".into(),
                    media_type: "image/jpeg".into(),
                    file_size: 1,
                    dimension: Dimension { width: 1, height: 1 },
                    book_id: old.id.clone(),
                },
                true,
            )
            .await
            .unwrap();
        storage
            .create_read_list(
                "readlist",
                &ReadListCreation {
                    name: "Upgrade".into(),
                    summary: String::new(),
                    ordered: true,
                    book_ids: vec![old.id.clone()],
                },
            )
            .await
            .unwrap();
        let reader = User::new("reader@example.org".into(), "unused".into());
        storage.create_user(&reader).await.unwrap();
        storage
            .save_read_progress(&ReadProgress {
                book_id: old.id.clone(),
                user_id: reader.id.clone(),
                page: 4,
                completed: false,
                read_date: Local::now().naive_local(),
                device_id: "device".into(),
                device_name: "Reader".into(),
                locator: None,
            })
            .await
            .unwrap();
        let destination = temporary.path().join("library/Series/Volume 2.cbz");
        fs::write(&destination, b"new").unwrap();
        let book = imported(&destination, &library, &series);
        let events = [
            HistoricalEvent::book_file_deleted_for_reason(&old, "File was deleted to import an upgrade"),
            HistoricalEvent::book_imported(&book, Path::new("/imports/Volume 2.cbz"), true),
        ];

        storage
            .commit_book_import(&book, &series, Some(&old), &[], &[], &events)
            .await
            .unwrap();

        assert!(storage.find_book_by_id(&old.id).await.unwrap().is_none());
        let saved = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved.number, 1);
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(media.status, MediaStatus::Outdated);
        assert_eq!(
            storage.find_book_metadata(&book.id).await.unwrap().unwrap().summary,
            "kept summary"
        );
        assert_eq!(storage.list_book_thumbnails(&book.id).await.unwrap().len(), 1);
        let read_list = storage.find_read_list("readlist", &reader).await.unwrap().unwrap();
        assert_eq!(read_list.book_ids.len(), 1);
        assert_eq!(read_list.book_ids[0], book.id);
        let progress = storage.find_read_progress(&book.id, &reader.id).await.unwrap().unwrap();
        assert_eq!(progress.page, 4);
        assert!(storage.find_read_progress(&old.id, &reader.id).await.unwrap().is_none());
        assert_eq!(
            storage
                .list_historical_events(&PageRequest {
                    unpaged: true,
                    ..PageRequest::default()
                })
                .await
                .unwrap()
                .total_elements,
            2
        );
    }

    #[tokio::test]
    async fn import_transaction_rolls_back_new_book_and_upgrade_deletion_on_history_failure() {
        let (temporary, storage, library, series, old) = fixture().await;
        let destination = temporary.path().join("library/Series/Volume 2.cbz");
        fs::write(&destination, b"new").unwrap();
        let book = imported(&destination, &library, &series);
        sqlx::query(
            "CREATE TRIGGER fail_import_history BEFORE INSERT ON HISTORICAL_EVENT BEGIN SELECT RAISE(FAIL, 'fail'); END",
        )
        .execute(&storage.pool)
        .await
        .unwrap();

        let result = storage
            .commit_book_import(
                &book,
                &series,
                Some(&old),
                &[],
                &[],
                std::slice::from_ref(&HistoricalEvent::book_imported(
                    &book,
                    Path::new("/imports/Volume 2.cbz"),
                    true,
                )),
            )
            .await;
        assert!(result.is_err());
        assert!(storage.find_book_by_id(&book.id).await.unwrap().is_none());
        assert!(storage.find_book_by_id(&old.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn analyzed_upgrade_is_removed_and_added_in_the_next_kobo_sync() {
        let (temporary, storage, library, series, old) = fixture().await;
        storage
            .replace_media_analysis(&Media::result(
                &old.id,
                MediaStatus::Ready,
                Some(EPUB_MEDIA_TYPE.into()),
                None,
            ))
            .await
            .unwrap();
        let user = User::new("sync@example.org".into(), "unused".into());
        storage.create_user(&user).await.unwrap();
        let before = storage.create_sync_point(&user, None).await.unwrap();
        let destination = temporary.path().join("library/Series/Volume 2.epub");
        fs::write(&destination, b"new").unwrap();
        let book = imported(&destination, &library, &series);
        storage
            .commit_book_import(
                &book,
                &series,
                Some(&old),
                &[],
                &[],
                std::slice::from_ref(&HistoricalEvent::book_imported(
                    &book,
                    Path::new("/imports/Volume 2.epub"),
                    true,
                )),
            )
            .await
            .unwrap();
        storage
            .replace_media_analysis(&Media::result(
                &book.id,
                MediaStatus::Ready,
                Some(EPUB_MEDIA_TYPE.into()),
                None,
            ))
            .await
            .unwrap();
        let after = storage.create_sync_point(&user, None).await.unwrap();
        let batch = storage
            .take_sync_batch(Some(&before.id), &after.id, &user.id, NonZeroUsize::new(10).unwrap())
            .await
            .unwrap();
        assert_eq!(batch.books.len(), 2);
        assert!(
            batch
                .books
                .iter()
                .any(|change| change.snapshot.book_id == old.id && change.kind == SyncChangeKind::Removed)
        );
        assert!(
            batch
                .books
                .iter()
                .any(|change| change.snapshot.book_id == book.id && change.kind == SyncChangeKind::Added)
        );
    }
}
