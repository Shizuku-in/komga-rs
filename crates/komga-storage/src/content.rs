use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{Local, NaiveDateTime};
use komga_core::{Book, ScanResult, Series};
use sqlx::FromRow;
use url::Url;

use crate::{Result, Storage, StorageError};

/// Counts of content rows changed while applying one filesystem scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanApplyStats {
    /// Book identifiers whose indexed projections changed.
    pub book_ids: BTreeSet<String>,
    /// Series identifiers whose indexed projections changed.
    pub series_ids: BTreeSet<String>,
    /// Newly persisted series.
    pub series_added: u64,
    /// Existing series whose filesystem fields changed.
    pub series_updated: u64,
    /// Series marked as deleted because their directory disappeared.
    pub series_deleted: u64,
    /// Newly persisted books.
    pub books_added: u64,
    /// Existing books whose filesystem fields changed.
    pub books_updated: u64,
    /// Books marked as deleted because their file disappeared.
    pub books_deleted: u64,
    /// Series whose Mylar sidecar was added or modified.
    pub sidecars_changed: Vec<String>,
}

#[derive(Debug, Clone, FromRow)]
struct SeriesRow {
    id: String,
    created_date: NaiveDateTime,
    last_modified_date: NaiveDateTime,
    file_last_modified: NaiveDateTime,
    name: String,
    url: String,
    library_id: String,
    book_count: i32,
    deleted_date: Option<NaiveDateTime>,
    oneshot: bool,
}

#[derive(Debug, Clone, FromRow)]
struct BookRow {
    id: String,
    created_date: NaiveDateTime,
    last_modified_date: NaiveDateTime,
    file_last_modified: NaiveDateTime,
    name: String,
    url: String,
    series_id: String,
    file_size: i64,
    number: i32,
    library_id: String,
    file_hash: String,
    deleted_date: Option<NaiveDateTime>,
    oneshot: bool,
    file_hash_koreader: String,
}

const SELECT_SERIES: &str = r#"
    SELECT ID AS id, CREATED_DATE AS created_date, LAST_MODIFIED_DATE AS last_modified_date,
           FILE_LAST_MODIFIED AS file_last_modified, NAME AS name, URL AS url,
           LIBRARY_ID AS library_id, BOOK_COUNT AS book_count,
           DELETED_DATE AS deleted_date, ONESHOT AS oneshot
      FROM SERIES"#;

const SELECT_BOOK: &str = r#"
    SELECT ID AS id, CREATED_DATE AS created_date, LAST_MODIFIED_DATE AS last_modified_date,
           FILE_LAST_MODIFIED AS file_last_modified, NAME AS name, URL AS url,
           SERIES_ID AS series_id, FILE_SIZE AS file_size, NUMBER AS number,
           LIBRARY_ID AS library_id, FILE_HASH AS file_hash,
           DELETED_DATE AS deleted_date, ONESHOT AS oneshot,
           FILE_HASH_KOREADER AS file_hash_koreader
      FROM BOOK"#;

impl Storage {
    /// Finds one series by identifier, including soft-deleted rows.
    pub async fn find_series_by_id(&self, series_id: &str) -> Result<Option<Series>> {
        sqlx::query_as::<_, SeriesRow>(&format!("{SELECT_SERIES} WHERE ID = ?"))
            .bind(series_id)
            .fetch_optional(&self.pool)
            .await?
            .map(SeriesRow::into_domain)
            .transpose()
    }
    /// Finds one persisted book by identifier, including soft-deleted rows.
    ///
    /// # Errors
    ///
    /// Returns an error when the row contains an invalid URL or cannot be queried.
    pub async fn find_book_by_id(&self, book_id: &str) -> Result<Option<Book>> {
        sqlx::query_as::<_, BookRow>(&format!("{SELECT_BOOK} WHERE ID = ?"))
            .bind(book_id)
            .fetch_optional(&self.pool)
            .await?
            .map(BookRow::into_domain)
            .transpose()
    }

    /// Lists every series in a library, including soft-deleted rows.
    ///
    /// # Errors
    ///
    /// Returns an error when rows contain invalid URLs or cannot be queried.
    pub async fn list_series_by_library_id(&self, library_id: &str) -> Result<Vec<Series>> {
        let rows = sqlx::query_as::<_, SeriesRow>(&format!(
            "{SELECT_SERIES} WHERE LIBRARY_ID = ? ORDER BY URL, LAST_MODIFIED_DATE"
        ))
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(SeriesRow::into_domain).collect()
    }

