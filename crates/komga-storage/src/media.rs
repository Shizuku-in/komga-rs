use chrono::NaiveDateTime;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use komga_core::{
    BookPage, Dimension, EPUB_EXTENSION_CLASS, Media, MediaExtension, MediaExtensionEpub, MediaFile, MediaStatus,
    ThumbnailBook,
};
use sqlx::FromRow;

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

    /// Replaces the generated thumbnail while preserving a selected custom cover.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction fails.
    pub async fn replace_generated_thumbnail(&self, thumbnail: &ThumbnailBook) -> Result<()> {
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
        Ok(())
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
}
