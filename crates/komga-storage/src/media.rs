use chrono::NaiveDateTime;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use komga_core::{
    BookPage, Dimension, EPUB_EXTENSION_CLASS, HashedPageTarget, HistoricalEvent, Media, MediaExtension,
    MediaExtensionEpub, MediaFile, MediaStatus, SeriesCover, ThumbnailBook, ThumbnailSeries,
};
use sqlx::{FromRow, Sqlite, Transaction};

use crate::{Result, Storage, StorageError};

#[derive(Debug, FromRow)]
struct MediaRow {
    media_type: Option<String>,
    status: String,
    created_date: NaiveDateTime,
    last_modified_date: NaiveDateTime,
    comment: Option<String>,
    book_id: String,
    page_count: i32,
    extension_class: Option<String>,
    extension_value_blob: Option<Vec<u8>>,
    epub_divina_compatible: bool,
    epub_is_kepub: bool,
}

#[derive(Debug, FromRow)]
struct PageRow {
    file_name: String,
    media_type: String,
    width: Option<i32>,
    height: Option<i32>,
    file_hash: String,
    file_size: Option<i64>,
}

#[derive(Debug, FromRow)]
struct PageStateRow {
    number: i32,
    file_name: String,
    media_type: String,
    file_hash: String,
    file_hash_algorithm: Option<String>,
    file_size: Option<i64>,
}

/// One persisted edge page that requires a new or upgraded hash.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct PageHashTarget {
    /// Zero-based page number stored by Komga.
    pub number: i32,
    /// Exact entry name used to reject stale writes.
    pub file_name: String,
    /// Detected page media type.
    pub media_type: String,
}

/// One computed page hash ready for conditional persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHashUpdate {
    /// Zero-based page number stored by Komga.
    pub number: i32,
    /// Exact entry name from the analyzed-media snapshot.
    pub file_name: String,
    /// Detected page media type from the analyzed-media snapshot.
    pub media_type: String,
    /// Seeded XXH3-128 digest.
    pub file_hash: String,
    /// Stable identifier for the bytes supplied to XXH3-128.
    pub algorithm: String,
}

#[derive(Debug, FromRow)]
struct FileRow {
    file_name: String,
    media_type: Option<String>,
    sub_type: Option<String>,
    file_size: Option<i64>,
}

#[derive(Debug, FromRow)]
struct ThumbnailRow {
    id: String,
    thumbnail: Option<Vec<u8>>,
    url: Option<String>,
    selected: bool,
    thumbnail_type: String,
    media_type: String,
    file_size: i64,
    width: i32,
    height: i32,
    book_id: String,
}

#[derive(Debug, FromRow)]
struct SeriesThumbnailRow {
    id: String,
    thumbnail: Option<Vec<u8>>,
    url: Option<String>,
    selected: bool,
    thumbnail_type: String,
    media_type: String,
    file_size: i64,
    width: i32,
    height: i32,
    series_id: String,
}