    /// Lists every book in a series, including soft-deleted rows.
    ///
    /// # Errors
    ///
    /// Returns an error when rows contain invalid URLs or cannot be queried.
    pub async fn list_books_by_series_id(&self, series_id: &str) -> Result<Vec<Book>> {
        let rows = sqlx::query_as::<_, BookRow>(&format!("{SELECT_BOOK} WHERE SERIES_ID = ? ORDER BY NUMBER, URL"))
            .bind(series_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(BookRow::into_domain).collect()
    }

    /// Lists active books with ready media in a library.
    pub async fn list_ready_books_by_library_id(&self, library_id: &str) -> Result<Vec<(String, String)>> {
        Ok(sqlx::query_as(
            "SELECT B.ID, B.SERIES_ID FROM BOOK B JOIN MEDIA M ON M.BOOK_ID=B.ID WHERE B.LIBRARY_ID=? AND B.DELETED_DATE IS NULL AND M.STATUS='READY' ORDER BY B.SERIES_ID,B.NUMBER",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Applies one filesystem snapshot atomically to Kotlin-compatible content tables.
    ///
    /// `deep` forces book reconciliation even when a series directory timestamp
    /// is unchanged. A successful apply also clears the library unavailable flag.
    ///
    /// # Errors
    ///
    /// Returns an error if current rows are invalid or any transaction step fails.
    pub async fn apply_library_scan(&self, library_id: &str, scan: &ScanResult, deep: bool) -> Result<ScanApplyStats> {
        let mut tx = self.pool.begin().await?;
        let existing_series = sqlx::query_as::<_, SeriesRow>(&format!(
            "{SELECT_SERIES} WHERE LIBRARY_ID = ? ORDER BY LAST_MODIFIED_DATE DESC"
        ))
        .bind(library_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(SeriesRow::into_domain)
        .collect::<Result<Vec<_>>>()?;
        let existing_books = sqlx::query_as::<_, BookRow>(&format!(
            "{SELECT_BOOK} WHERE LIBRARY_ID = ? ORDER BY LAST_MODIFIED_DATE DESC"
        ))
        .bind(library_id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(BookRow::into_domain)
        .collect::<Result<Vec<_>>>()?;

        let scanned_series_urls = scan
            .series
            .iter()
            .map(|entry| entry.series.url.as_str())
            .collect::<HashSet<_>>();
        let scanned_book_urls = scan
            .series
            .iter()
            .flat_map(|entry| entry.books.iter())
            .map(|book| book.url.as_str())
            .collect::<HashSet<_>>();
        let now = Local::now().naive_local();
        let mut stats = ScanApplyStats::default();
        let mut series_with_missing_books = HashSet::new();

        for book in existing_books
            .iter()
            .filter(|book| book.deleted_date.is_none() && !scanned_book_urls.contains(book.url.as_str()))
        {
            sqlx::query("UPDATE BOOK SET DELETED_DATE = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?")
                .bind(now)
                .bind(&book.id)
                .execute(&mut *tx)
                .await?;
            series_with_missing_books.insert(book.series_id.clone());
            stats.books_deleted += 1;
            stats.book_ids.insert(book.id.clone());
        }
        for series in existing_series
            .iter()
            .filter(|series| series.deleted_date.is_none() && !scanned_series_urls.contains(series.url.as_str()))
        {
            sqlx::query("UPDATE SERIES SET DELETED_DATE = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?")
                .bind(now)
                .bind(&series.id)
                .execute(&mut *tx)
                .await?;
            stats.series_deleted += 1;
            stats.series_ids.insert(series.id.clone());
        }

        let mut active_series = HashMap::new();
        for series in existing_series.iter().filter(|series| series.deleted_date.is_none()) {
            active_series
                .entry(series.url.as_str().to_owned())
                .or_insert_with(|| series.clone());
        }
        let mut active_books = HashMap::<String, HashMap<String, Book>>::new();
        for book in existing_books.iter().filter(|book| book.deleted_date.is_none()) {
            active_books
                .entry(book.series_id.clone())
                .or_default()
                .entry(book.url.as_str().to_owned())
                .or_insert_with(|| book.clone());
        }

        for scanned in &scan.series {
            let existing = active_series.get(scanned.series.url.as_str());
            let (series_id, reconcile_books) = if let Some(existing) = existing {
                let changed = existing.file_last_modified != scanned.series.file_last_modified
                    || existing.name != scanned.series.name
                    || existing.oneshot != scanned.series.oneshot
                    || existing.book_count != i32::try_from(scanned.books.len()).unwrap_or(i32::MAX)
                    || series_with_missing_books.contains(&existing.id);
                if changed {
                    sqlx::query(
                        r#"UPDATE SERIES SET NAME = ?, URL = ?, FILE_LAST_MODIFIED = ?,
                                  BOOK_COUNT = ?, DELETED_DATE = NULL, ONESHOT = ?,
                                  LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?"#,
                    )
                    .bind(&scanned.series.name)
                    .bind(scanned.series.url.as_str())
                    .bind(scanned.series.file_last_modified)
                    .bind(i32::try_from(scanned.books.len()).unwrap_or(i32::MAX))
                    .bind(scanned.series.oneshot)
                    .bind(&existing.id)
                    .execute(&mut *tx)
                    .await?;
                    stats.series_updated += 1;
                    stats.series_ids.insert(existing.id.clone());
                }
                (existing.id.clone(), deep || changed)
            } else {
                sqlx::query(
                    r#"INSERT INTO SERIES
                         (ID, FILE_LAST_MODIFIED, NAME, URL, LIBRARY_ID, BOOK_COUNT, ONESHOT)
                       VALUES (?, ?, ?, ?, ?, ?, ?)"#,
                )
                .bind(&scanned.series.id)
                .bind(scanned.series.file_last_modified)
                .bind(&scanned.series.name)
                .bind(scanned.series.url.as_str())
                .bind(library_id)
                .bind(i32::try_from(scanned.books.len()).unwrap_or(i32::MAX))
                .bind(scanned.series.oneshot)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO SERIES_METADATA (STATUS, TITLE, TITLE_SORT, SERIES_ID) VALUES ('ONGOING', ?, ?, ?)",
                )
                .bind(&scanned.series.name)
                .bind(&scanned.series.name)
                .bind(&scanned.series.id)
                .execute(&mut *tx)
                .await?;
                sqlx::query("INSERT INTO BOOK_METADATA_AGGREGATION (SERIES_ID) VALUES (?)")
                    .bind(&scanned.series.id)
                    .execute(&mut *tx)
                    .await?;
                stats.series_added += 1;
                stats.series_ids.insert(scanned.series.id.clone());
                (scanned.series.id.clone(), true)
            };

            if !reconcile_books {
                continue;
            }
            let books_for_series = active_books.get(&series_id);
            for scanned_book in &scanned.books {
                if let Some(existing) = books_for_series.and_then(|books| books.get(scanned_book.url.as_str())) {
                    let file_changed = existing.file_last_modified != scanned_book.file_last_modified
                        || existing.file_size != scanned_book.file_size;
                    let changed = file_changed
                        || existing.name != scanned_book.name
                        || existing.number != scanned_book.number
                        || existing.oneshot != scanned_book.oneshot;
                    if changed {
                        sqlx::query(
                            r#"UPDATE BOOK SET NAME = ?, URL = ?, FILE_LAST_MODIFIED = ?,
                                      FILE_SIZE = ?, NUMBER = ?, ONESHOT = ?, DELETED_DATE = NULL,
                                      LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?"#,
                        )
                        .bind(&scanned_book.name)
                        .bind(scanned_book.url.as_str())
                        .bind(scanned_book.file_last_modified)
                        .bind(scanned_book.file_size)
                        .bind(scanned_book.number)
                        .bind(scanned_book.oneshot)
                        .bind(&existing.id)
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query(
                            r#"UPDATE BOOK_METADATA SET
                                  NUMBER = CASE WHEN NUMBER_LOCK = 0 THEN ? ELSE NUMBER END,
                                  NUMBER_SORT = CASE WHEN NUMBER_SORT_LOCK = 0 THEN ? ELSE NUMBER_SORT END,
                                  LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                                WHERE BOOK_ID = ?"#,
                        )
                        .bind(scanned_book.number.to_string())
                        .bind(f64::from(scanned_book.number))
                        .bind(&existing.id)
                        .execute(&mut *tx)
                        .await?;
                        if file_changed {
                            sqlx::query(
                                "UPDATE MEDIA SET STATUS = 'OUTDATED', LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE BOOK_ID = ?",
                            )
                            .bind(&existing.id)
                            .execute(&mut *tx)
                            .await?;
                        }
                        stats.books_updated += 1;
                        stats.book_ids.insert(existing.id.clone());
                    }
                } else {
                    insert_book(&mut tx, scanned_book, &series_id, library_id).await?;
                    stats.books_added += 1;
                    stats.book_ids.insert(scanned_book.id.clone());
                }
            }
        }

        let scanned_sidecar_urls = scan.sidecars.iter().map(|s| s.url.as_str()).collect::<HashSet<_>>();
        sqlx::query("DELETE FROM SIDECAR WHERE LIBRARY_ID = ? AND URL NOT IN (SELECT value FROM json_each(?))")
            .bind(library_id)
            .bind(serde_json::to_string(&scanned_sidecar_urls).unwrap_or_else(|_| "[]".into()))
            .execute(&mut *tx)
            .await?;
        for sidecar in &scan.sidecars {
            let previous =
                sqlx::query_scalar::<_, NaiveDateTime>("SELECT LAST_MODIFIED_TIME FROM SIDECAR WHERE URL = ?")
                    .bind(sidecar.url.as_str())
                    .fetch_optional(&mut *tx)
                    .await?;
            sqlx::query("INSERT INTO SIDECAR (URL,PARENT_URL,LAST_MODIFIED_TIME,LIBRARY_ID) VALUES (?,?,?,?) ON CONFLICT(URL) DO UPDATE SET PARENT_URL=excluded.PARENT_URL,LAST_MODIFIED_TIME=excluded.LAST_MODIFIED_TIME,LIBRARY_ID=excluded.LIBRARY_ID")
                .bind(sidecar.url.as_str()).bind(sidecar.parent_url.as_str()).bind(sidecar.last_modified_time).bind(library_id).execute(&mut *tx).await?;
            if previous != Some(sidecar.last_modified_time)
                && let Some(id) =
                    sqlx::query_scalar::<_, String>("SELECT ID FROM SERIES WHERE URL = ? AND DELETED_DATE IS NULL")
                        .bind(sidecar.parent_url.as_str())
                        .fetch_optional(&mut *tx)
                        .await?
            {
                stats.sidecars_changed.push(id);
            }
        }

        sqlx::query(
            "UPDATE LIBRARY SET UNAVAILABLE_DATE = NULL, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ? AND UNAVAILABLE_DATE IS NOT NULL",
        )
        .bind(library_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(stats)
    }

    /// Sets or clears the timestamp indicating that a library root is unavailable.
    ///
    /// # Errors
    ///
    /// Returns a database error when the library cannot be updated.
    pub async fn set_library_unavailable(&self, library_id: &str, unavailable: bool) -> Result<()> {
        let expression = if unavailable { "CURRENT_TIMESTAMP" } else { "NULL" };
        sqlx::query(&format!(
            "UPDATE LIBRARY SET UNAVAILABLE_DATE = {expression}, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?"
        ))
        .bind(library_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

async fn insert_book(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    book: &Book,
    series_id: &str,
    library_id: &str,
) -> Result<()> {
    sqlx::query(
        r#"INSERT INTO BOOK
             (ID, FILE_LAST_MODIFIED, NAME, URL, SERIES_ID, FILE_SIZE, NUMBER,
              LIBRARY_ID, FILE_HASH, ONESHOT, FILE_HASH_KOREADER)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
    )
    .bind(&book.id)
    .bind(book.file_last_modified)
    .bind(&book.name)
    .bind(book.url.as_str())
    .bind(series_id)
    .bind(book.file_size)
    .bind(book.number)
    .bind(library_id)
    .bind(&book.file_hash)
    .bind(book.oneshot)
    .bind(&book.file_hash_koreader)
    .execute(&mut **tx)
    .await?;
    sqlx::query("INSERT INTO MEDIA (STATUS, BOOK_ID) VALUES ('UNKNOWN', ?)")
        .bind(&book.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO BOOK_METADATA (NUMBER, NUMBER_SORT, TITLE, BOOK_ID) VALUES (?, ?, ?, ?)")
        .bind(book.number.to_string())
        .bind(f64::from(book.number))
        .bind(&book.name)
        .bind(&book.id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

impl SeriesRow {
    fn into_domain(self) -> Result<Series> {
        Ok(Series {
            id: self.id,
            created_date: Some(self.created_date),
            last_modified_date: Some(self.last_modified_date),
            file_last_modified: self.file_last_modified,
            name: self.name,
            url: parse_url(self.url)?,
            library_id: self.library_id,
            book_count: self.book_count,
            deleted_date: self.deleted_date,
            oneshot: self.oneshot,
        })
    }
}

impl BookRow {
    fn into_domain(self) -> Result<Book> {
        Ok(Book {
            id: self.id,
            created_date: Some(self.created_date),
            last_modified_date: Some(self.last_modified_date),
            file_last_modified: self.file_last_modified,
            name: self.name,
            url: parse_url(self.url)?,
            series_id: self.series_id,
            file_size: self.file_size,
            number: self.number,
            library_id: self.library_id,
            file_hash: self.file_hash,
            deleted_date: self.deleted_date,
            oneshot: self.oneshot,
            file_hash_koreader: self.file_hash_koreader,
        })
    }
}

fn parse_url(value: String) -> Result<Url> {
    Url::parse(&value).map_err(|error| StorageError::InvalidData(format!("invalid content URL {value:?}: {error}")))
}
