//! Atomic persistence for source conversion and extension repair.

use komga_core::{Book, HistoricalEvent, Media, MediaStatus, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, ZIP_MEDIA_TYPE};
use sqlx::FromRow;

use crate::{Result, Storage, history::insert_historical_event};

#[derive(Debug, FromRow)]
struct PageState {
    number: i32,
    file_name: String,
    media_type: String,
    file_hash: String,
    file_hash_algorithm: Option<String>,
    file_size: Option<i64>,
}

impl Storage {
    /// Lists active books whose analyzed media can be converted to CBZ.
    pub async fn list_books_convertible_to_cbz(&self, library_id: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            r#"SELECT B.ID, B.SERIES_ID
                 FROM BOOK B JOIN MEDIA M ON M.BOOK_ID = B.ID
                WHERE B.LIBRARY_ID = ? AND B.DELETED_DATE IS NULL
                  AND M.MEDIA_TYPE IN (?, ?)
                ORDER BY B.SERIES_ID, B.NUMBER, B.URL"#,
        )
        .bind(library_id)
        .bind(RAR4_MEDIA_TYPE)
        .bind(RAR5_MEDIA_TYPE)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Lists active books whose stored media type disagrees with the source extension.
    pub async fn list_books_with_mismatched_extensions(&self, library_id: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            r#"SELECT B.ID, B.SERIES_ID
                 FROM BOOK B JOIN MEDIA M ON M.BOOK_ID = B.ID
                WHERE B.LIBRARY_ID = ? AND B.DELETED_DATE IS NULL
                  AND CASE
                    WHEN M.MEDIA_TYPE IN (?, ?) THEN lower(B.URL) NOT LIKE '%.cbr'
                    WHEN M.MEDIA_TYPE = ? THEN lower(B.URL) NOT LIKE '%.cbz'
                    WHEN M.MEDIA_TYPE = 'application/pdf' THEN lower(B.URL) NOT LIKE '%.pdf'
                    WHEN M.MEDIA_TYPE = 'application/epub+zip' THEN lower(B.URL) NOT LIKE '%.epub'
                    ELSE 0
                  END
                ORDER BY B.SERIES_ID, B.NUMBER, B.URL"#,
        )
        .bind(library_id)
        .bind(RAR4_MEDIA_TYPE)
        .bind(RAR5_MEDIA_TYPE)
        .bind(ZIP_MEDIA_TYPE)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Atomically persists a validated RAR-to-CBZ replacement while its snapshots remain current.
    ///
    /// Page hashes and their algorithm identifiers are retained only for exact
    /// filename, media-type, and size matches. Returns `false` when another
    /// worker changed the book or media after conversion began.
    pub async fn commit_book_conversion_if_current(
        &self,
        original_book: &Book,
        original_media: &Media,
        converted_book: &Book,
        converted_media: &Media,
        events: &[HistoricalEvent],
    ) -> Result<bool> {
        let (Some(original_type), Some(original_media_modified)) =
            (original_media.media_type.as_deref(), original_media.last_modified_date)
        else {
            return Ok(false);
        };
        if original_book.id != converted_book.id
            || original_media.book_id != original_book.id
            || converted_media.book_id != original_book.id
            || original_media.status != MediaStatus::Ready
            || !matches!(original_type, RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE)
            || converted_media.status != MediaStatus::Ready
            || converted_media.media_type.as_deref() != Some(ZIP_MEDIA_TYPE)
        {
            return Ok(false);
        }

        let mut tx = self.pool.begin().await?;
        let current = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                 SELECT 1 FROM BOOK B JOIN MEDIA M ON M.BOOK_ID = B.ID
                  WHERE B.ID = ? AND B.URL = ? AND B.FILE_SIZE = ?
                    AND B.FILE_LAST_MODIFIED = ? AND B.DELETED_DATE IS NULL
                    AND M.STATUS = ? AND M.MEDIA_TYPE = ? AND M.PAGE_COUNT = ?
                    AND M.LAST_MODIFIED_DATE = ?)"#,
        )
        .bind(&original_book.id)
        .bind(original_book.url.as_str())
        .bind(original_book.file_size)
        .bind(original_book.file_last_modified)
        .bind(original_media.status.as_str())
        .bind(original_type)
        .bind(original_media.page_count)
        .bind(original_media_modified)
        .fetch_one(&mut *tx)
        .await?;
        if !current {
            tx.rollback().await?;
            return Ok(false);
        }

        let pages = sqlx::query_as::<_, PageState>(
            r#"SELECT NUMBER AS number, FILE_NAME AS file_name, MEDIA_TYPE AS media_type,
                      FILE_HASH AS file_hash, FILE_HASH_ALGORITHM AS file_hash_algorithm,
                      FILE_SIZE AS file_size
                 FROM MEDIA_PAGE WHERE BOOK_ID = ? ORDER BY NUMBER"#,
        )
        .bind(&original_book.id)
        .fetch_all(&mut *tx)
        .await?;
        if pages.len() != original_media.pages.len()
            || pages
                .iter()
                .zip(&original_media.pages)
                .enumerate()
                .any(|(index, (row, page))| {
                    usize::try_from(row.number).ok() != Some(index)
                        || row.file_name != page.file_name
                        || row.media_type != page.media_type
                        || row.file_hash != page.file_hash
                        || row.file_size != page.file_size
                })
        {
            tx.rollback().await?;
            return Ok(false);
        }

        let updated = sqlx::query(
            r#"UPDATE BOOK SET NAME = ?, URL = ?, FILE_SIZE = ?, FILE_LAST_MODIFIED = ?,
                               FILE_HASH = '', FILE_HASH_KOREADER = '',
                               LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE ID = ? AND URL = ? AND FILE_SIZE = ? AND FILE_LAST_MODIFIED = ?
                   AND DELETED_DATE IS NULL"#,
        )
        .bind(&converted_book.name)
        .bind(converted_book.url.as_str())
        .bind(converted_book.file_size)
        .bind(converted_book.file_last_modified)
        .bind(&original_book.id)
        .bind(original_book.url.as_str())
        .bind(original_book.file_size)
        .bind(original_book.file_last_modified)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }

        sqlx::query(
            r#"UPDATE MEDIA SET STATUS = ?, MEDIA_TYPE = ?, COMMENT = ?, PAGE_COUNT = ?,
                      EPUB_DIVINA_COMPATIBLE = ?, EPUB_IS_KEPUB = ?,
                      EXTENSION_CLASS = NULL, EXTENSION_VALUE_BLOB = NULL,
                      LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE BOOK_ID = ?"#,
        )
        .bind(converted_media.status.as_str())
        .bind(&converted_media.media_type)
        .bind(&converted_media.comment)
        .bind(converted_media.page_count)
        .bind(converted_media.epub_divina_compatible)
        .bind(converted_media.epub_is_kepub)
        .bind(&original_book.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM MEDIA_PAGE WHERE BOOK_ID = ?")
            .bind(&original_book.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM MEDIA_FILE WHERE BOOK_ID = ?")
            .bind(&original_book.id)
            .execute(&mut *tx)
            .await?;
        for (number, page) in converted_media.pages.iter().enumerate() {
            let Some(previous) = pages.iter().find(|row| {
                row.file_name == page.file_name && row.media_type == page.media_type && row.file_size == page.file_size
            }) else {
                tx.rollback().await?;
                return Ok(false);
            };
            sqlx::query(
                r#"INSERT INTO MEDIA_PAGE
                     (BOOK_ID, FILE_NAME, MEDIA_TYPE, NUMBER, WIDTH, HEIGHT,
                      FILE_HASH, FILE_SIZE, FILE_HASH_ALGORITHM)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&original_book.id)
            .bind(&page.file_name)
            .bind(&page.media_type)
            .bind(i32::try_from(number).unwrap_or(i32::MAX))
            .bind(page.dimension.map(|value| value.width))
            .bind(page.dimension.map(|value| value.height))
            .bind(&page.file_hash)
            .bind(page.file_size)
            .bind(&previous.file_hash_algorithm)
            .execute(&mut *tx)
            .await?;
        }
        for file in &converted_media.files {
            sqlx::query(
                "INSERT INTO MEDIA_FILE (BOOK_ID, FILE_NAME, MEDIA_TYPE, SUB_TYPE, FILE_SIZE) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&original_book.id)
            .bind(&file.file_name)
            .bind(&file.media_type)
            .bind(&file.sub_type)
            .bind(file.file_size)
            .execute(&mut *tx)
            .await?;
        }
        update_oneshot_series(&mut tx, original_book, converted_book).await?;
        for event in events {
            insert_historical_event(&mut tx, event).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Atomically persists a filename-extension repair while the source snapshot remains current.
    pub async fn commit_book_extension_repair_if_current(
        &self,
        original_book: &Book,
        repaired_book: &Book,
    ) -> Result<bool> {
        if original_book.id != repaired_book.id {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            r#"UPDATE BOOK SET NAME = ?, URL = ?, FILE_SIZE = ?, FILE_LAST_MODIFIED = ?,
                               FILE_HASH = '', FILE_HASH_KOREADER = '',
                               LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE ID = ? AND URL = ? AND FILE_SIZE = ? AND FILE_LAST_MODIFIED = ?
                   AND DELETED_DATE IS NULL"#,
        )
        .bind(&repaired_book.name)
        .bind(repaired_book.url.as_str())
        .bind(repaired_book.file_size)
        .bind(repaired_book.file_last_modified)
        .bind(&original_book.id)
        .bind(original_book.url.as_str())
        .bind(original_book.file_size)
        .bind(original_book.file_last_modified)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }
        update_oneshot_series(&mut tx, original_book, repaired_book).await?;
        tx.commit().await?;
        Ok(true)
    }
}

async fn update_oneshot_series(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    original: &Book,
    updated: &Book,
) -> Result<()> {
    if original.oneshot {
        sqlx::query(
            r#"UPDATE SERIES SET NAME = ?, URL = ?, FILE_LAST_MODIFIED = ?,
                                 LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE ID = ? AND LIBRARY_ID = ? AND ONESHOT = 1"#,
        )
        .bind(&updated.name)
        .bind(updated.url.as_str())
        .bind(updated.file_last_modified)
        .bind(&original.series_id)
        .bind(&original.library_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{BookPage, Library, MediaFile, PageRequest, RAR4_MEDIA_TYPE, scan_book_file, scan_library_files};
    use url::Url;

    use super::*;

    async fn fixture() -> (tempfile::TempDir, Storage, Library, komga_core::Series, Book, Media) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_path = root.join("Series");
        fs::create_dir_all(&series_path).unwrap();
        fs::write(series_path.join("Volume.cbr"), b"rar source").unwrap();
        let storage = Storage::connect_memory().await.unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        let media = ready_media(&book.id, RAR4_MEDIA_TYPE);
        storage.replace_media_analysis(&media).await.unwrap();
        sqlx::query("UPDATE MEDIA_PAGE SET FILE_HASH_ALGORITHM = 'rgb24-v1' WHERE BOOK_ID = ?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE BOOK SET FILE_HASH = 'source-hash', FILE_HASH_KOREADER = 'koreader-hash' WHERE ID = ?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        let book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        (temporary, storage, library, series, book, media)
    }

    fn ready_media(book_id: &str, media_type: &str) -> Media {
        Media {
            status: MediaStatus::Ready,
            media_type: Some(media_type.into()),
            page_count: 1,
            pages: vec![BookPage {
                file_name: "pages/01.png".into(),
                media_type: "image/png".into(),
                dimension: None,
                file_hash: "page-hash".into(),
                file_size: Some(4),
            }],
            files: vec![MediaFile {
                file_name: "ComicInfo.xml".into(),
                media_type: Some("application/xml".into()),
                sub_type: None,
                file_size: Some(8),
            }],
            comment: None,
            extension: None,
            book_id: book_id.into(),
            epub_divina_compatible: false,
            epub_is_kepub: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    fn converted_book(temporary: &tempfile::TempDir, original: &Book) -> Book {
        let path = temporary.path().join("library/Series/Volume.cbz");
        fs::write(&path, b"zip replacement").unwrap();
        let scanned = scan_book_file(&path, &original.library_id, original.oneshot).unwrap();
        let mut converted = original.clone();
        converted.name = scanned.name;
        converted.url = scanned.url;
        converted.file_size = scanned.file_size;
        converted.file_last_modified = scanned.file_last_modified;
        converted.file_hash.clear();
        converted.file_hash_koreader.clear();
        converted
    }

    #[tokio::test]
    async fn conversion_commit_updates_snapshots_audits_and_preserves_page_hash_algorithm() {
        let (temporary, storage, library, _series, book, media) = fixture().await;
        assert_eq!(
            storage.list_books_convertible_to_cbz(&library.id).await.unwrap(),
            [(book.id.clone(), book.series_id.clone())]
        );
        assert!(
            storage
                .list_books_with_mismatched_extensions(&library.id)
                .await
                .unwrap()
                .is_empty()
        );
        let converted = converted_book(&temporary, &book);
        let converted_media = ready_media(&book.id, ZIP_MEDIA_TYPE);
        let events = [
            HistoricalEvent::book_file_deleted_for_reason(&book, "File was deleted after conversion to CBZ"),
            HistoricalEvent::book_converted(&converted, &book),
        ];

        assert!(
            storage
                .commit_book_conversion_if_current(&book, &media, &converted, &converted_media, &events)
                .await
                .unwrap()
        );

        let saved = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved.url, converted.url);
        assert_eq!(saved.number, book.number);
        assert!(saved.file_hash.is_empty() && saved.file_hash_koreader.is_empty());
        let saved_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_media.media_type.as_deref(), Some(ZIP_MEDIA_TYPE));
        assert_eq!(saved_media.pages[0].file_hash, "page-hash");
        let algorithm: Option<String> =
            sqlx::query_scalar("SELECT FILE_HASH_ALGORITHM FROM MEDIA_PAGE WHERE BOOK_ID = ?")
                .bind(&book.id)
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert_eq!(algorithm.as_deref(), Some("rgb24-v1"));
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
    async fn conversion_commit_rolls_back_book_media_and_history_together() {
        let (temporary, storage, _library, _series, book, media) = fixture().await;
        let converted = converted_book(&temporary, &book);
        let converted_media = ready_media(&book.id, ZIP_MEDIA_TYPE);
        sqlx::query(
            "CREATE TRIGGER fail_conversion_history BEFORE INSERT ON HISTORICAL_EVENT BEGIN SELECT RAISE(FAIL, 'fail'); END",
        )
        .execute(&storage.pool)
        .await
        .unwrap();

        let result = storage
            .commit_book_conversion_if_current(
                &book,
                &media,
                &converted,
                &converted_media,
                std::slice::from_ref(&HistoricalEvent::book_converted(&converted, &book)),
            )
            .await;
        assert!(result.is_err());
        let saved = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved.url, book.url);
        assert_eq!(saved.file_hash, "source-hash");
        assert_eq!(
            storage
                .find_media_by_book_id(&book.id)
                .await
                .unwrap()
                .unwrap()
                .media_type
                .as_deref(),
            Some(RAR4_MEDIA_TYPE)
        );
    }

    #[tokio::test]
    async fn extension_repair_updates_oneshot_series_and_clears_file_hashes() {
        let (temporary, storage, library, series, book, _media) = fixture().await;
        sqlx::query("UPDATE BOOK SET ONESHOT = 1 WHERE ID = ?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE SERIES SET ONESHOT = 1, URL = ? WHERE ID = ?")
            .bind(book.url.as_str())
            .bind(&series.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        let book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let repaired = converted_book(&temporary, &book);

        assert!(
            storage
                .commit_book_extension_repair_if_current(&book, &repaired)
                .await
                .unwrap()
        );
        let saved = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let saved_series = storage.find_series_by_id(&series.id).await.unwrap().unwrap();
        assert_eq!(saved.url, repaired.url);
        assert_eq!(saved.number, book.number);
        assert!(saved.file_hash.is_empty() && saved.file_hash_koreader.is_empty());
        assert_eq!(saved_series.url, repaired.url);

        sqlx::query("UPDATE MEDIA SET MEDIA_TYPE = ? WHERE BOOK_ID = ?")
            .bind(ZIP_MEDIA_TYPE)
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        assert!(
            storage
                .list_books_with_mismatched_extensions(&library.id)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
