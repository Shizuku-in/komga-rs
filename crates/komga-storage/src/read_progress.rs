//! Kotlin-compatible reading-progress persistence and series aggregation.

use std::io::Write;

use chrono::NaiveDateTime;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use komga_core::{R2Locator, ReadProgress};
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
    sqlx::query("DELETE FROM READ_PROGRESS_SERIES WHERE SERIES_ID = ? AND USER_ID = ?")
        .bind(&series_id)
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
}
