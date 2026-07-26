//! Atomic persistence for user-requested source-file deletion.

use chrono::NaiveDateTime;
use komga_core::HistoricalEvent;

use crate::{Result, Storage, history::insert_historical_event};

impl Storage {
    /// Soft-deletes source-backed content and writes all related audit events atomically.
    ///
    /// Book and Series rows that no longer exist are ignored. Any SQL or constraint
    /// failure rolls back both timestamp updates and every historical event.
    ///
    /// # Errors
    ///
    /// Returns a database error when an update or historical-event insert fails.
    pub async fn commit_source_deletion(
        &self,
        book_ids: &[String],
        series_ids: &[String],
        events: &[HistoricalEvent],
        timestamp: NaiveDateTime,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for book_id in book_ids {
            sqlx::query("UPDATE BOOK SET DELETED_DATE = ?, LAST_MODIFIED_DATE = ? WHERE ID = ?")
                .bind(timestamp)
                .bind(timestamp)
                .bind(book_id)
                .execute(&mut *tx)
                .await?;
        }
        for series_id in series_ids {
            sqlx::query("UPDATE SERIES SET DELETED_DATE = ?, LAST_MODIFIED_DATE = ? WHERE ID = ?")
                .bind(timestamp)
                .bind(timestamp)
                .bind(series_id)
                .execute(&mut *tx)
                .await?;
        }
        for event in events {
            insert_historical_event(&mut tx, event).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use chrono::{Duration, Local};
    use komga_core::{
        EPUB_MEDIA_TYPE, HistoricalEvent, Library, Media, MediaStatus, PageRequest, SyncChangeKind, User,
        scan_library_files,
    };
    use url::Url;

    use super::*;

    async fn fixture() -> (tempfile::TempDir, Storage, komga_core::Series, komga_core::Book) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        std::fs::create_dir_all(&series_path).unwrap();
        std::fs::write(series_path.join("Volume.cbz"), [1_u8]).unwrap();
        let storage = Storage::connect_memory().await.unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        (temporary, storage, series, book)
    }

    #[tokio::test]
    async fn source_deletion_soft_deletes_content_and_inserts_all_events() {
        let (_temporary, storage, series, book) = fixture().await;
        let timestamp = Local::now().naive_local() + Duration::seconds(1);
        let events = [
            HistoricalEvent::book_file_deleted(&book),
            HistoricalEvent::series_folder_deleted(&series.id, &series.url.to_file_path().unwrap()),
        ];

        storage
            .commit_source_deletion(
                std::slice::from_ref(&book.id),
                std::slice::from_ref(&series.id),
                &events,
                timestamp,
            )
            .await
            .unwrap();

        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let saved_series = storage.find_series_by_id(&series.id).await.unwrap().unwrap();
        assert_eq!(saved_book.deleted_date, Some(timestamp));
        assert_eq!(saved_book.last_modified_date, Some(timestamp));
        assert_eq!(saved_series.deleted_date, Some(timestamp));
        assert_eq!(saved_series.last_modified_date, Some(timestamp));
        let history = storage
            .list_historical_events(&PageRequest {
                unpaged: true,
                ..PageRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(history.total_elements, 2);
    }

    #[tokio::test]
    async fn source_deletion_rolls_back_updates_when_an_event_insert_fails() {
        let (_temporary, storage, series, book) = fixture().await;
        let original_book_modified = book.last_modified_date;
        let original_series_modified = series.last_modified_date;
        let event = HistoricalEvent::book_file_deleted(&book);
        storage.insert_historical_event(&event).await.unwrap();

        let result = storage
            .commit_source_deletion(
                std::slice::from_ref(&book.id),
                std::slice::from_ref(&series.id),
                std::slice::from_ref(&event),
                Local::now().naive_local() + Duration::seconds(1),
            )
            .await;
        assert!(result.is_err());

        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let saved_series = storage.find_series_by_id(&series.id).await.unwrap().unwrap();
        assert_eq!(saved_book.deleted_date, None);
        assert_eq!(saved_book.last_modified_date, original_book_modified);
        assert_eq!(saved_series.deleted_date, None);
        assert_eq!(saved_series.last_modified_date, original_series_modified);
        assert_eq!(
            storage
                .list_historical_events(&PageRequest {
                    unpaged: true,
                    ..PageRequest::default()
                })
                .await
                .unwrap()
                .total_elements,
            1
        );
    }

    #[tokio::test]
    async fn source_deletion_is_removed_from_the_next_kobo_sync_point() {
        let (_temporary, storage, _series, book) = fixture().await;
        storage
            .replace_media_analysis(&Media::result(
                &book.id,
                MediaStatus::Ready,
                Some(EPUB_MEDIA_TYPE.into()),
                None,
            ))
            .await
            .unwrap();
        let user = User::new("sync@example.org".into(), "unused".into());
        storage.create_user(&user).await.unwrap();
        let before = storage.create_sync_point(&user, None).await.unwrap();

        storage
            .commit_source_deletion(
                std::slice::from_ref(&book.id),
                &[],
                std::slice::from_ref(&HistoricalEvent::book_file_deleted(&book)),
                Local::now().naive_local(),
            )
            .await
            .unwrap();
        let after = storage.create_sync_point(&user, None).await.unwrap();
        let batch = storage
            .take_sync_batch(Some(&before.id), &after.id, &user.id, NonZeroUsize::new(10).unwrap())
            .await
            .unwrap();
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].snapshot.book_id, book.id);
        assert_eq!(batch.books[0].kind, SyncChangeKind::Removed);
    }
}
