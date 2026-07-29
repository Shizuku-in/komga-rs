use std::collections::BTreeSet;

use komga_core::{Library, ScanInterval, SeriesCover};
use sqlx::FromRow;
use url::Url;

use crate::{Result, Storage, StorageError};

/// Outcome of a library deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteLibraryResult {
    /// The library and its configuration relations were deleted.
    Deleted {
        /// Permanently removed book identifiers.
        book_ids: Vec<String>,
        /// Permanently removed series identifiers.
        series_ids: Vec<String>,
    },
    /// No library had the supplied identifier.
    NotFound,
}

#[derive(Debug, FromRow)]
struct LibraryRow {
    id: String,
    created_date: String,
    last_modified_date: String,
    name: String,
    root: String,
    import_comic_info_book: bool,
    import_comic_info_series: bool,
    import_comic_info_collection: bool,
    import_comic_info_read_list: bool,
    import_comic_info_series_append_volume: bool,
    import_epub_book: bool,
    import_epub_series: bool,
    import_mylar_series: bool,
    import_local_artwork: bool,
    import_barcode_isbn: bool,
    scan_force_modified_time: bool,
    scan_interval: String,
    scan_on_startup: bool,
    scan_cbx: bool,
    scan_pdf: bool,
    scan_epub: bool,
    scan_mobi: bool,
    repair_extensions: bool,
    convert_to_cbz: bool,
    empty_trash_after_scan: bool,
    series_cover: String,
    hash_files: bool,
    hash_pages: bool,
    hash_koreader: bool,
    analyze_dimensions: bool,
    oneshots_directory: Option<String>,
    unavailable_date: Option<String>,
}

const SELECT_LIBRARY: &str = r#"
    SELECT ID AS id,
           CAST(CREATED_DATE AS TEXT) AS created_date,
           CAST(LAST_MODIFIED_DATE AS TEXT) AS last_modified_date,
           NAME AS name, ROOT AS root,
           IMPORT_COMICINFO_BOOK AS import_comic_info_book,
           IMPORT_COMICINFO_SERIES AS import_comic_info_series,
           IMPORT_COMICINFO_COLLECTION AS import_comic_info_collection,
           IMPORT_COMICINFO_READLIST AS import_comic_info_read_list,
           IMPORT_COMICINFO_SERIES_APPEND_VOLUME AS import_comic_info_series_append_volume,
           IMPORT_EPUB_BOOK AS import_epub_book,
           IMPORT_EPUB_SERIES AS import_epub_series,
           IMPORT_MYLAR_SERIES AS import_mylar_series,
           IMPORT_LOCAL_ARTWORK AS import_local_artwork,
           IMPORT_BARCODE_ISBN AS import_barcode_isbn,
           SCAN_FORCE_MODIFIED_TIME AS scan_force_modified_time,
           SCAN_INTERVAL AS scan_interval,
           SCAN_STARTUP AS scan_on_startup,
           SCAN_CBX AS scan_cbx, SCAN_PDF AS scan_pdf, SCAN_EPUB AS scan_epub,
           SCAN_MOBI AS scan_mobi,
           REPAIR_EXTENSIONS AS repair_extensions,
           CONVERT_TO_CBZ AS convert_to_cbz,
           EMPTY_TRASH_AFTER_SCAN AS empty_trash_after_scan,
           SERIES_COVER AS series_cover,
           HASH_FILES AS hash_files, HASH_PAGES AS hash_pages,
           HASH_KOREADER AS hash_koreader,
           ANALYZE_DIMENSIONS AS analyze_dimensions,
           ONESHOTS_DIRECTORY AS oneshots_directory,
           CAST(UNAVAILABLE_DATE AS TEXT) AS unavailable_date
      FROM LIBRARY
"#;

impl Storage {
    /// Lists all libraries and their directory exclusions.
    ///
    /// # Errors
    ///
    /// Returns an error when rows cannot be queried or contain invalid enums or URLs.
    pub async fn list_libraries(&self) -> Result<Vec<Library>> {
        let rows = sqlx::query_as::<_, LibraryRow>(SELECT_LIBRARY)
            .fetch_all(&self.pool)
            .await?;
        let mut libraries = Vec::with_capacity(rows.len());
        for row in rows {
            libraries.push(self.hydrate_library(row).await?);
        }
        Ok(libraries)
    }