impl Storage {
    /// Loads analyzed media together with naturally ordered pages and files.
    ///
    /// `MEDIA_FILE` has no explicit ordinal in Kotlin's schema. Its insertion
    /// order is significant for EPUB because analysis writes spine resources
    /// before assets, so rows are read by SQLite `rowid` rather than filename.
    ///
    /// # Errors
    ///
    /// Returns an error when rows cannot be queried or contain an invalid status.
    pub async fn find_media_by_book_id(&self, book_id: &str) -> Result<Option<Media>> {
        let row = sqlx::query_as::<_, MediaRow>(
            r#"SELECT MEDIA_TYPE AS media_type, STATUS AS status,
                      CREATED_DATE AS created_date, LAST_MODIFIED_DATE AS last_modified_date,
                      COMMENT AS comment, BOOK_ID AS book_id, PAGE_COUNT AS page_count,
                      EXTENSION_CLASS AS extension_class, EXTENSION_VALUE_BLOB AS extension_value_blob,
                      EPUB_DIVINA_COMPATIBLE AS epub_divina_compatible,
                      EPUB_IS_KEPUB AS epub_is_kepub
                 FROM MEDIA WHERE BOOK_ID = ?"#,
        )
        .bind(book_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let pages = sqlx::query_as::<_, PageRow>(
            r#"SELECT FILE_NAME AS file_name, MEDIA_TYPE AS media_type,
                      WIDTH AS width, HEIGHT AS height, FILE_HASH AS file_hash,
                      FILE_SIZE AS file_size
                 FROM MEDIA_PAGE WHERE BOOK_ID = ? ORDER BY NUMBER"#,
        )
        .bind(book_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(PageRow::into_domain)
        .collect();
        // Kotlin reads these rows in insertion order; rowid preserves the EPUB spine.
        let files = sqlx::query_as::<_, FileRow>(
            r#"SELECT FILE_NAME AS file_name, MEDIA_TYPE AS media_type,
                      SUB_TYPE AS sub_type, FILE_SIZE AS file_size
                 FROM MEDIA_FILE WHERE BOOK_ID = ? ORDER BY rowid"#,
        )
        .bind(book_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(FileRow::into_domain)
        .collect();
        Ok(Some(row.into_domain(pages, files)?))
    }

    /// Lists books requiring analysis in Kotlin's series/number scheduling order.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn list_book_analysis_targets(&self, library_id: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as::<_, (String, String)>(
            r#"SELECT B.ID, B.SERIES_ID
                 FROM BOOK B
                 JOIN MEDIA M ON M.BOOK_ID = B.ID
                WHERE B.LIBRARY_ID = ? AND B.DELETED_DATE IS NULL
                  AND M.STATUS IN ('UNKNOWN', 'OUTDATED')
                ORDER BY B.SERIES_ID, B.NUMBER"#,
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Lists active ready ZIP books whose configured edge pages require hashes.
    ///
    /// Existing non-JPEG hashes are compatible with Kotlin and remain valid.
    /// JPEG hashes are valid only when their algorithm marker matches
    /// `jpeg_algorithm`.
    ///
    /// # Errors
    ///
    /// Returns a database error when candidates cannot be queried.
    pub async fn list_books_with_incomplete_page_hashes(
        &self,
        library_id: &str,
        edge_size: usize,
        jpeg_algorithm: &str,
    ) -> Result<Vec<String>> {
        let edge_size = i64::try_from(edge_size).unwrap_or(i64::MAX);
        Ok(sqlx::query_scalar(
            r#"SELECT DISTINCT B.ID
                 FROM BOOK B
                 JOIN MEDIA M ON M.BOOK_ID = B.ID
                 JOIN MEDIA_PAGE P ON P.BOOK_ID = B.ID
                WHERE B.LIBRARY_ID = ? AND B.DELETED_DATE IS NULL
                  AND M.STATUS = 'READY' AND M.MEDIA_TYPE = 'application/zip'
                  AND (P.NUMBER < ? OR P.NUMBER >= MAX(M.PAGE_COUNT - ?, 0))
                  AND (P.FILE_HASH = '' OR
                       (P.MEDIA_TYPE = 'image/jpeg' AND
                        COALESCE(P.FILE_HASH_ALGORITHM, '') <> ?))
                ORDER BY B.ID"#,
        )
        .bind(library_id)
        .bind(edge_size)
        .bind(edge_size)
        .bind(jpeg_algorithm)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Lists edge pages that currently need hashing for one ready ZIP book.
    ///
    /// # Errors
    ///
    /// Returns a database error when targets cannot be queried.
    pub async fn list_page_hash_targets(
        &self,
        book_id: &str,
        edge_size: usize,
        jpeg_algorithm: &str,
    ) -> Result<Vec<PageHashTarget>> {
        let edge_size = i64::try_from(edge_size).unwrap_or(i64::MAX);
        Ok(sqlx::query_as(
            r#"SELECT P.NUMBER AS number, P.FILE_NAME AS file_name, P.MEDIA_TYPE AS media_type
                 FROM MEDIA M
                 JOIN MEDIA_PAGE P ON P.BOOK_ID = M.BOOK_ID
                WHERE M.BOOK_ID = ? AND M.STATUS = 'READY' AND M.MEDIA_TYPE = 'application/zip'
                  AND (P.NUMBER < ? OR P.NUMBER >= MAX(M.PAGE_COUNT - ?, 0))
                  AND (P.FILE_HASH = '' OR
                       (P.MEDIA_TYPE = 'image/jpeg' AND
                        COALESCE(P.FILE_HASH_ALGORITHM, '') <> ?))
                ORDER BY P.NUMBER"#,
        )
        .bind(book_id)
        .bind(edge_size)
        .bind(edge_size)
        .bind(jpeg_algorithm)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Stores selected page hashes only while the source and media snapshots
    /// used to compute them are still current.
    ///
    /// Returns `false` when the book, media, or any target page changed. An
    /// empty update collection succeeds without writing.
    ///
    /// # Errors
    ///
    /// Returns a database error when snapshot validation or persistence fails.
    pub async fn update_page_hashes_if_current(
        &self,
        book: &komga_core::Book,
        media: &Media,
        updates: &[PageHashUpdate],
    ) -> Result<bool> {
        if updates.is_empty() {
            return Ok(true);
        }
        let Some(media_type) = media.media_type.as_deref() else {
            return Ok(false);
        };
        let Some(media_last_modified) = media.last_modified_date else {
            return Ok(false);
        };
        let mut tx = self.pool.begin().await?;
        let current = sqlx::query_scalar::<_, bool>(
            r#"SELECT EXISTS(
                 SELECT 1 FROM BOOK B JOIN MEDIA M ON M.BOOK_ID = B.ID
                  WHERE B.ID = ? AND B.URL = ? AND B.FILE_SIZE = ?
                    AND B.FILE_LAST_MODIFIED = ? AND B.DELETED_DATE IS NULL
                    AND M.STATUS = ? AND M.MEDIA_TYPE = ? AND M.PAGE_COUNT = ?
                    AND M.LAST_MODIFIED_DATE = ?)"#,
        )
        .bind(&book.id)
        .bind(book.url.as_str())
        .bind(book.file_size)
        .bind(book.file_last_modified)
        .bind(media.status.as_str())
        .bind(media_type)
        .bind(media.page_count)
        .bind(media_last_modified)
        .fetch_one(&mut *tx)
        .await?;
        if !current {
            tx.rollback().await?;
            return Ok(false);
        }
        for update in updates {
            let result = sqlx::query(
                r#"UPDATE MEDIA_PAGE SET FILE_HASH = ?, FILE_HASH_ALGORITHM = ?
                    WHERE BOOK_ID = ? AND NUMBER = ? AND FILE_NAME = ? AND MEDIA_TYPE = ?"#,
            )
            .bind(&update.file_hash)
            .bind(&update.algorithm)
            .bind(&book.id)
            .bind(update.number)
            .bind(&update.file_name)
            .bind(&update.media_type)
            .execute(&mut *tx)
            .await?;
            if result.rows_affected() != 1 {
                tx.rollback().await?;
                return Ok(false);
            }
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Commits a validated ZIP page removal only while the database still
    /// matches the snapshots used to rewrite the source file.
    ///
    /// Surviving page hashes and algorithm markers are retained by exact
    /// filename, media type, and size. Reading progress is intentionally not
    /// remapped. Known hash delete counters are incremented by the number of
    /// pages actually removed.
    ///
    /// Returns `false` when the book, media, or page snapshots are stale.
    ///
    /// # Errors
    ///
    /// Returns a database error when validation or persistence fails.
    pub async fn apply_zip_page_removal_if_current(
        &self,
        original_book: &komga_core::Book,
        original_media: &Media,
        updated_book: &komga_core::Book,
        updated_media: &Media,
        removed: &[HashedPageTarget],
    ) -> Result<bool> {
        let (Some(original_media_type), Some(original_media_modified)) =
            (original_media.media_type.as_deref(), original_media.last_modified_date)
        else {
            return Ok(false);
        };
        if original_book.id != updated_book.id
            || original_media.book_id != original_book.id
            || updated_media.book_id != original_book.id
            || removed.is_empty()
            || original_media.pages.len() != updated_media.pages.len() + removed.len()
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
        .bind(original_media_type)
        .bind(original_media.page_count)
        .bind(original_media_modified)
        .fetch_one(&mut *tx)
        .await?;
        if !current {
            tx.rollback().await?;
            return Ok(false);
        }
        let states = sqlx::query_as::<_, PageStateRow>(
            r#"SELECT NUMBER AS number, FILE_NAME AS file_name, MEDIA_TYPE AS media_type,
                      FILE_HASH AS file_hash, FILE_HASH_ALGORITHM AS file_hash_algorithm,
                      FILE_SIZE AS file_size
                 FROM MEDIA_PAGE WHERE BOOK_ID = ? ORDER BY NUMBER"#,
        )
        .bind(&original_book.id)
        .fetch_all(&mut *tx)
        .await?;
        if states.len() != original_media.pages.len()
            || states
                .iter()
                .zip(&original_media.pages)
                .enumerate()
                .any(|(number, (state, page))| {
                    usize::try_from(state.number).ok() != Some(number)
                        || state.file_name != page.file_name
                        || state.media_type != page.media_type
                        || state.file_hash != page.file_hash
                        || state.file_size != page.file_size
                })
        {
            tx.rollback().await?;
            return Ok(false);
        }
        for target in removed {
            let Some(index) = target
                .page_number
                .checked_sub(1)
                .and_then(|value| usize::try_from(value).ok())
            else {
                tx.rollback().await?;
                return Ok(false);
            };
            let Some(state) = states.get(index) else {
                tx.rollback().await?;
                return Ok(false);
            };
            if state.file_name != target.file_name
                || state.media_type != target.media_type
                || state.file_hash != target.file_hash
                || state.file_size != Some(target.file_size)
            {
                tx.rollback().await?;
                return Ok(false);
            }
        }

        let book_update = sqlx::query(
            r#"UPDATE BOOK SET FILE_SIZE = ?, FILE_LAST_MODIFIED = ?, FILE_HASH = '',
                               FILE_HASH_KOREADER = '', LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE ID = ? AND URL = ? AND FILE_SIZE = ? AND FILE_LAST_MODIFIED = ?
                   AND DELETED_DATE IS NULL"#,
        )
        .bind(updated_book.file_size)
        .bind(updated_book.file_last_modified)
        .bind(&original_book.id)
        .bind(original_book.url.as_str())
        .bind(original_book.file_size)
        .bind(original_book.file_last_modified)
        .execute(&mut *tx)
        .await?;
        if book_update.rows_affected() != 1 {
            tx.rollback().await?;
            return Ok(false);
        }

        let (extension_class, extension_value_blob) = encode_media_extension(updated_media.extension.as_ref())?;
        sqlx::query(
            r#"UPDATE MEDIA SET STATUS = ?, MEDIA_TYPE = ?, COMMENT = ?, PAGE_COUNT = ?,
                      EPUB_DIVINA_COMPATIBLE = ?, EPUB_IS_KEPUB = ?,
                      EXTENSION_CLASS = ?, EXTENSION_VALUE_BLOB = ?,
                      LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE BOOK_ID = ?"#,
        )
        .bind(updated_media.status.as_str())
        .bind(&updated_media.media_type)
        .bind(&updated_media.comment)
        .bind(updated_media.page_count)
        .bind(updated_media.epub_divina_compatible)
        .bind(updated_media.epub_is_kepub)
        .bind(extension_class)
        .bind(extension_value_blob)
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
        for (number, page) in updated_media.pages.iter().enumerate() {
            let Some(previous) = states.iter().find(|state| {
                state.file_name == page.file_name
                    && state.media_type == page.media_type
                    && state.file_size == page.file_size
                    && state.file_hash == page.file_hash
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
        for file in &updated_media.files {
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
        for target in removed {
            sqlx::query(
                r#"UPDATE PAGE_HASH SET DELETE_COUNT = DELETE_COUNT + 1,
                                      LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                     WHERE HASH = ?"#,
            )
            .bind(&target.file_hash)
            .execute(&mut *tx)
            .await?;
            crate::history::insert_historical_event(
                &mut tx,
                &HistoricalEvent::duplicate_page_deleted(original_book, target),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Atomically replaces media, pages, files, and affected reading progress.
    ///
    /// # Errors
    ///
    /// Returns a database error when any statement in the transaction fails.
    pub async fn replace_media_analysis(&self, media: &Media) -> Result<()> {
        let (extension_class, extension_value_blob) = encode_media_extension(media.extension.as_ref())?;
        let mut tx = self.pool.begin().await?;
        let previous = sqlx::query_as::<_, (String, i32)>("SELECT STATUS, PAGE_COUNT FROM MEDIA WHERE BOOK_ID = ?")
            .bind(&media.book_id)
            .fetch_optional(&mut *tx)
            .await?;
        sqlx::query(
            r#"UPDATE MEDIA SET STATUS = ?, MEDIA_TYPE = ?, COMMENT = ?, PAGE_COUNT = ?,
                      EPUB_DIVINA_COMPATIBLE = ?, EPUB_IS_KEPUB = ?,
                      EXTENSION_CLASS = ?, EXTENSION_VALUE_BLOB = ?,
                      LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE BOOK_ID = ?"#,
        )
        .bind(media.status.as_str())
        .bind(&media.media_type)
        .bind(&media.comment)
        .bind(media.page_count)
        .bind(media.epub_divina_compatible)
        .bind(media.epub_is_kepub)
        .bind(extension_class)
        .bind(extension_value_blob)
        .bind(&media.book_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM MEDIA_PAGE WHERE BOOK_ID = ?")
            .bind(&media.book_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM MEDIA_FILE WHERE BOOK_ID = ?")
            .bind(&media.book_id)
            .execute(&mut *tx)
            .await?;
        for (index, page) in media.pages.iter().enumerate() {
            sqlx::query(
                r#"INSERT INTO MEDIA_PAGE
                     (BOOK_ID, FILE_NAME, MEDIA_TYPE, NUMBER, WIDTH, HEIGHT, FILE_HASH, FILE_SIZE)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&media.book_id)
            .bind(&page.file_name)
            .bind(&page.media_type)
            .bind(i32::try_from(index).unwrap_or(i32::MAX))
            .bind(page.dimension.map(|value| value.width))
            .bind(page.dimension.map(|value| value.height))
            .bind(&page.file_hash)
            .bind(page.file_size)
            .execute(&mut *tx)
            .await?;
        }
        for file in &media.files {
            sqlx::query(
                "INSERT INTO MEDIA_FILE (BOOK_ID, FILE_NAME, MEDIA_TYPE, SUB_TYPE, FILE_SIZE) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&media.book_id)
            .bind(&file.file_name)
            .bind(&file.media_type)
            .bind(&file.sub_type)
            .bind(file.file_size)
            .execute(&mut *tx)
            .await?;
        }
        if previous.is_some_and(|(status, count)| status == "OUTDATED" && count != media.page_count) {
            sqlx::query(
                r#"UPDATE READ_PROGRESS
                      SET PAGE = CASE WHEN COMPLETED = 1 THEN ? ELSE 1 END,
                          LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                    WHERE BOOK_ID = ?"#,
            )
            .bind(media.page_count)
            .bind(&media.book_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Lists every thumbnail belonging to a book.
    pub async fn list_book_thumbnails(&self, book_id: &str) -> Result<Vec<ThumbnailBook>> {
        Ok(sqlx::query_as::<_, ThumbnailRow>(&format!(
            "{} WHERE BOOK_ID = ? ORDER BY CREATED_DATE, ID",
            thumbnail_select("THUMBNAIL_BOOK", "BOOK_ID", "book_id")
        ))
        .bind(book_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(ThumbnailRow::into_domain)
        .collect())
    }

    /// Finds a thumbnail only when it belongs to the requested book.
    pub async fn find_book_thumbnail(&self, book_id: &str, thumbnail_id: &str) -> Result<Option<ThumbnailBook>> {
        Ok(sqlx::query_as::<_, ThumbnailRow>(&format!(
            "{} WHERE BOOK_ID = ? AND ID = ?",
            thumbnail_select("THUMBNAIL_BOOK", "BOOK_ID", "book_id")
        ))
        .bind(book_id)
        .bind(thumbnail_id)
        .fetch_optional(&self.pool)
        .await?
        .map(ThumbnailRow::into_domain))
    }

    /// Finds a book thumbnail by its globally unique identifier.
    pub async fn find_book_thumbnail_by_id(&self, thumbnail_id: &str) -> Result<Option<ThumbnailBook>> {
        Ok(sqlx::query_as::<_, ThumbnailRow>(&format!(
            "{} WHERE ID = ?",
            thumbnail_select("THUMBNAIL_BOOK", "BOOK_ID", "book_id")
        ))
        .bind(thumbnail_id)
        .fetch_optional(&self.pool)
        .await?
        .map(ThumbnailRow::into_domain))
    }

    /// Inserts a user-uploaded book thumbnail and applies selection policy atomically.
    pub async fn insert_book_thumbnail(&self, thumbnail: &ThumbnailBook, selected: bool) -> Result<ThumbnailBook> {
        let mut tx = self.pool.begin().await?;
        insert_thumbnail(&mut tx, thumbnail).await?;
        if selected {
            mark_book_thumbnail_selected(&mut tx, &thumbnail.book_id, &thumbnail.id).await?;
        } else {
            ensure_book_thumbnail_selected(&mut tx, &thumbnail.book_id).await?;
        }
        let is_selected =
            sqlx::query_scalar::<_, bool>("SELECT SELECTED FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND ID = ?")
                .bind(&thumbnail.book_id)
                .bind(&thumbnail.id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        let mut saved = thumbnail.clone();
        saved.selected = is_selected;
        Ok(saved)
    }

    /// Replaces a filesystem-backed sidecar thumbnail with the same URL.
    ///
    /// The first discovered sidecar replaces a generated selection, but never
    /// overrides a user-selected uploaded or sidecar thumbnail.
    ///
    /// # Errors
    ///
    /// Returns a database error when reconciliation cannot be committed.
    pub async fn upsert_book_sidecar_thumbnail(
        &self,
        thumbnail: &ThumbnailBook,
        prefer_selected: bool,
    ) -> Result<ThumbnailBook> {
        let mut tx = self.pool.begin().await?;
        let selected_type = sqlx::query_scalar::<_, String>(
            "SELECT TYPE FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND SELECTED = 1 LIMIT 1",
        )
        .bind(&thumbnail.book_id)
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND TYPE = 'SIDECAR' AND URL = ?")
            .bind(&thumbnail.book_id)
            .bind(&thumbnail.url)
            .execute(&mut *tx)
            .await?;
        let selected = prefer_selected && selected_type.as_deref().is_none_or(|value| value == "GENERATED");
        sqlx::query(
            r#"INSERT INTO THUMBNAIL_BOOK
                 (ID, THUMBNAIL, URL, SELECTED, TYPE, BOOK_ID, WIDTH, HEIGHT, MEDIA_TYPE, FILE_SIZE)
               VALUES (?, ?, ?, ?, 'SIDECAR', ?, ?, ?, ?, ?)"#,
        )
        .bind(&thumbnail.id)
        .bind(&thumbnail.thumbnail)
        .bind(&thumbnail.url)
        .bind(selected)
        .bind(&thumbnail.book_id)
        .bind(thumbnail.dimension.width)
        .bind(thumbnail.dimension.height)
        .bind(&thumbnail.media_type)
        .bind(thumbnail.file_size)
        .execute(&mut *tx)
        .await?;
        if selected {
            sqlx::query("UPDATE THUMBNAIL_BOOK SET SELECTED = (ID = ?) WHERE BOOK_ID = ?")
                .bind(&thumbnail.id)
                .bind(&thumbnail.book_id)
                .execute(&mut *tx)
                .await?;
        } else {
            ensure_book_thumbnail_selected(&mut tx, &thumbnail.book_id).await?;
        }
        tx.commit().await?;
        let mut saved = thumbnail.clone();
        saved.selected = selected;
        Ok(saved)
    }

    /// Selects a thumbnail belonging to a book.
    pub async fn select_book_thumbnail(&self, book_id: &str, thumbnail_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let exists = thumbnail_exists(&mut tx, "THUMBNAIL_BOOK", "BOOK_ID", book_id, thumbnail_id).await?;
        if exists {
            mark_book_thumbnail_selected(&mut tx, book_id, thumbnail_id).await?;
        }
        tx.commit().await?;
        Ok(exists)
    }

    /// Deletes a user-uploaded book thumbnail and restores a selected cover.
    pub async fn delete_book_thumbnail(&self, book_id: &str, thumbnail_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let deleted = sqlx::query("DELETE FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND ID = ? AND TYPE = 'USER_UPLOADED'")
            .bind(book_id)
            .bind(thumbnail_id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        if deleted {
            ensure_book_thumbnail_selected(&mut tx, book_id).await?;
        }
        tx.commit().await?;
        Ok(deleted)
    }

    /// Removes a missing filesystem-backed thumbnail and restores book selection.
    pub async fn remove_stale_book_thumbnail(&self, book_id: &str, thumbnail_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND ID = ? AND URL IS NOT NULL")
            .bind(book_id)
            .bind(thumbnail_id)
            .execute(&mut *tx)
            .await?;
        ensure_book_thumbnail_selected(&mut tx, book_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Lists every thumbnail belonging to a series.
    pub async fn list_series_thumbnails(&self, series_id: &str) -> Result<Vec<ThumbnailSeries>> {
        Ok(sqlx::query_as::<_, SeriesThumbnailRow>(&format!(
            "{} WHERE SERIES_ID = ? ORDER BY CREATED_DATE, ID",
            thumbnail_select("THUMBNAIL_SERIES", "SERIES_ID", "series_id")
        ))
        .bind(series_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(SeriesThumbnailRow::into_domain)
        .collect())
    }

    /// Finds a thumbnail only when it belongs to the requested series.
    pub async fn find_series_thumbnail(&self, series_id: &str, thumbnail_id: &str) -> Result<Option<ThumbnailSeries>> {
        Ok(sqlx::query_as::<_, SeriesThumbnailRow>(&format!(
            "{} WHERE SERIES_ID = ? AND ID = ?",
            thumbnail_select("THUMBNAIL_SERIES", "SERIES_ID", "series_id")
        ))
        .bind(series_id)
        .bind(thumbnail_id)
        .fetch_optional(&self.pool)
        .await?
        .map(SeriesThumbnailRow::into_domain))
    }

    /// Loads the selected series-specific thumbnail.
    pub async fn find_selected_series_thumbnail(&self, series_id: &str) -> Result<Option<ThumbnailSeries>> {
        Ok(sqlx::query_as::<_, SeriesThumbnailRow>(&format!(
            "{} WHERE SERIES_ID = ? AND SELECTED = 1 LIMIT 1",
            thumbnail_select("THUMBNAIL_SERIES", "SERIES_ID", "series_id")
        ))
        .bind(series_id)
        .fetch_optional(&self.pool)
        .await?
        .map(SeriesThumbnailRow::into_domain))
    }

    /// Inserts a user-uploaded series thumbnail and optionally selects it.
    pub async fn insert_series_thumbnail(
        &self,
        thumbnail: &ThumbnailSeries,
        selected: bool,
    ) -> Result<ThumbnailSeries> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO THUMBNAIL_SERIES
                 (ID, THUMBNAIL, URL, SELECTED, TYPE, SERIES_ID, WIDTH, HEIGHT, MEDIA_TYPE, FILE_SIZE)
               VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&thumbnail.id)
        .bind(&thumbnail.thumbnail)
        .bind(&thumbnail.url)
        .bind(&thumbnail.thumbnail_type)
        .bind(&thumbnail.series_id)
        .bind(thumbnail.dimension.width)
        .bind(thumbnail.dimension.height)
        .bind(&thumbnail.media_type)
        .bind(thumbnail.file_size)
        .execute(&mut *tx)
        .await?;
        if selected {
            mark_series_thumbnail_selected(&mut tx, &thumbnail.series_id, &thumbnail.id).await?;
        }
        tx.commit().await?;
        let mut saved = thumbnail.clone();
        saved.selected = selected;
        Ok(saved)
    }

    /// Replaces a filesystem-backed series thumbnail with the same URL.
    ///
    /// The first discovered sidecar is selected only when the series currently
    /// has no selected thumbnail. Existing uploaded and other sidecar selections
    /// are preserved.
    ///
    /// # Errors
    ///
    /// Returns a database error when reconciliation cannot be committed.
    pub async fn upsert_series_sidecar_thumbnail(
        &self,
        thumbnail: &ThumbnailSeries,
        prefer_selected: bool,
    ) -> Result<ThumbnailSeries> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM THUMBNAIL_SERIES WHERE SERIES_ID = ? AND TYPE = 'SIDECAR' AND URL = ?")
            .bind(&thumbnail.series_id)
            .bind(&thumbnail.url)
            .execute(&mut *tx)
            .await?;
        let has_selected = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM THUMBNAIL_SERIES WHERE SERIES_ID = ? AND SELECTED = 1)",
        )
        .bind(&thumbnail.series_id)
        .fetch_one(&mut *tx)
        .await?;
        let selected = prefer_selected && !has_selected;
        sqlx::query(
            r#"INSERT INTO THUMBNAIL_SERIES
                 (ID, THUMBNAIL, URL, SELECTED, TYPE, SERIES_ID, WIDTH, HEIGHT, MEDIA_TYPE, FILE_SIZE)
               VALUES (?, ?, ?, ?, 'SIDECAR', ?, ?, ?, ?, ?)"#,
        )
        .bind(&thumbnail.id)
        .bind(&thumbnail.thumbnail)
        .bind(&thumbnail.url)
        .bind(selected)
        .bind(&thumbnail.series_id)
        .bind(thumbnail.dimension.width)
        .bind(thumbnail.dimension.height)
        .bind(&thumbnail.media_type)
        .bind(thumbnail.file_size)
        .execute(&mut *tx)
        .await?;
        if selected {
            mark_series_thumbnail_selected(&mut tx, &thumbnail.series_id, &thumbnail.id).await?;
        }
        tx.commit().await?;
        let mut saved = thumbnail.clone();
        saved.selected = selected;
        Ok(saved)
    }

    /// Selects a thumbnail belonging to a series.
    pub async fn select_series_thumbnail(&self, series_id: &str, thumbnail_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let exists = thumbnail_exists(&mut tx, "THUMBNAIL_SERIES", "SERIES_ID", series_id, thumbnail_id).await?;
        if exists {
            mark_series_thumbnail_selected(&mut tx, series_id, thumbnail_id).await?;
        }
        tx.commit().await?;
        Ok(exists)
    }

    /// Deletes a user-uploaded series thumbnail.
    pub async fn delete_series_thumbnail(&self, series_id: &str, thumbnail_id: &str) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM THUMBNAIL_SERIES WHERE SERIES_ID = ? AND ID = ? AND TYPE = 'USER_UPLOADED'")
                .bind(series_id)
                .bind(thumbnail_id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    /// Removes a missing filesystem-backed thumbnail and restores series selection.
    pub async fn remove_stale_series_thumbnail(&self, series_id: &str, thumbnail_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM THUMBNAIL_SERIES WHERE SERIES_ID = ? AND ID = ? AND URL IS NOT NULL")
            .bind(series_id)
            .bind(thumbnail_id)
            .execute(&mut *tx)
            .await?;
        ensure_series_thumbnail_selected(&mut tx, series_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Chooses the book supplying a series fallback thumbnail.
    pub async fn find_series_cover_book_id(
        &self,
        series_id: &str,
        user_id: &str,
        strategy: SeriesCover,
    ) -> Result<Option<String>> {
        let unread = matches!(
            strategy,
            SeriesCover::FirstUnreadOrFirst | SeriesCover::FirstUnreadOrLast
        );
        let descending = matches!(strategy, SeriesCover::Last);
        let found = find_cover_book(&self.pool, series_id, user_id, unread, descending).await?;
        if found.is_some() || !unread {
            return Ok(found);
        }
        find_cover_book(
            &self.pool,
            series_id,
            user_id,
            false,
            strategy == SeriesCover::FirstUnreadOrLast,
        )
        .await
    }

    /// Lists books targeted by the global generated-thumbnail refresh.
    pub async fn list_book_thumbnail_regeneration_targets(&self, smaller_than: Option<i32>) -> Result<Vec<String>> {
        if let Some(size) = smaller_than {
            return Ok(sqlx::query_scalar(
                "SELECT BOOK_ID FROM THUMBNAIL_BOOK WHERE TYPE = 'GENERATED' AND WIDTH < ? AND HEIGHT < ?",
            )
            .bind(size)
            .bind(size)
            .fetch_all(&self.pool)
            .await?);
        }
        Ok(sqlx::query_scalar("SELECT ID FROM BOOK WHERE DELETED_DATE IS NULL")
            .fetch_all(&self.pool)
            .await?)
    }

    /// Replaces the generated thumbnail while preserving a selected custom cover.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction fails.
    pub async fn replace_generated_thumbnail(&self, thumbnail: &ThumbnailBook) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let selected_type = sqlx::query_scalar::<_, String>(
            "SELECT TYPE FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND SELECTED = 1 LIMIT 1",
        )
        .bind(&thumbnail.book_id)
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND TYPE = 'GENERATED'")
            .bind(&thumbnail.book_id)
            .execute(&mut *tx)
            .await?;
        let selected = selected_type.as_deref().is_none_or(|value| value == "GENERATED");
        sqlx::query(
            r#"INSERT INTO THUMBNAIL_BOOK
                 (ID, THUMBNAIL, URL, SELECTED, TYPE, BOOK_ID, WIDTH, HEIGHT, MEDIA_TYPE, FILE_SIZE)
               VALUES (?, ?, ?, ?, 'GENERATED', ?, ?, ?, ?, ?)"#,
        )
        .bind(&thumbnail.id)
        .bind(&thumbnail.thumbnail)
        .bind(&thumbnail.url)
        .bind(selected)
        .bind(&thumbnail.book_id)
        .bind(thumbnail.dimension.width)
        .bind(thumbnail.dimension.height)
        .bind(&thumbnail.media_type)
        .bind(thumbnail.file_size)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(selected)
    }

    /// Loads the selected thumbnail for a book.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn find_selected_book_thumbnail(&self, book_id: &str) -> Result<Option<ThumbnailBook>> {
        Ok(sqlx::query_as::<_, ThumbnailRow>(
            r#"SELECT ID AS id, THUMBNAIL AS thumbnail, URL AS url,
                      SELECTED AS selected, TYPE AS thumbnail_type,
                      MEDIA_TYPE AS media_type, FILE_SIZE AS file_size,
                      WIDTH AS width, HEIGHT AS height, BOOK_ID AS book_id
                 FROM THUMBNAIL_BOOK
                WHERE BOOK_ID = ? AND SELECTED = 1 LIMIT 1"#,
        )
        .bind(book_id)
        .fetch_optional(&self.pool)
        .await?
        .map(ThumbnailRow::into_domain))
    }
}

impl MediaRow {
    fn into_domain(self, pages: Vec<BookPage>, files: Vec<MediaFile>) -> Result<Media> {
        let status = MediaStatus::try_from(self.status.as_str()).map_err(StorageError::InvalidData)?;
        Ok(Media {
            status,
            media_type: self.media_type,
            pages,
            page_count: self.page_count,
            files,
            comment: self.comment,
            extension: decode_media_extension(self.extension_class.as_deref(), self.extension_value_blob.as_deref()),
            book_id: self.book_id,
            epub_divina_compatible: self.epub_divina_compatible,
            epub_is_kepub: self.epub_is_kepub,
            created_date: Some(self.created_date),
            last_modified_date: Some(self.last_modified_date),
        })
    }
}

fn encode_media_extension(extension: Option<&MediaExtension>) -> Result<(Option<&'static str>, Option<Vec<u8>>)> {
    let Some(extension) = extension else {
        return Ok((None, None));
    };
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    match extension {
        MediaExtension::Epub(value) => serde_json::to_writer(&mut encoder, value),
    }
    .map_err(|error| StorageError::InvalidData(format!("could not serialize media extension: {error}")))?;
    let bytes = encoder
        .finish()
        .map_err(|error| StorageError::InvalidData(format!("could not compress media extension: {error}")))?;
    Ok((Some(extension.class_name()), Some(bytes)))
}

fn decode_media_extension(extension_class: Option<&str>, bytes: Option<&[u8]>) -> Option<MediaExtension> {
    if extension_class != Some(EPUB_EXTENSION_CLASS) {
        return None;
    }
    let decoder = GzDecoder::new(bytes?);
    serde_json::from_reader::<_, MediaExtensionEpub>(decoder)
        .ok()
        .map(MediaExtension::Epub)
}

impl PageRow {
    fn into_domain(self) -> BookPage {
        BookPage {
            file_name: self.file_name,
            media_type: self.media_type,
            dimension: self
                .width
                .zip(self.height)
                .map(|(width, height)| Dimension { width, height }),
            file_hash: self.file_hash,
            file_size: self.file_size,
        }
    }
}

impl FileRow {
    fn into_domain(self) -> MediaFile {
        MediaFile {
            file_name: self.file_name,
            media_type: self.media_type,
            sub_type: self.sub_type,
            file_size: self.file_size,
        }
    }
}

impl ThumbnailRow {
    fn into_domain(self) -> ThumbnailBook {
        ThumbnailBook {
            id: self.id,
            thumbnail: self.thumbnail,
            url: self.url,
            selected: self.selected,
            thumbnail_type: self.thumbnail_type,
            media_type: self.media_type,
            file_size: self.file_size,
            dimension: Dimension {
                width: self.width,
                height: self.height,
            },
            book_id: self.book_id,
        }
    }
}

impl SeriesThumbnailRow {
    fn into_domain(self) -> ThumbnailSeries {
        ThumbnailSeries {
            id: self.id,
            thumbnail: self.thumbnail,
            url: self.url,
            selected: self.selected,
            thumbnail_type: self.thumbnail_type,
            media_type: self.media_type,
            file_size: self.file_size,
            dimension: Dimension {
                width: self.width,
                height: self.height,
            },
            series_id: self.series_id,
        }
    }
}

fn thumbnail_select(table: &str, parent_column: &str, parent_alias: &str) -> String {
    format!(
        "SELECT ID AS id, THUMBNAIL AS thumbnail, URL AS url, SELECTED AS selected, \
         TYPE AS thumbnail_type, MEDIA_TYPE AS media_type, FILE_SIZE AS file_size, \
         WIDTH AS width, HEIGHT AS height, {parent_column} AS {parent_alias} FROM {table}"
    )
}

async fn insert_thumbnail(tx: &mut Transaction<'_, Sqlite>, thumbnail: &ThumbnailBook) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO THUMBNAIL_BOOK
             (ID, THUMBNAIL, URL, SELECTED, TYPE, BOOK_ID, WIDTH, HEIGHT, MEDIA_TYPE, FILE_SIZE)
           VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&thumbnail.id)
    .bind(&thumbnail.thumbnail)
    .bind(&thumbnail.url)
    .bind(&thumbnail.thumbnail_type)
    .bind(&thumbnail.book_id)
    .bind(thumbnail.dimension.width)
    .bind(thumbnail.dimension.height)
    .bind(&thumbnail.media_type)
    .bind(thumbnail.file_size)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn thumbnail_exists(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    parent_column: &str,
    parent_id: &str,
    thumbnail_id: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS(SELECT 1 FROM {table} WHERE {parent_column} = ? AND ID = ?)"
    ))
    .bind(parent_id)
    .bind(thumbnail_id)
    .fetch_one(&mut **tx)
    .await?)
}

async fn mark_book_thumbnail_selected(
    tx: &mut Transaction<'_, Sqlite>,
    book_id: &str,
    thumbnail_id: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE THUMBNAIL_BOOK SET SELECTED = (ID = ?), LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE BOOK_ID = ?",
    )
    .bind(thumbnail_id)
    .bind(book_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn ensure_book_thumbnail_selected(tx: &mut Transaction<'_, Sqlite>, book_id: &str) -> Result<()> {
    let selected =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? AND SELECTED = 1")
            .bind(book_id)
            .fetch_one(&mut **tx)
            .await?;
    if selected == 0
        && let Some(id) = sqlx::query_scalar::<_, String>(
            "SELECT ID FROM THUMBNAIL_BOOK WHERE BOOK_ID = ? ORDER BY CREATED_DATE, ID LIMIT 1",
        )
        .bind(book_id)
        .fetch_optional(&mut **tx)
        .await?
    {
        mark_book_thumbnail_selected(tx, book_id, &id).await?;
    }
    Ok(())
}

async fn mark_series_thumbnail_selected(
    tx: &mut Transaction<'_, Sqlite>,
    series_id: &str,
    thumbnail_id: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE THUMBNAIL_SERIES SET SELECTED = (ID = ?), LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE SERIES_ID = ?",
    )
    .bind(thumbnail_id)
    .bind(series_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn ensure_series_thumbnail_selected(tx: &mut Transaction<'_, Sqlite>, series_id: &str) -> Result<()> {
    let selected =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM THUMBNAIL_SERIES WHERE SERIES_ID = ? AND SELECTED = 1")
            .bind(series_id)
            .fetch_one(&mut **tx)
            .await?;
    if selected == 0
        && let Some(id) = sqlx::query_scalar::<_, String>(
            "SELECT ID FROM THUMBNAIL_SERIES WHERE SERIES_ID = ? ORDER BY CREATED_DATE, ID LIMIT 1",
        )
        .bind(series_id)
        .fetch_optional(&mut **tx)
        .await?
    {
        mark_series_thumbnail_selected(tx, series_id, &id).await?;
    }
    Ok(())
}

async fn find_cover_book(
    pool: &sqlx::SqlitePool,
    series_id: &str,
    user_id: &str,
    unread: bool,
    descending: bool,
) -> Result<Option<String>> {
    let direction = if descending { "DESC" } else { "ASC" };
    let unread_clause = if unread {
        "AND (rp.COMPLETED IS NULL OR rp.COMPLETED = 0)"
    } else {
        ""
    };
    Ok(sqlx::query_scalar::<_, String>(&format!(
        r#"SELECT b.ID FROM BOOK b
             JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
             LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = ?
            WHERE b.SERIES_ID = ? {unread_clause}
            ORDER BY bm.NUMBER_SORT {direction}, b.ID {direction} LIMIT 1"#
    ))
    .bind(user_id)
    .bind(series_id)
    .fetch_optional(pool)
    .await?)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{
        Book, EPUB_ASSET_SUBTYPE, EPUB_MEDIA_TYPE, EPUB_PAGE_SUBTYPE, EpubTocEntry, Library, R2Location, R2Locator,
        ScanInterval, User, new_id, scan_library_files,
    };
    use url::Url;

    use super::*;

    async fn scanned_book(storage: &Storage, temporary: &std::path::Path) -> Book {
        let root = temporary.join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Volume.cbz"), []).unwrap();
        let mut library = Library::new("Media".into(), Url::from_directory_path(&root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap();
        storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0)
    }

    fn ready_media(book_id: &str) -> Media {
        Media {
            status: MediaStatus::Ready,
            media_type: Some("application/zip".into()),
            pages: vec![
                BookPage {
                    file_name: "1.png".into(),
                    media_type: "image/png".into(),
                    dimension: Some(Dimension { width: 10, height: 20 }),
                    file_hash: String::new(),
                    file_size: Some(100),
                },
                BookPage {
                    file_name: "2.jpg".into(),
                    media_type: "image/jpeg".into(),
                    dimension: None,
                    file_hash: String::new(),
                    file_size: Some(200),
                },
            ],
            page_count: 2,
            files: vec![
                MediaFile {
                    file_name: "z-last.xhtml".into(),
                    media_type: Some("application/xhtml+xml".into()),
                    sub_type: Some(EPUB_PAGE_SUBTYPE.into()),
                    file_size: Some(5),
                },
                MediaFile {
                    file_name: "a-first.xhtml".into(),
                    media_type: Some("application/xhtml+xml".into()),
                    sub_type: Some(EPUB_PAGE_SUBTYPE.into()),
                    file_size: Some(6),
                },
                MediaFile {
                    file_name: "cover.jpg".into(),
                    media_type: Some("image/jpeg".into()),
                    sub_type: Some(EPUB_ASSET_SUBTYPE.into()),
                    file_size: Some(7),
                },
            ],
            comment: None,
            extension: None,
            book_id: book_id.into(),
            epub_divina_compatible: false,
            epub_is_kepub: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    #[tokio::test]
    async fn media_replacement_round_trips_and_adjusts_outdated_progress() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let completed = User::new("completed@example.org".into(), "hash".into());
        let reading = User::new("reading@example.org".into(), "hash".into());
        storage.create_user(&completed).await.unwrap();
        storage.create_user(&reading).await.unwrap();
        for (user, completed, page) in [(&completed, true, 1), (&reading, false, 8)] {
            sqlx::query("INSERT INTO READ_PROGRESS (BOOK_ID, USER_ID, PAGE, COMPLETED) VALUES (?, ?, ?, ?)")
                .bind(&book.id)
                .bind(&user.id)
                .bind(page)
                .bind(completed)
                .execute(&storage.pool)
                .await
                .unwrap();
        }
        sqlx::query("UPDATE MEDIA SET STATUS = 'OUTDATED', PAGE_COUNT = 1 WHERE BOOK_ID = ?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();

        storage.replace_media_analysis(&ready_media(&book.id)).await.unwrap();

        let media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.pages.len(), 2);
        assert_eq!(
            media
                .files
                .iter()
                .map(|file| file.file_name.as_str())
                .collect::<Vec<_>>(),
            ["z-last.xhtml", "a-first.xhtml", "cover.jpg"]
        );
        let progress = sqlx::query_as::<_, (String, i32)>(
            "SELECT USER_ID, PAGE FROM READ_PROGRESS WHERE BOOK_ID = ? ORDER BY USER_ID",
        )
        .bind(&book.id)
        .fetch_all(&storage.pool)
        .await
        .unwrap();
        assert!(progress.contains(&(completed.id, 2)));
        assert!(progress.contains(&(reading.id, 1)));
    }

    #[tokio::test]
    async fn page_hash_targets_cover_only_missing_or_stale_edge_pages() {
        const RGB_ALGORITHM: &str = "RGB24_XXH3_128_V1";

        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let mut media = ready_media(&book.id);
        media.pages = (0..8)
            .map(|number| BookPage {
                file_name: format!("{number}.jpg"),
                media_type: if number == 6 { "image/png" } else { "image/jpeg" }.into(),
                dimension: None,
                file_hash: if matches!(number, 3 | 6) {
                    String::new()
                } else {
                    "legacy".into()
                },
                file_size: Some(100 + i64::from(number)),
            })
            .collect();
        media.page_count = 8;
        storage.replace_media_analysis(&media).await.unwrap();
        sqlx::query("UPDATE MEDIA_PAGE SET FILE_HASH_ALGORITHM = ? WHERE BOOK_ID = ? AND NUMBER = 7")
            .bind(RGB_ALGORITHM)
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();

        assert_eq!(
            storage
                .list_books_with_incomplete_page_hashes(&book.library_id, 2, RGB_ALGORITHM)
                .await
                .unwrap(),
            std::slice::from_ref(&book.id)
        );
        let targets = storage
            .list_page_hash_targets(&book.id, 2, RGB_ALGORITHM)
            .await
            .unwrap();
        assert_eq!(
            targets.iter().map(|target| target.number).collect::<Vec<_>>(),
            [0, 1, 6]
        );

        let current_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let current_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let updates = targets
            .iter()
            .map(|target| PageHashUpdate {
                number: target.number,
                file_name: target.file_name.clone(),
                media_type: target.media_type.clone(),
                file_hash: format!("hash-{}", target.number),
                algorithm: if target.media_type == "image/jpeg" {
                    RGB_ALGORITHM.into()
                } else {
                    "RAW_XXH3_128_V1".into()
                },
            })
            .collect::<Vec<_>>();
        assert!(
            storage
                .update_page_hashes_if_current(&current_book, &current_media, &updates)
                .await
                .unwrap()
        );
        assert!(
            storage
                .list_page_hash_targets(&book.id, 2, RGB_ALGORITHM)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            sqlx::query_as::<_, (String, String)>(
                "SELECT FILE_HASH,FILE_HASH_ALGORITHM FROM MEDIA_PAGE WHERE BOOK_ID=? AND NUMBER=6",
            )
            .bind(&book.id)
            .fetch_one(&storage.pool)
            .await
            .unwrap(),
            ("hash-6".into(), "RAW_XXH3_128_V1".into())
        );

        let mut stale_updates = updates;
        stale_updates[0].file_name = "changed.jpg".into();
        assert!(
            !storage
                .update_page_hashes_if_current(&current_book, &current_media, &stale_updates)
                .await
                .unwrap()
        );
        sqlx::query("UPDATE MEDIA SET STATUS='OUTDATED' WHERE BOOK_ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        assert!(
            storage
                .list_books_with_incomplete_page_hashes(&book.library_id, 2, RGB_ALGORITHM)
                .await
                .unwrap()
                .is_empty()
        );
        sqlx::query("UPDATE MEDIA SET STATUS='READY',MEDIA_TYPE='application/pdf' WHERE BOOK_ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        assert!(
            storage
                .list_books_with_incomplete_page_hashes(&book.library_id, 2, RGB_ALGORITHM)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn zip_page_removal_transaction_preserves_hash_algorithms_and_progress_and_rejects_stale_snapshots() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let mut media = ready_media(&book.id);
        media.pages[0].file_hash = "remove-hash".into();
        media.pages[1].file_hash = "keep-hash".into();
        storage.replace_media_analysis(&media).await.unwrap();
        sqlx::query("UPDATE MEDIA_PAGE SET FILE_HASH_ALGORITHM='RAW_XXH3_128_V1' WHERE BOOK_ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE BOOK SET FILE_HASH='book-hash',FILE_HASH_KOREADER='koreader-hash' WHERE ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        storage
            .create_or_update_page_hash("remove-hash", Some(100), komga_core::PageHashAction::DeleteManual, None)
            .await
            .unwrap();
        let user = User::new("reader@example.org".into(), "hash".into());
        storage.create_user(&user).await.unwrap();
        sqlx::query("INSERT INTO READ_PROGRESS (BOOK_ID,USER_ID,PAGE,COMPLETED) VALUES (?,?,2,0)")
            .bind(&book.id)
            .bind(&user.id)
            .execute(&storage.pool)
            .await
            .unwrap();

        let original_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        let original_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        let mut updated_book = original_book.clone();
        updated_book.file_size += 10;
        updated_book.file_last_modified += chrono::Duration::seconds(1);
        let mut updated_media = original_media.clone();
        updated_media.pages.remove(0);
        updated_media.page_count = 1;
        let removed = HashedPageTarget {
            file_name: original_media.pages[0].file_name.clone(),
            media_type: original_media.pages[0].media_type.clone(),
            file_hash: original_media.pages[0].file_hash.clone(),
            file_size: original_media.pages[0].file_size.unwrap(),
            page_number: 1,
        };
        assert!(
            storage
                .apply_zip_page_removal_if_current(
                    &original_book,
                    &original_media,
                    &updated_book,
                    &updated_media,
                    std::slice::from_ref(&removed),
                )
                .await
                .unwrap()
        );

        let saved_book = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_book.file_size, updated_book.file_size);
        assert!(saved_book.file_hash.is_empty() && saved_book.file_hash_koreader.is_empty());
        let saved_media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved_media.pages.len(), 1);
        assert_eq!(saved_media.pages[0].file_hash, "keep-hash");
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT FILE_HASH_ALGORITHM FROM MEDIA_PAGE WHERE BOOK_ID=? AND NUMBER=0",)
                .bind(&book.id)
                .fetch_one(&storage.pool)
                .await
                .unwrap(),
            "RAW_XXH3_128_V1"
        );
        assert_eq!(
            storage
                .find_read_progress(&book.id, &user.id)
                .await
                .unwrap()
                .unwrap()
                .page,
            2
        );
        assert_eq!(
            storage
                .find_known_page_hash("remove-hash")
                .await
                .unwrap()
                .unwrap()
                .delete_count,
            1
        );
        let history = storage
            .list_historical_events(&komga_core::PageRequest::default())
            .await
            .unwrap();
        assert_eq!(history.total_elements, 1);
        let event = &history.content[0];
        assert_eq!(event.event_type, "DuplicatePageDeleted");
        assert_eq!(event.book_id.as_deref(), Some(book.id.as_str()));
        assert_eq!(event.series_id.as_deref(), Some(book.series_id.as_str()));
        assert_eq!(event.properties["page number"], "1");
        assert_eq!(event.properties["page file name"], removed.file_name);
        assert_eq!(event.properties["page file hash"], "remove-hash");
        assert_eq!(event.properties["page file size"], removed.file_size.to_string());
        assert_eq!(event.properties["page media type"], removed.media_type);
        assert_eq!(
            event.properties["name"],
            original_book.url.to_file_path().unwrap().to_string_lossy()
        );
        assert!(
            !storage
                .apply_zip_page_removal_if_current(
                    &original_book,
                    &original_media,
                    &updated_book,
                    &updated_media,
                    &[removed],
                )
                .await
                .unwrap()
        );
        assert_eq!(
            storage
                .find_known_page_hash("remove-hash")
                .await
                .unwrap()
                .unwrap()
                .delete_count,
            1
        );
        assert_eq!(
            storage
                .list_historical_events(&komga_core::PageRequest::default())
                .await
                .unwrap()
                .total_elements,
            1
        );
    }

    #[tokio::test]
    async fn epub_extension_round_trips_with_kotlin_class_and_gzip_json() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let extension = MediaExtensionEpub {
            toc: vec![EpubTocEntry {
                title: "Chapter 1".into(),
                href: Some("text/chapter.xhtml".into()),
                children: Vec::new(),
            }],
            is_fixed_layout: true,
            positions: vec![R2Locator {
                href: "text/chapter.xhtml".into(),
                media_type: "application/xhtml+xml".into(),
                title: Some("Chapter 1".into()),
                locations: Some(R2Location {
                    progression: Some(0.0),
                    position: Some(1),
                    total_progression: Some(1.0),
                    ..Default::default()
                }),
                text: None,
                kobo_span: Some("kobo.1.1".into()),
            }],
            ..Default::default()
        };
        let mut media = ready_media(&book.id);
        media.media_type = Some(EPUB_MEDIA_TYPE.into());
        media.extension = Some(MediaExtension::Epub(extension.clone()));

        storage.replace_media_analysis(&media).await.unwrap();

        let loaded = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        assert_eq!(loaded.extension, Some(MediaExtension::Epub(extension)));
        let (class_name, blob) = sqlx::query_as::<_, (String, Vec<u8>)>(
            "SELECT EXTENSION_CLASS, EXTENSION_VALUE_BLOB FROM MEDIA WHERE BOOK_ID = ?",
        )
        .bind(&book.id)
        .fetch_one(&storage.pool)
        .await
        .unwrap();
        assert_eq!(class_name, EPUB_EXTENSION_CLASS);
        assert!(blob.starts_with(&[0x1f, 0x8b]));
    }

    #[tokio::test]
    async fn generated_thumbnail_preserves_custom_selection_then_selects_when_none_exists() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        sqlx::query(
            r#"INSERT INTO THUMBNAIL_BOOK
                 (ID, THUMBNAIL, SELECTED, TYPE, BOOK_ID, WIDTH, HEIGHT, MEDIA_TYPE, FILE_SIZE)
               VALUES ('custom', X'01', 1, 'USER_UPLOADED', ?, 1, 1, 'image/jpeg', 1)"#,
        )
        .bind(&book.id)
        .execute(&storage.pool)
        .await
        .unwrap();
        let generated = ThumbnailBook {
            id: new_id(),
            thumbnail: Some(vec![2]),
            url: None,
            selected: false,
            thumbnail_type: "GENERATED".into(),
            media_type: "image/jpeg".into(),
            file_size: 1,
            dimension: Dimension { width: 1, height: 1 },
            book_id: book.id.clone(),
        };

        storage.replace_generated_thumbnail(&generated).await.unwrap();
        assert_eq!(
            storage
                .find_selected_book_thumbnail(&book.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            "custom"
        );

        sqlx::query("UPDATE THUMBNAIL_BOOK SET SELECTED = 0 WHERE BOOK_ID = ?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        let replacement = ThumbnailBook {
            id: new_id(),
            ..generated
        };
        storage.replace_generated_thumbnail(&replacement).await.unwrap();
        assert_eq!(
            storage
                .find_selected_book_thumbnail(&book.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            replacement.id
        );
    }

    #[tokio::test]
    async fn rar_support_migration_only_marks_unsupported_rar_outdated() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let migration = include_str!("../migrations/0006_rar_support.sql");

        for media_type in [
            "application/vnd.rar",
            "application/x-rar-compressed",
            "application/x-rar-compressed; version=4",
            "application/x-rar-compressed; version=5",
        ] {
            sqlx::query("UPDATE MEDIA SET STATUS = 'UNSUPPORTED', MEDIA_TYPE = ? WHERE BOOK_ID = ?")
                .bind(media_type)
                .bind(&book.id)
                .execute(&storage.pool)
                .await
                .unwrap();
            sqlx::query(migration).execute(&storage.pool).await.unwrap();
            let status = sqlx::query_scalar::<_, String>("SELECT STATUS FROM MEDIA WHERE BOOK_ID = ?")
                .bind(&book.id)
                .fetch_one(&storage.pool)
                .await
                .unwrap();
            assert_eq!(status, "OUTDATED", "{media_type}");
        }

        for (status, media_type) in [
            ("UNSUPPORTED", "application/pdf"),
            ("READY", "application/x-rar-compressed"),
        ] {
            sqlx::query("UPDATE MEDIA SET STATUS = ?, MEDIA_TYPE = ? WHERE BOOK_ID = ?")
                .bind(status)
                .bind(media_type)
                .bind(&book.id)
                .execute(&storage.pool)
                .await
                .unwrap();
            sqlx::query(migration).execute(&storage.pool).await.unwrap();
            let actual = sqlx::query_scalar::<_, String>("SELECT STATUS FROM MEDIA WHERE BOOK_ID = ?")
                .bind(&book.id)
                .fetch_one(&storage.pool)
                .await
                .unwrap();
            assert_eq!(actual, status, "{media_type}");
        }
    }

    #[tokio::test]
    async fn pdf_support_migration_only_marks_unsupported_pdf_outdated() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let migration = include_str!("../migrations/0007_pdf_support.sql");

        sqlx::query("UPDATE MEDIA SET STATUS = 'UNSUPPORTED', MEDIA_TYPE = 'application/pdf' WHERE BOOK_ID = ?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query(migration).execute(&storage.pool).await.unwrap();
        let status = sqlx::query_scalar::<_, String>("SELECT STATUS FROM MEDIA WHERE BOOK_ID = ?")
            .bind(&book.id)
            .fetch_one(&storage.pool)
            .await
            .unwrap();
        assert_eq!(status, "OUTDATED");

        for (status, media_type) in [("READY", "application/pdf"), ("UNSUPPORTED", "application/epub+zip")] {
            sqlx::query("UPDATE MEDIA SET STATUS = ?, MEDIA_TYPE = ? WHERE BOOK_ID = ?")
                .bind(status)
                .bind(media_type)
                .bind(&book.id)
                .execute(&storage.pool)
                .await
                .unwrap();
            sqlx::query(migration).execute(&storage.pool).await.unwrap();
            let actual = sqlx::query_scalar::<_, String>("SELECT STATUS FROM MEDIA WHERE BOOK_ID = ?")
                .bind(&book.id)
                .fetch_one(&storage.pool)
                .await
                .unwrap();
            assert_eq!(actual, status, "{media_type}");
        }
    }

    #[tokio::test]
    async fn uploaded_book_and_series_thumbnails_enforce_selection_and_ownership() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let generated = ThumbnailBook {
            id: "generated".into(),
            thumbnail: Some(vec![1]),
            url: None,
            selected: false,
            thumbnail_type: "GENERATED".into(),
            media_type: "image/jpeg".into(),
            file_size: 1,
            dimension: Dimension { width: 10, height: 20 },
            book_id: book.id.clone(),
        };
        storage.replace_generated_thumbnail(&generated).await.unwrap();
        let uploaded = ThumbnailBook {
            id: "uploaded".into(),
            thumbnail: Some(vec![2]),
            thumbnail_type: "USER_UPLOADED".into(),
            ..generated.clone()
        };
        storage.insert_book_thumbnail(&uploaded, true).await.unwrap();
        assert_eq!(
            storage
                .find_selected_book_thumbnail(&book.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            "uploaded"
        );
        assert!(
            storage
                .find_book_thumbnail("other", "uploaded")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!storage.delete_book_thumbnail(&book.id, "generated").await.unwrap());
        assert!(storage.delete_book_thumbnail(&book.id, "uploaded").await.unwrap());
        assert_eq!(
            storage
                .find_selected_book_thumbnail(&book.id)
                .await
                .unwrap()
                .unwrap()
                .id,
            "generated"
        );

        let series = ThumbnailSeries {
            id: "series-upload".into(),
            thumbnail: Some(vec![3]),
            url: None,
            selected: false,
            thumbnail_type: "USER_UPLOADED".into(),
            media_type: "image/png".into(),
            file_size: 1,
            dimension: Dimension { width: 2, height: 3 },
            series_id: book.series_id.clone(),
        };
        storage.insert_series_thumbnail(&series, true).await.unwrap();
        assert_eq!(storage.list_series_thumbnails(&book.series_id).await.unwrap().len(), 1);
        assert!(
            storage
                .find_series_thumbnail("other", "series-upload")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .delete_series_thumbnail(&book.series_id, "series-upload")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn series_sidecar_upsert_replaces_urls_preserves_selection_and_rolls_back() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let book = scanned_book(&storage, temporary.path()).await;
        let sidecar = |id: &str, url: &str| ThumbnailSeries {
            id: id.into(),
            thumbnail: None,
            url: Some(url.into()),
            selected: false,
            thumbnail_type: "SIDECAR".into(),
            media_type: "image/png".into(),
            file_size: 10,
            dimension: Dimension { width: 2, height: 5 },
            series_id: book.series_id.clone(),
        };

        let first = storage
            .upsert_series_sidecar_thumbnail(&sidecar("first", "file:///cover.jpg"), true)
            .await
            .unwrap();
        assert!(first.selected);
        let second = storage
            .upsert_series_sidecar_thumbnail(&sidecar("second", "file:///poster.jpg"), false)
            .await
            .unwrap();
        assert!(!second.selected);
        assert!(
            storage
                .select_series_thumbnail(&book.series_id, "second")
                .await
                .unwrap()
        );

        storage
            .upsert_series_sidecar_thumbnail(&sidecar("first-replaced", "file:///cover.jpg"), true)
            .await
            .unwrap();
        let selected = storage
            .find_selected_series_thumbnail(&book.series_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(selected.id, "second");
        assert!(
            storage
                .find_series_thumbnail(&book.series_id, "first")
                .await
                .unwrap()
                .is_none()
        );

        let uploaded = ThumbnailSeries {
            id: "uploaded".into(),
            thumbnail: Some(vec![1]),
            url: None,
            selected: false,
            thumbnail_type: "USER_UPLOADED".into(),
            media_type: "image/jpeg".into(),
            file_size: 1,
            dimension: Dimension { width: 1, height: 1 },
            series_id: book.series_id.clone(),
        };
        storage.insert_series_thumbnail(&uploaded, true).await.unwrap();
        storage
            .upsert_series_sidecar_thumbnail(&sidecar("third", "file:///folder.jpg"), true)
            .await
            .unwrap();
        assert_eq!(
            storage
                .find_selected_series_thumbnail(&book.series_id)
                .await
                .unwrap()
                .unwrap()
                .id,
            "uploaded"
        );

        sqlx::query(
            "CREATE TRIGGER fail_series_sidecar BEFORE INSERT ON THUMBNAIL_SERIES WHEN NEW.TYPE = 'SIDECAR' BEGIN SELECT RAISE(FAIL, 'forced'); END",
        )
        .execute(storage.pool())
        .await
        .unwrap();
        assert!(
            storage
                .upsert_series_sidecar_thumbnail(&sidecar("replacement-fails", "file:///cover.jpg"), false)
                .await
                .is_err()
        );
        assert!(
            storage
                .find_series_thumbnail(&book.series_id, "first-replaced")
                .await
                .unwrap()
                .is_some()
        );
    }
}
