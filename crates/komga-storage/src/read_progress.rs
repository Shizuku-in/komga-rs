//! Kotlin-compatible reading-progress persistence and series aggregation.

use std::io::Write;

use chrono::NaiveDateTime;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use komga_core::{R2Locator, ReadProgress, SeriesReadProgressDto, series_read_progress};
use sqlx::{FromRow, Sqlite, Transaction};

use crate::{Result, Storage, StorageError};

#[derive(Debug, FromRow)]
struct ReadProgressRow {
    book_id: String,
    user_id: String,
    page: i32,
    completed: bool,
    read_date: Option<NaiveDateTime>,
    last_modified_date: NaiveDateTime,
    device_id: Option<String>,
    device_name: Option<String>,
    locator: Option<Vec<u8>>,
}

impl Storage {
    /// Computes one user's progress summary across all books in a series.
    pub async fn find_series_read_progress(&self, series_id: &str, user_id: &str) -> Result<SeriesReadProgressDto> {
        let books = sqlx::query_as::<_, (f64, Option<bool>)>(
            r#"SELECT bm.NUMBER_SORT, rp.COMPLETED
                 FROM BOOK b
                 JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
                 LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = ?
                WHERE b.SERIES_ID = ?"#,
        )
        .bind(user_id)
        .bind(series_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(series_read_progress(books))
    }

    /// Marks incomplete books in a series complete, optionally through a sort number.
    ///
    /// Returns the number of inserted or updated rows.
    pub async fn mark_series_read_completed(
        &self,
        series_id: &str,
        user_id: &str,
        through_number_sort: Option<f64>,
        read_date: NaiveDateTime,
    ) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query(
            r#"INSERT INTO READ_PROGRESS
                 (BOOK_ID, USER_ID, PAGE, COMPLETED, READ_DATE, DEVICE_ID, DEVICE_NAME, LOCATOR)
               SELECT b.ID, ?, m.PAGE_COUNT, 1, ?, '', '', NULL
                 FROM BOOK b
                 JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
                 JOIN MEDIA m ON m.BOOK_ID = b.ID
                 LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = ?
                WHERE b.SERIES_ID = ?
                  AND (? IS NULL OR bm.NUMBER_SORT <= ?)
                  AND (rp.BOOK_ID IS NULL OR rp.COMPLETED = 0)
               ON CONFLICT(BOOK_ID, USER_ID) DO UPDATE SET
                 PAGE = excluded.PAGE,
                 COMPLETED = 1,
                 READ_DATE = excluded.READ_DATE,
                 LAST_MODIFIED_DATE = CURRENT_TIMESTAMP,
                 DEVICE_ID = '',
                 DEVICE_NAME = '',
                 LOCATOR = NULL"#,
        )
        .bind(user_id)
        .bind(read_date)
        .bind(user_id)
        .bind(series_id)
        .bind(through_number_sort)
        .bind(through_number_sort)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed > 0 {
            aggregate_series_progress_by_series(&mut tx, series_id, user_id).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    /// Deletes all of one user's progress rows in a series atomically.
    pub async fn delete_series_read_progress(&self, series_id: &str, user_id: &str) -> Result<u64> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query(
            "DELETE FROM READ_PROGRESS WHERE USER_ID = ? AND BOOK_ID IN (SELECT ID FROM BOOK WHERE SERIES_ID = ?)",
        )
        .bind(user_id)
        .bind(series_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed > 0 {
            aggregate_series_progress_by_series(&mut tx, series_id, user_id).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    /// Loads one user's progress for a book, including its Readium locator.
    ///
    /// Historical rows with a null read date fall back to their modification
    /// date. Invalid legacy locator blobs are treated as absent, matching Kotlin.
    ///
    /// # Errors
    ///
    /// Returns a database error when the row cannot be queried.
    pub async fn find_read_progress(&self, book_id: &str, user_id: &str) -> Result<Option<ReadProgress>> {
        let row = sqlx::query_as::<_, ReadProgressRow>(
            r#"SELECT BOOK_ID AS book_id, USER_ID AS user_id, PAGE AS page,
                      COMPLETED AS completed, READ_DATE AS read_date,
                      LAST_MODIFIED_DATE AS last_modified_date,
                      DEVICE_ID AS device_id, DEVICE_NAME AS device_name,
                      LOCATOR AS locator
                 FROM READ_PROGRESS WHERE BOOK_ID = ? AND USER_ID = ?"#,
        )
        .bind(book_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(ReadProgressRow::into_domain))
    }

    /// Inserts or replaces one user's progress and refreshes its series totals.
    ///
    /// The original creation timestamp is retained on update. The progress row
    /// and `READ_PROGRESS_SERIES` aggregation are committed atomically.
    ///
    /// # Errors
    ///
    /// Returns an error when the locator cannot be encoded or the transaction fails.
    pub async fn save_read_progress(&self, progress: &ReadProgress) -> Result<()> {
        let locator = encode_locator(progress.locator.as_ref())?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO READ_PROGRESS
                 (BOOK_ID, USER_ID, PAGE, COMPLETED, READ_DATE, DEVICE_ID, DEVICE_NAME, LOCATOR)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(BOOK_ID, USER_ID) DO UPDATE SET
                 PAGE = excluded.PAGE,
                 COMPLETED = excluded.COMPLETED,
                 READ_DATE = excluded.READ_DATE,
                 LAST_MODIFIED_DATE = CURRENT_TIMESTAMP,
                 DEVICE_ID = excluded.DEVICE_ID,
                 DEVICE_NAME = excluded.DEVICE_NAME,
                 LOCATOR = excluded.LOCATOR"#,
        )
        .bind(&progress.book_id)
        .bind(&progress.user_id)
        .bind(progress.page)
        .bind(progress.completed)
        .bind(progress.read_date)
        .bind(&progress.device_id)
        .bind(&progress.device_name)
        .bind(locator)
        .execute(&mut *tx)
        .await?;
        aggregate_series_progress(&mut tx, &progress.book_id, &progress.user_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes one user's progress and refreshes its series totals.
    ///
    /// Returns whether a progress row existed.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction fails.
    pub async fn delete_read_progress(&self, book_id: &str, user_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let deleted = sqlx::query("DELETE FROM READ_PROGRESS WHERE BOOK_ID = ? AND USER_ID = ?")
            .bind(book_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        if deleted {
            aggregate_series_progress(&mut tx, book_id, user_id).await?;
        }
        tx.commit().await?;
        Ok(deleted)
    }
}

impl ReadProgressRow {
    fn into_domain(self) -> ReadProgress {
        ReadProgress {
            book_id: self.book_id,
            user_id: self.user_id,
            page: self.page,
            completed: self.completed,
            read_date: self.read_date.unwrap_or(self.last_modified_date),
            device_id: self.device_id.unwrap_or_default(),
            device_name: self.device_name.unwrap_or_default(),
            locator: decode_locator(self.locator.as_deref()),
        }
    }
}

async fn aggregate_series_progress(tx: &mut Transaction<'_, Sqlite>, book_id: &str, user_id: &str) -> Result<()> {
    let series_id = sqlx::query_scalar::<_, String>("SELECT SERIES_ID FROM BOOK WHERE ID = ?")
        .bind(book_id)
        .fetch_optional(&mut **tx)
        .await?;
    let Some(series_id) = series_id else {
        return Ok(());
    };
    aggregate_series_progress_by_series(tx, &series_id, user_id).await
}

pub(crate) async fn aggregate_series_progress_by_series(
    tx: &mut Transaction<'_, Sqlite>,
    series_id: &str,
    user_id: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM READ_PROGRESS_SERIES WHERE SERIES_ID = ? AND USER_ID = ?")
        .bind(series_id)
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        r#"INSERT INTO READ_PROGRESS_SERIES
             (SERIES_ID, USER_ID, READ_COUNT, IN_PROGRESS_COUNT,
              MOST_RECENT_READ_DATE, LAST_MODIFIED_DATE)
           SELECT b.SERIES_ID, rp.USER_ID,
                  SUM(CASE WHEN rp.COMPLETED = 1 THEN 1 ELSE 0 END),
                  SUM(CASE WHEN rp.COMPLETED = 0 THEN 1 ELSE 0 END),
                  MAX(rp.READ_DATE), CURRENT_TIMESTAMP
             FROM BOOK b
             JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID
            WHERE b.SERIES_ID = ? AND rp.USER_ID = ?
            GROUP BY b.SERIES_ID, rp.USER_ID"#,
    )
    .bind(series_id)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn encode_locator(locator: Option<&R2Locator>) -> Result<Option<Vec<u8>>> {
    let Some(locator) = locator else {
        return Ok(None);
    };
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    serde_json::to_writer(&mut encoder, locator)
        .map_err(|error| StorageError::InvalidData(format!("could not serialize read-progress locator: {error}")))?;
    encoder
        .flush()
        .map_err(|error| StorageError::InvalidData(format!("could not compress read-progress locator: {error}")))?;
    encoder
        .finish()
        .map(Some)
        .map_err(|error| StorageError::InvalidData(format!("could not finish read-progress locator: {error}")))
}

fn decode_locator(bytes: Option<&[u8]>) -> Option<R2Locator> {
    serde_json::from_reader(GzDecoder::new(bytes?)).ok()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use chrono::Utc;
    use komga_core::{Library, R2Location, ScanInterval, User, UserRole, scan_library_files};
    use url::Url;

    use super::*;

    async fn fixture(storage: &Storage, root: &Path) -> (User, Vec<komga_core::Book>) {
        let library_root = root.join("library");
        let series = library_root.join("Series");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("Book 1.cbz"), [1]).unwrap();
        fs::write(series.join("Book 2.cbz"), [2]).unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(library_root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap();
        let books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.roles.insert(UserRole::PageStreaming);
        storage.create_user(&user).await.unwrap();
        (user, books)
    }

    fn progress(book_id: &str, user_id: &str, page: i32, completed: bool) -> ReadProgress {
        ReadProgress {
            book_id: book_id.into(),
            user_id: user_id.into(),
            page,
            completed,
            read_date: Utc::now().naive_utc(),
            device_id: "device".into(),
            device_name: "Reader".into(),
            locator: Some(R2Locator {
                href: "chapter.xhtml".into(),
                media_type: "application/xhtml+xml".into(),
                title: None,
                locations: Some(R2Location {
                    progression: Some(0.5),
                    position: Some(page),
                    total_progression: Some(0.5),
                    ..Default::default()
                }),
                text: None,
                kobo_span: Some("kobo.1.1".into()),
            }),
        }
    }

    #[tokio::test]
    async fn progress_round_trips_and_rebuilds_series_aggregation() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (user, books) = fixture(&storage, temporary.path()).await;
        let first = progress(&books[0].id, &user.id, 5, false);
        storage.save_read_progress(&first).await.unwrap();
        assert_eq!(
            storage.find_read_progress(&books[0].id, &user.id).await.unwrap(),
            Some(first)
        );
        let blob =
            sqlx::query_scalar::<_, Vec<u8>>("SELECT LOCATOR FROM READ_PROGRESS WHERE BOOK_ID = ? AND USER_ID = ?")
                .bind(&books[0].id)
                .bind(&user.id)
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert!(blob.starts_with(&[0x1f, 0x8b]));

        storage
            .save_read_progress(&progress(&books[1].id, &user.id, 10, true))
            .await
            .unwrap();
        let aggregate = sqlx::query_as::<_, (i32, i32)>(
            "SELECT READ_COUNT, IN_PROGRESS_COUNT FROM READ_PROGRESS_SERIES WHERE SERIES_ID = ? AND USER_ID = ?",
        )
        .bind(&books[0].series_id)
        .bind(&user.id)
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(aggregate, (1, 1));

        storage
            .save_read_progress(&progress(&books[0].id, &user.id, 10, true))
            .await
            .unwrap();
        assert!(storage.delete_read_progress(&books[1].id, &user.id).await.unwrap());
        let aggregate = sqlx::query_as::<_, (i32, i32)>(
            "SELECT READ_COUNT, IN_PROGRESS_COUNT FROM READ_PROGRESS_SERIES WHERE SERIES_ID = ? AND USER_ID = ?",
        )
        .bind(&books[0].series_id)
        .bind(&user.id)
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(aggregate, (1, 0));
        assert!(!storage.delete_read_progress(&books[1].id, &user.id).await.unwrap());
        assert!(storage.delete_read_progress(&books[0].id, &user.id).await.unwrap());
        assert!(
            sqlx::query_scalar::<_, String>(
                "SELECT SERIES_ID FROM READ_PROGRESS_SERIES WHERE SERIES_ID = ? AND USER_ID = ?",
            )
            .bind(&books[0].series_id)
            .bind(&user.id)
            .fetch_optional(&storage.pool)
            .await
            .unwrap()
            .is_none()
        );
    }

    #[tokio::test]
    async fn null_read_date_falls_back_and_invalid_locator_is_ignored() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (user, books) = fixture(&storage, temporary.path()).await;
        sqlx::query(
            "INSERT INTO READ_PROGRESS (BOOK_ID, USER_ID, PAGE, COMPLETED, READ_DATE, LOCATOR) VALUES (?, ?, 1, 0, NULL, ?)",
        )
        .bind(&books[0].id)
        .bind(&user.id)
        .bind(b"invalid".as_slice())
        .execute(&storage.pool)
        .await
        .unwrap();
        let row = storage
            .find_read_progress(&books[0].id, &user.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.locator, None);
        let last_modified = sqlx::query_scalar::<_, NaiveDateTime>(
            "SELECT LAST_MODIFIED_DATE FROM READ_PROGRESS WHERE BOOK_ID = ? AND USER_ID = ?",
        )
        .bind(&books[0].id)
        .bind(&user.id)
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(row.read_date, last_modified);
    }

    #[tokio::test]
    async fn series_progress_completion_threshold_and_deletion_are_atomic_and_idempotent() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (user, books) = fixture(&storage, temporary.path()).await;
        sqlx::query("UPDATE MEDIA SET PAGE_COUNT = CASE BOOK_ID WHEN ? THEN 12 ELSE 24 END")
            .bind(&books[0].id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE BOOK_METADATA SET NUMBER_SORT = CASE BOOK_ID WHEN ? THEN 1.25 ELSE 2.5 END")
            .bind(&books[0].id)
            .execute(&storage.pool)
            .await
            .unwrap();
        let original = progress(&books[0].id, &user.id, 12, true);
        storage.save_read_progress(&original).await.unwrap();
        let in_progress = progress(&books[1].id, &user.id, 4, false);
        storage.save_read_progress(&in_progress).await.unwrap();

        let changed = storage
            .mark_series_read_completed(&books[0].series_id, &user.id, Some(2.5), Utc::now().naive_utc())
            .await
            .unwrap();
        assert_eq!(changed, 1);
        assert_eq!(
            storage.find_read_progress(&books[0].id, &user.id).await.unwrap(),
            Some(original)
        );
        let second = storage
            .find_read_progress(&books[1].id, &user.id)
            .await
            .unwrap()
            .unwrap();
        assert!(second.completed);
        assert_eq!(second.page, 24);
        assert_eq!(second.device_id, "");
        assert_eq!(second.locator, None);
        assert_eq!(
            storage
                .mark_series_read_completed(&books[0].series_id, &user.id, None, Utc::now().naive_utc())
                .await
                .unwrap(),
            0
        );
        let summary = storage
            .find_series_read_progress(&books[0].series_id, &user.id)
            .await
            .unwrap();
        assert_eq!((summary.books_count, summary.books_read_count), (2, 2));
        assert_eq!(
            storage
                .delete_series_read_progress(&books[0].series_id, &user.id)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            storage
                .delete_series_read_progress(&books[0].series_id, &user.id)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            storage
                .find_series_read_progress(&books[0].series_id, &user.id)
                .await
                .unwrap(),
            SeriesReadProgressDto {
                books_count: 2,
                books_unread_count: 2,
                max_number_sort: 2.5,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn series_progress_rolls_back_when_aggregation_fails() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (user, books) = fixture(&storage, temporary.path()).await;
        sqlx::query(
            "CREATE TRIGGER fail_series_progress BEFORE INSERT ON READ_PROGRESS_SERIES BEGIN SELECT RAISE(FAIL, 'fail'); END",
        )
        .execute(&storage.pool)
        .await
        .unwrap();
        assert!(
            storage
                .mark_series_read_completed(&books[0].series_id, &user.id, None, Utc::now().naive_utc())
                .await
                .is_err()
        );
        let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM READ_PROGRESS WHERE USER_ID = ?")
            .bind(&user.id)
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