    /// Finds one library by identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the row cannot be queried or converted.
    pub async fn find_library_by_id(&self, id: &str) -> Result<Option<Library>> {
        let row = sqlx::query_as::<_, LibraryRow>(&format!("{SELECT_LIBRARY} WHERE ID = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => Ok(Some(self.hydrate_library(row).await?)),
            None => Ok(None),
        }
    }

    async fn hydrate_library(&self, row: LibraryRow) -> Result<Library> {
        let exclusions = sqlx::query_scalar::<_, String>(
            "SELECT EXCLUSION FROM LIBRARY_EXCLUSIONS WHERE LIBRARY_ID = ? ORDER BY EXCLUSION",
        )
        .bind(&row.id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect::<BTreeSet<_>>();
        row.into_domain(exclusions)
    }

    /// Inserts a library and all directory exclusions atomically.
    ///
    /// # Errors
    ///
    /// Returns a database error when insertion fails.
    pub async fn create_library(&self, library: &Library) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"INSERT INTO LIBRARY (
                ID, NAME, ROOT, IMPORT_COMICINFO_BOOK, IMPORT_COMICINFO_SERIES,
                IMPORT_COMICINFO_COLLECTION, IMPORT_COMICINFO_READLIST,
                IMPORT_COMICINFO_SERIES_APPEND_VOLUME, IMPORT_EPUB_BOOK,
                IMPORT_EPUB_SERIES, IMPORT_MYLAR_SERIES, IMPORT_LOCAL_ARTWORK,
                IMPORT_BARCODE_ISBN, SCAN_FORCE_MODIFIED_TIME, SCAN_INTERVAL,
                SCAN_STARTUP, SCAN_CBX, SCAN_PDF, SCAN_EPUB, SCAN_MOBI, REPAIR_EXTENSIONS,
                CONVERT_TO_CBZ, EMPTY_TRASH_AFTER_SCAN, SERIES_COVER, HASH_FILES,
                HASH_PAGES, HASH_KOREADER, ANALYZE_DIMENSIONS, ONESHOTS_DIRECTORY,
                UNAVAILABLE_DATE
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&library.id)
        .bind(&library.name)
        .bind(library.root.as_str())
        .bind(library.import_comic_info_book)
        .bind(library.import_comic_info_series)
        .bind(library.import_comic_info_collection)
        .bind(library.import_comic_info_read_list)
        .bind(library.import_comic_info_series_append_volume)
        .bind(library.import_epub_book)
        .bind(library.import_epub_series)
        .bind(library.import_mylar_series)
        .bind(library.import_local_artwork)
        .bind(library.import_barcode_isbn)
        .bind(library.scan_force_modified_time)
        .bind(library.scan_interval.as_str())
        .bind(library.scan_on_startup)
        .bind(library.scan_cbx)
        .bind(library.scan_pdf)
        .bind(library.scan_epub)
        .bind(library.scan_mobi)
        .bind(library.repair_extensions)
        .bind(library.convert_to_cbz)
        .bind(library.empty_trash_after_scan)
        .bind(library.series_cover.as_str())
        .bind(library.hash_files)
        .bind(library.hash_pages)
        .bind(library.hash_koreader)
        .bind(library.analyze_dimensions)
        .bind(&library.oneshots_directory)
        .bind(&library.unavailable_date)
        .execute(&mut *tx)
        .await?;
        replace_exclusions(&mut tx, library).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces all mutable library fields and directory exclusions atomically.
    ///
    /// Returns `false` when the library no longer exists.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update fails.
    pub async fn update_library(&self, library: &Library) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            r#"UPDATE LIBRARY SET
                NAME = ?, ROOT = ?, IMPORT_COMICINFO_BOOK = ?, IMPORT_COMICINFO_SERIES = ?,
                IMPORT_COMICINFO_COLLECTION = ?, IMPORT_COMICINFO_READLIST = ?,
                IMPORT_COMICINFO_SERIES_APPEND_VOLUME = ?, IMPORT_EPUB_BOOK = ?,
                IMPORT_EPUB_SERIES = ?, IMPORT_MYLAR_SERIES = ?, IMPORT_LOCAL_ARTWORK = ?,
                IMPORT_BARCODE_ISBN = ?, SCAN_FORCE_MODIFIED_TIME = ?, SCAN_INTERVAL = ?,
                SCAN_STARTUP = ?, SCAN_CBX = ?, SCAN_PDF = ?, SCAN_EPUB = ?, SCAN_MOBI = ?,
                REPAIR_EXTENSIONS = ?, CONVERT_TO_CBZ = ?, EMPTY_TRASH_AFTER_SCAN = ?,
                SERIES_COVER = ?, HASH_FILES = ?, HASH_PAGES = ?, HASH_KOREADER = ?,
                ANALYZE_DIMENSIONS = ?, ONESHOTS_DIRECTORY = ?, UNAVAILABLE_DATE = ?,
                LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
              WHERE ID = ?"#,
        )
        .bind(&library.name)
        .bind(library.root.as_str())
        .bind(library.import_comic_info_book)
        .bind(library.import_comic_info_series)
        .bind(library.import_comic_info_collection)
        .bind(library.import_comic_info_read_list)
        .bind(library.import_comic_info_series_append_volume)
        .bind(library.import_epub_book)
        .bind(library.import_epub_series)
        .bind(library.import_mylar_series)
        .bind(library.import_local_artwork)
        .bind(library.import_barcode_isbn)
        .bind(library.scan_force_modified_time)
        .bind(library.scan_interval.as_str())
        .bind(library.scan_on_startup)
        .bind(library.scan_cbx)
        .bind(library.scan_pdf)
        .bind(library.scan_epub)
        .bind(library.scan_mobi)
        .bind(library.repair_extensions)
        .bind(library.convert_to_cbz)
        .bind(library.empty_trash_after_scan)
        .bind(library.series_cover.as_str())
        .bind(library.hash_files)
        .bind(library.hash_pages)
        .bind(library.hash_koreader)
        .bind(library.analyze_dimensions)
        .bind(&library.oneshots_directory)
        .bind(&library.unavailable_date)
        .bind(&library.id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(false);
        }
        replace_exclusions(&mut tx, library).await?;
        tx.commit().await?;
        Ok(true)
    }
}

async fn replace_exclusions(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, library: &Library) -> Result<()> {
    sqlx::query("DELETE FROM LIBRARY_EXCLUSIONS WHERE LIBRARY_ID = ?")
        .bind(&library.id)
        .execute(&mut **tx)
        .await?;
    for exclusion in &library.scan_directory_exclusions {
        sqlx::query("INSERT INTO LIBRARY_EXCLUSIONS (LIBRARY_ID, EXCLUSION) VALUES (?, ?)")
            .bind(&library.id)
            .bind(exclusion)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

impl LibraryRow {
    fn into_domain(self, scan_directory_exclusions: BTreeSet<String>) -> Result<Library> {
        Ok(Library {
            id: self.id,
            name: self.name,
            root: Url::parse(&self.root)
                .map_err(|error| StorageError::InvalidData(format!("invalid library root: {error}")))?,
            import_comic_info_book: self.import_comic_info_book,
            import_comic_info_series: self.import_comic_info_series,
            import_comic_info_collection: self.import_comic_info_collection,
            import_comic_info_read_list: self.import_comic_info_read_list,
            import_comic_info_series_append_volume: self.import_comic_info_series_append_volume,
            import_epub_book: self.import_epub_book,
            import_epub_series: self.import_epub_series,
            import_mylar_series: self.import_mylar_series,
            import_local_artwork: self.import_local_artwork,
            import_barcode_isbn: self.import_barcode_isbn,
            scan_force_modified_time: self.scan_force_modified_time,
            scan_interval: ScanInterval::try_from(self.scan_interval.as_str())
                .map_err(|_| StorageError::InvalidData(format!("invalid scan interval: {}", self.scan_interval)))?,
            scan_on_startup: self.scan_on_startup,
            scan_cbx: self.scan_cbx,
            scan_pdf: self.scan_pdf,
            scan_epub: self.scan_epub,
            scan_mobi: self.scan_mobi,
            scan_directory_exclusions,
            repair_extensions: self.repair_extensions,
            convert_to_cbz: self.convert_to_cbz,
            empty_trash_after_scan: self.empty_trash_after_scan,
            series_cover: SeriesCover::try_from(self.series_cover.as_str())
                .map_err(|_| StorageError::InvalidData(format!("invalid series cover: {}", self.series_cover)))?,
            hash_files: self.hash_files,
            hash_pages: self.hash_pages,
            hash_koreader: self.hash_koreader,
            analyze_dimensions: self.analyze_dimensions,
            oneshots_directory: self.oneshots_directory,
            unavailable_date: self.unavailable_date.map(crate::sqlite_datetime_to_utc),
            created_date: Some(crate::sqlite_datetime_to_utc(self.created_date)),
            last_modified_date: Some(crate::sqlite_datetime_to_utc(self.last_modified_date)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mobi_scan_setting_defaults_on_and_round_trips() {
        let storage = Storage::connect_memory().await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut library = Library::new("MOBI".to_owned(), Url::from_directory_path(root.path()).unwrap());
        storage.create_library(&library).await.unwrap();

        let loaded = storage.find_library_by_id(&library.id).await.unwrap().unwrap();
        assert!(loaded.scan_mobi);

        library.scan_mobi = false;
        assert!(storage.update_library(&library).await.unwrap());
        let loaded = storage.find_library_by_id(&library.id).await.unwrap().unwrap();
        assert!(!loaded.scan_mobi);

        let declaration = sqlx::query_as::<_, (String, bool)>(
            "SELECT dflt_value, \"notnull\" FROM pragma_table_info('LIBRARY') WHERE upper(name) = 'SCAN_MOBI'",
        )
        .fetch_one(storage.pool())
        .await
        .unwrap();
        assert_eq!(declaration, ("1".to_owned(), true));
    }
}
