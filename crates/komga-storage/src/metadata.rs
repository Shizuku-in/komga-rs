//! Transactional metadata persistence and series book-metadata aggregation.

use std::collections::{BTreeSet, HashMap};

use chrono::{NaiveDate, NaiveDateTime};
use komga_core::{
    AlternateTitle, Author, BookMetadata, BookMetadataAggregation, ReadingDirection, SeriesMetadata, SeriesStatus,
    WebLink, aggregate_book_metadata,
};
use sqlx::{FromRow, SqliteConnection};

use crate::{Result, Storage, StorageError};

#[derive(Debug, FromRow)]
struct BookMetadataRow {
    book_id: String,
    title: String,
    title_lock: bool,
    summary: String,
    summary_lock: bool,
    number: String,
    number_lock: bool,
    number_sort: f64,
    number_sort_lock: bool,
    release_date: Option<NaiveDate>,
    release_date_lock: bool,
    authors_lock: bool,
    tags_lock: bool,
    isbn: String,
    isbn_lock: bool,
    links_lock: bool,
    created_date: NaiveDateTime,
    last_modified_date: NaiveDateTime,
}

#[derive(Debug, FromRow)]
struct SeriesMetadataRow {
    series_id: String,
    status: String,
    status_lock: bool,
    title: String,
    title_lock: bool,
    title_sort: String,
    title_sort_lock: bool,
    summary: String,
    summary_lock: bool,
    reading_direction: Option<String>,
    reading_direction_lock: bool,
    publisher: String,
    publisher_lock: bool,
    age_rating: Option<i32>,
    age_rating_lock: bool,
    language: String,
    language_lock: bool,
    genres_lock: bool,
    tags_lock: bool,
    total_book_count: Option<i32>,
    total_book_count_lock: bool,
    sharing_labels_lock: bool,
    links_lock: bool,
    alternate_titles_lock: bool,
    created_date: NaiveDateTime,
    last_modified_date: NaiveDateTime,
}

#[derive(Debug, FromRow)]
struct AggregationRow {
    series_id: String,
    release_date: Option<NaiveDate>,
    summary: String,
    summary_number: String,
    created_date: NaiveDateTime,
    last_modified_date: NaiveDateTime,
}

#[derive(Debug, FromRow)]
struct BookAuthorRow {
    book_id: String,
    name: String,
    role: String,
}

#[derive(Debug, FromRow)]
struct BookTagRow {
    book_id: String,
    tag: String,
}

#[derive(Debug, FromRow)]
struct BookLinkRow {
    book_id: String,
    label: String,
    url: String,
}

const BOOK_METADATA_SELECT: &str = r#"
    SELECT BOOK_ID AS book_id, TITLE AS title, TITLE_LOCK AS title_lock,
           SUMMARY AS summary, SUMMARY_LOCK AS summary_lock,
           NUMBER AS number, NUMBER_LOCK AS number_lock,
           NUMBER_SORT AS number_sort, NUMBER_SORT_LOCK AS number_sort_lock,
           RELEASE_DATE AS release_date, RELEASE_DATE_LOCK AS release_date_lock,
           AUTHORS_LOCK AS authors_lock, TAGS_LOCK AS tags_lock,
           ISBN AS isbn, ISBN_LOCK AS isbn_lock, LINKS_LOCK AS links_lock,
           CREATED_DATE AS created_date, LAST_MODIFIED_DATE AS last_modified_date
      FROM BOOK_METADATA"#;

const SERIES_METADATA_SELECT: &str = r#"
    SELECT SERIES_ID AS series_id, STATUS AS status, STATUS_LOCK AS status_lock,
           TITLE AS title, TITLE_LOCK AS title_lock,
           TITLE_SORT AS title_sort, TITLE_SORT_LOCK AS title_sort_lock,
           SUMMARY AS summary, SUMMARY_LOCK AS summary_lock,
           READING_DIRECTION AS reading_direction,
           READING_DIRECTION_LOCK AS reading_direction_lock,
           PUBLISHER AS publisher, PUBLISHER_LOCK AS publisher_lock,
           AGE_RATING AS age_rating, AGE_RATING_LOCK AS age_rating_lock,
           LANGUAGE AS language, LANGUAGE_LOCK AS language_lock,
           GENRES_LOCK AS genres_lock, TAGS_LOCK AS tags_lock,
           TOTAL_BOOK_COUNT AS total_book_count,
           TOTAL_BOOK_COUNT_LOCK AS total_book_count_lock,
           SHARING_LABELS_LOCK AS sharing_labels_lock,
           LINKS_LOCK AS links_lock, ALTERNATE_TITLES_LOCK AS alternate_titles_lock,
           CREATED_DATE AS created_date, LAST_MODIFIED_DATE AS last_modified_date
      FROM SERIES_METADATA"#;

impl Storage {
    /// Loads complete editable metadata for one book.
    ///
    /// # Errors
    ///
    /// Returns an error when the metadata or any relation cannot be queried.
    pub async fn find_book_metadata(&self, book_id: &str) -> Result<Option<BookMetadata>> {
        let mut tx = self.pool.begin().await?;
        let metadata = load_book_metadata(&mut tx, book_id).await?;
        tx.commit().await?;
        Ok(metadata)
    }

    /// Atomically replaces one book's metadata and relation rows.
    ///
    /// Returns `false` when no metadata exists for the supplied book.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction cannot be completed.
    pub async fn save_book_metadata(&self, metadata: &BookMetadata) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            r#"UPDATE BOOK_METADATA SET
                   TITLE = ?, TITLE_LOCK = ?, SUMMARY = ?, SUMMARY_LOCK = ?,
                   NUMBER = ?, NUMBER_LOCK = ?, NUMBER_SORT = ?, NUMBER_SORT_LOCK = ?,
                   RELEASE_DATE = ?, RELEASE_DATE_LOCK = ?, AUTHORS_LOCK = ?, TAGS_LOCK = ?,
                   ISBN = ?, ISBN_LOCK = ?, LINKS_LOCK = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE BOOK_ID = ?"#,
        )
        .bind(&metadata.title)
        .bind(metadata.title_lock)
        .bind(&metadata.summary)
        .bind(metadata.summary_lock)
        .bind(&metadata.number)
        .bind(metadata.number_lock)
        .bind(metadata.number_sort)
        .bind(metadata.number_sort_lock)
        .bind(metadata.release_date)
        .bind(metadata.release_date_lock)
        .bind(metadata.authors_lock)
        .bind(metadata.tags_lock)
        .bind(&metadata.isbn)
        .bind(metadata.isbn_lock)
        .bind(metadata.links_lock)
        .bind(&metadata.book_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        replace_book_relations(&mut tx, metadata).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Loads complete editable metadata for one series.
    ///
    /// # Errors
    ///
    /// Returns an error when an enum value or relation is invalid or cannot be queried.
    pub async fn find_series_metadata(&self, series_id: &str) -> Result<Option<SeriesMetadata>> {
        let mut tx = self.pool.begin().await?;
        let metadata = load_series_metadata(&mut tx, series_id).await?;
        tx.commit().await?;
        Ok(metadata)
    }

    /// Atomically replaces one series' metadata and relation rows.
    ///
    /// Returns `false` when no metadata exists for the supplied series.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction cannot be completed.
    pub async fn save_series_metadata(&self, metadata: &SeriesMetadata) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            r#"UPDATE SERIES_METADATA SET
                   STATUS = ?, STATUS_LOCK = ?, TITLE = ?, TITLE_LOCK = ?,
                   TITLE_SORT = ?, TITLE_SORT_LOCK = ?, SUMMARY = ?, SUMMARY_LOCK = ?,
                   READING_DIRECTION = ?, READING_DIRECTION_LOCK = ?,
                   PUBLISHER = ?, PUBLISHER_LOCK = ?, AGE_RATING = ?, AGE_RATING_LOCK = ?,
                   LANGUAGE = ?, LANGUAGE_LOCK = ?, GENRES_LOCK = ?, TAGS_LOCK = ?,
                   TOTAL_BOOK_COUNT = ?, TOTAL_BOOK_COUNT_LOCK = ?,
                   SHARING_LABELS_LOCK = ?, LINKS_LOCK = ?, ALTERNATE_TITLES_LOCK = ?,
                   LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
                 WHERE SERIES_ID = ?"#,
        )
        .bind(metadata.status.as_str())
        .bind(metadata.status_lock)
        .bind(&metadata.title)
        .bind(metadata.title_lock)
        .bind(&metadata.title_sort)
        .bind(metadata.title_sort_lock)
        .bind(&metadata.summary)
        .bind(metadata.summary_lock)
        .bind(metadata.reading_direction.map(ReadingDirection::as_str))
        .bind(metadata.reading_direction_lock)
        .bind(&metadata.publisher)
        .bind(metadata.publisher_lock)
        .bind(metadata.age_rating)
        .bind(metadata.age_rating_lock)
        .bind(&metadata.language)
        .bind(metadata.language_lock)
        .bind(metadata.genres_lock)
        .bind(metadata.tags_lock)
        .bind(metadata.total_book_count)
        .bind(metadata.total_book_count_lock)
        .bind(metadata.sharing_labels_lock)
        .bind(metadata.links_lock)
        .bind(metadata.alternate_titles_lock)
        .bind(&metadata.series_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        replace_series_relations(&mut tx, metadata).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Loads the persisted aggregation for one series.
    ///
    /// # Errors
    ///
    /// Returns a database error when the aggregation cannot be queried.
    pub async fn find_book_metadata_aggregation(&self, series_id: &str) -> Result<Option<BookMetadataAggregation>> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query_as::<_, AggregationRow>(
            r#"SELECT SERIES_ID AS series_id, RELEASE_DATE AS release_date,
                      SUMMARY AS summary, SUMMARY_NUMBER AS summary_number,
                      CREATED_DATE AS created_date, LAST_MODIFIED_DATE AS last_modified_date
                 FROM BOOK_METADATA_AGGREGATION WHERE SERIES_ID = ?"#,
        )
        .bind(series_id)
        .fetch_optional(&mut *tx)
        .await?;
        let aggregation = if let Some(row) = row {
            let authors = sqlx::query_as::<_, (String, String)>(
                "SELECT NAME, ROLE FROM BOOK_METADATA_AGGREGATION_AUTHOR WHERE SERIES_ID = ? ORDER BY rowid",
            )
            .bind(series_id)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|(name, role)| Author { name, role })
            .collect();
            let tags = sqlx::query_scalar::<_, String>(
                "SELECT TAG FROM BOOK_METADATA_AGGREGATION_TAG WHERE SERIES_ID = ? ORDER BY TAG",
            )
            .bind(series_id)
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect();
            Some(row.into_domain(authors, tags))
        } else {
            None
        };
        tx.commit().await?;
        Ok(aggregation)
    }

    /// Rebuilds one series' book metadata aggregation in a single transaction.
    ///
    /// Returns `false` when the series no longer exists.
    ///
    /// # Errors
    ///
    /// Returns an error when source metadata is invalid or the transaction fails.
    pub async fn rebuild_book_metadata_aggregation(&self, series_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        if !rebuild_book_metadata_aggregation_in(&mut tx, series_id).await? {
            tx.rollback().await?;
            return Ok(false);
        }
        tx.commit().await?;
        Ok(true)
    }
}

pub(crate) async fn rebuild_book_metadata_aggregation_in(
    connection: &mut SqliteConnection,
    series_id: &str,
) -> Result<bool> {
    let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM SERIES WHERE ID = ?)")
        .bind(series_id)
        .fetch_one(&mut *connection)
        .await?;
    if !exists {
        return Ok(false);
    }
    let metadatas = load_series_book_metadata(connection, series_id).await?;
    let aggregation = aggregate_book_metadata(series_id, &metadatas);
    sqlx::query(
        r#"INSERT INTO BOOK_METADATA_AGGREGATION
             (SERIES_ID, RELEASE_DATE, SUMMARY, SUMMARY_NUMBER)
           VALUES (?, ?, ?, ?)
           ON CONFLICT(SERIES_ID) DO UPDATE SET
             RELEASE_DATE = excluded.RELEASE_DATE,
             SUMMARY = excluded.SUMMARY,
             SUMMARY_NUMBER = excluded.SUMMARY_NUMBER,
             LAST_MODIFIED_DATE = CURRENT_TIMESTAMP"#,
    )
    .bind(series_id)
    .bind(aggregation.release_date)
    .bind(&aggregation.summary)
    .bind(&aggregation.summary_number)
    .execute(&mut *connection)
    .await?;
    sqlx::query("DELETE FROM BOOK_METADATA_AGGREGATION_AUTHOR WHERE SERIES_ID = ?")
        .bind(series_id)
        .execute(&mut *connection)
        .await?;
    sqlx::query("DELETE FROM BOOK_METADATA_AGGREGATION_TAG WHERE SERIES_ID = ?")
        .bind(series_id)
        .execute(&mut *connection)
        .await?;
    for author in &aggregation.authors {
        sqlx::query("INSERT INTO BOOK_METADATA_AGGREGATION_AUTHOR (SERIES_ID, NAME, ROLE) VALUES (?, ?, ?)")
            .bind(series_id)
            .bind(&author.name)
            .bind(&author.role)
            .execute(&mut *connection)
            .await?;
    }
    for tag in &aggregation.tags {
        sqlx::query("INSERT INTO BOOK_METADATA_AGGREGATION_TAG (SERIES_ID, TAG) VALUES (?, ?)")
            .bind(series_id)
            .bind(tag)
            .execute(&mut *connection)
            .await?;
    }
    Ok(true)
}

async fn load_book_metadata(connection: &mut SqliteConnection, book_id: &str) -> Result<Option<BookMetadata>> {
    let row = sqlx::query_as::<_, BookMetadataRow>(&format!("{BOOK_METADATA_SELECT} WHERE BOOK_ID = ?"))
        .bind(book_id)
        .fetch_optional(&mut *connection)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let authors = sqlx::query_as::<_, (String, String)>(
        "SELECT NAME, ROLE FROM BOOK_METADATA_AUTHOR WHERE BOOK_ID = ? ORDER BY rowid",
    )
    .bind(book_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(name, role)| Author { name, role })
    .collect();
    let tags = sqlx::query_scalar::<_, String>("SELECT TAG FROM BOOK_METADATA_TAG WHERE BOOK_ID = ? ORDER BY TAG")
        .bind(book_id)
        .fetch_all(&mut *connection)
        .await?
        .into_iter()
        .collect();
    let links = sqlx::query_as::<_, (String, String)>(
        "SELECT LABEL, URL FROM BOOK_METADATA_LINK WHERE BOOK_ID = ? ORDER BY rowid",
    )
    .bind(book_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(label, url)| WebLink { label, url })
    .collect();
    Ok(Some(row.into_domain(authors, tags, links)))
}

async fn load_series_metadata(connection: &mut SqliteConnection, series_id: &str) -> Result<Option<SeriesMetadata>> {
    let row = sqlx::query_as::<_, SeriesMetadataRow>(&format!("{SERIES_METADATA_SELECT} WHERE SERIES_ID = ?"))
        .bind(series_id)
        .fetch_optional(&mut *connection)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let genres = string_set(connection, "SERIES_METADATA_GENRE", "GENRE", series_id).await?;
    let tags = string_set(connection, "SERIES_METADATA_TAG", "TAG", series_id).await?;
    let sharing_labels = string_set(connection, "SERIES_METADATA_SHARING", "LABEL", series_id).await?;
    let links = sqlx::query_as::<_, (String, String)>(
        "SELECT LABEL, URL FROM SERIES_METADATA_LINK WHERE SERIES_ID = ? ORDER BY rowid",
    )
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(label, url)| WebLink { label, url })
    .collect();
    let alternate_titles = sqlx::query_as::<_, (String, String)>(
        "SELECT LABEL, TITLE FROM SERIES_METADATA_ALTERNATE_TITLE WHERE SERIES_ID = ? ORDER BY rowid",
    )
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .map(|(label, title)| AlternateTitle { label, title })
    .collect();
    Ok(Some(row.into_domain(
        genres,
        tags,
        sharing_labels,
        links,
        alternate_titles,
    )?))
}

async fn string_set(
    connection: &mut SqliteConnection,
    table: &str,
    column: &str,
    series_id: &str,
) -> Result<BTreeSet<String>> {
    Ok(sqlx::query_scalar::<_, String>(&format!(
        "SELECT {column} FROM {table} WHERE SERIES_ID = ? ORDER BY {column}"
    ))
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .collect())
}

async fn replace_book_relations(connection: &mut SqliteConnection, metadata: &BookMetadata) -> Result<()> {
    for table in ["BOOK_METADATA_AUTHOR", "BOOK_METADATA_TAG", "BOOK_METADATA_LINK"] {
        sqlx::query(&format!("DELETE FROM {table} WHERE BOOK_ID = ?"))
            .bind(&metadata.book_id)
            .execute(&mut *connection)
            .await?;
    }
    for author in &metadata.authors {
        sqlx::query("INSERT INTO BOOK_METADATA_AUTHOR (BOOK_ID, NAME, ROLE) VALUES (?, ?, ?)")
            .bind(&metadata.book_id)
            .bind(&author.name)
            .bind(&author.role)
            .execute(&mut *connection)
            .await?;
    }
    for tag in &metadata.tags {
        sqlx::query("INSERT INTO BOOK_METADATA_TAG (BOOK_ID, TAG) VALUES (?, ?)")
            .bind(&metadata.book_id)
            .bind(tag)
            .execute(&mut *connection)
            .await?;
    }
    for link in &metadata.links {
        sqlx::query("INSERT INTO BOOK_METADATA_LINK (BOOK_ID, LABEL, URL) VALUES (?, ?, ?)")
            .bind(&metadata.book_id)
            .bind(&link.label)
            .bind(&link.url)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

async fn replace_series_relations(connection: &mut SqliteConnection, metadata: &SeriesMetadata) -> Result<()> {
    for table in [
        "SERIES_METADATA_GENRE",
        "SERIES_METADATA_TAG",
        "SERIES_METADATA_SHARING",
        "SERIES_METADATA_LINK",
        "SERIES_METADATA_ALTERNATE_TITLE",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE SERIES_ID = ?"))
            .bind(&metadata.series_id)
            .execute(&mut *connection)
            .await?;
    }
    for (table, column, values) in [
        ("SERIES_METADATA_GENRE", "GENRE", &metadata.genres),
        ("SERIES_METADATA_TAG", "TAG", &metadata.tags),
        ("SERIES_METADATA_SHARING", "LABEL", &metadata.sharing_labels),
    ] {
        for value in values {
            sqlx::query(&format!("INSERT INTO {table} (SERIES_ID, {column}) VALUES (?, ?)"))
                .bind(&metadata.series_id)
                .bind(value)
                .execute(&mut *connection)
                .await?;
        }
    }
    for link in &metadata.links {
        sqlx::query("INSERT INTO SERIES_METADATA_LINK (SERIES_ID, LABEL, URL) VALUES (?, ?, ?)")
            .bind(&metadata.series_id)
            .bind(&link.label)
            .bind(&link.url)
            .execute(&mut *connection)
            .await?;
    }
    for title in &metadata.alternate_titles {
        sqlx::query("INSERT INTO SERIES_METADATA_ALTERNATE_TITLE (SERIES_ID, LABEL, TITLE) VALUES (?, ?, ?)")
            .bind(&metadata.series_id)
            .bind(&title.label)
            .bind(&title.title)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

async fn load_series_book_metadata(connection: &mut SqliteConnection, series_id: &str) -> Result<Vec<BookMetadata>> {
    let rows = sqlx::query_as::<_, BookMetadataRow>(&format!(
        "{BOOK_METADATA_SELECT} WHERE BOOK_ID IN (SELECT ID FROM BOOK WHERE SERIES_ID = ?) ORDER BY NUMBER_SORT, BOOK_ID"
    ))
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?;
    let mut authors = sqlx::query_as::<_, BookAuthorRow>(
        r#"SELECT a.BOOK_ID AS book_id, a.NAME AS name, a.ROLE AS role
             FROM BOOK_METADATA_AUTHOR a JOIN BOOK b ON b.ID = a.BOOK_ID
            WHERE b.SERIES_ID = ? ORDER BY b.NUMBER, a.rowid"#,
    )
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .fold(HashMap::<String, Vec<Author>>::new(), |mut map, row| {
        map.entry(row.book_id).or_default().push(Author {
            name: row.name,
            role: row.role,
        });
        map
    });
    let mut tags = sqlx::query_as::<_, BookTagRow>(
        r#"SELECT t.BOOK_ID AS book_id, t.TAG AS tag
             FROM BOOK_METADATA_TAG t JOIN BOOK b ON b.ID = t.BOOK_ID
            WHERE b.SERIES_ID = ? ORDER BY t.TAG"#,
    )
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .fold(HashMap::<String, BTreeSet<String>>::new(), |mut map, row| {
        map.entry(row.book_id).or_default().insert(row.tag);
        map
    });
    let mut links = sqlx::query_as::<_, BookLinkRow>(
        r#"SELECT l.BOOK_ID AS book_id, l.LABEL AS label, l.URL AS url
             FROM BOOK_METADATA_LINK l JOIN BOOK b ON b.ID = l.BOOK_ID
            WHERE b.SERIES_ID = ? ORDER BY b.NUMBER, l.rowid"#,
    )
    .bind(series_id)
    .fetch_all(&mut *connection)
    .await?
    .into_iter()
    .fold(HashMap::<String, Vec<WebLink>>::new(), |mut map, row| {
        map.entry(row.book_id).or_default().push(WebLink {
            label: row.label,
            url: row.url,
        });
        map
    });
    Ok(rows
        .into_iter()
        .map(|row| {
            let book_id = row.book_id.clone();
            row.into_domain(
                authors.remove(&book_id).unwrap_or_default(),
                tags.remove(&book_id).unwrap_or_default(),
                links.remove(&book_id).unwrap_or_default(),
            )
        })
        .collect())
}

impl BookMetadataRow {
    fn into_domain(self, authors: Vec<Author>, tags: BTreeSet<String>, links: Vec<WebLink>) -> BookMetadata {
        BookMetadata {
            book_id: self.book_id,
            title: self.title,
            title_lock: self.title_lock,
            summary: self.summary,
            summary_lock: self.summary_lock,
            number: self.number,
            number_lock: self.number_lock,
            number_sort: self.number_sort,
            number_sort_lock: self.number_sort_lock,
            release_date: self.release_date,
            release_date_lock: self.release_date_lock,
            authors,
            authors_lock: self.authors_lock,
            tags,
            tags_lock: self.tags_lock,
            isbn: self.isbn,
            isbn_lock: self.isbn_lock,
            links,
            links_lock: self.links_lock,
            created_date: self.created_date,
            last_modified_date: self.last_modified_date,
        }
    }
}

impl SeriesMetadataRow {
    fn into_domain(
        self,
        genres: BTreeSet<String>,
        tags: BTreeSet<String>,
        sharing_labels: BTreeSet<String>,
        links: Vec<WebLink>,
        alternate_titles: Vec<AlternateTitle>,
    ) -> Result<SeriesMetadata> {
        Ok(SeriesMetadata {
            series_id: self.series_id,
            status: SeriesStatus::try_from(self.status.as_str()).map_err(invalid_metadata)?,
            status_lock: self.status_lock,
            title: self.title,
            title_lock: self.title_lock,
            title_sort: self.title_sort,
            title_sort_lock: self.title_sort_lock,
            summary: self.summary,
            summary_lock: self.summary_lock,
            reading_direction: self
                .reading_direction
                .as_deref()
                .map(ReadingDirection::try_from)
                .transpose()
                .map_err(invalid_metadata)?,
            reading_direction_lock: self.reading_direction_lock,
            publisher: self.publisher,
            publisher_lock: self.publisher_lock,
            age_rating: self.age_rating,
            age_rating_lock: self.age_rating_lock,
            language: self.language,
            language_lock: self.language_lock,
            genres,
            genres_lock: self.genres_lock,
            tags,
            tags_lock: self.tags_lock,
            total_book_count: self.total_book_count,
            total_book_count_lock: self.total_book_count_lock,
            sharing_labels,
            sharing_labels_lock: self.sharing_labels_lock,
            links,
            links_lock: self.links_lock,
            alternate_titles,
            alternate_titles_lock: self.alternate_titles_lock,
            created_date: self.created_date,
            last_modified_date: self.last_modified_date,
        })
    }
}

impl AggregationRow {
    fn into_domain(self, authors: Vec<Author>, tags: BTreeSet<String>) -> BookMetadataAggregation {
        BookMetadataAggregation {
            series_id: self.series_id,
            authors,
            tags,
            release_date: self.release_date,
            summary: self.summary,
            summary_number: self.summary_number,
            created_date: Some(self.created_date),
            last_modified_date: Some(self.last_modified_date),
        }
    }
}

fn invalid_metadata(error: komga_core::MetadataError) -> StorageError {
    StorageError::InvalidData(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use komga_core::{
        BookMetadataPatch, Library, MetadataUpdate, ScanInterval, SeriesMetadataPatch, WebLink, scan_library_files,
    };
    use url::Url;

    use super::*;

    async fn fixture(storage: &Storage, temporary: &Path) -> (String, Vec<komga_core::Book>) {
        let root = temporary.join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Book 1.cbz"), [1]).unwrap();
        fs::write(root.join("Series/Book 2.cbz"), [2]).unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let books = storage.list_books_by_series_id(&series.id).await.unwrap();
        (series.id, books)
    }

    #[tokio::test]
    async fn book_and_series_metadata_round_trip_relations_transactionally() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (series_id, books) = fixture(&storage, temporary.path()).await;
        let existing = storage.find_book_metadata(&books[0].id).await.unwrap().unwrap();
        let updated = existing
            .apply_patch(BookMetadataPatch {
                authors: MetadataUpdate::Set(vec![Author {
                    name: "Author".into(),
                    role: "Writer".into(),
                }]),
                tags: MetadataUpdate::Set(["Favorite".into()].into()),
                links: MetadataUpdate::Set(vec![WebLink {
                    label: "Source".into(),
                    url: "https://example.org/book".into(),
                }]),
                ..Default::default()
            })
            .unwrap();
        assert!(storage.save_book_metadata(&updated).await.unwrap());
        let loaded = storage.find_book_metadata(&books[0].id).await.unwrap().unwrap();
        assert_eq!(loaded.authors[0].role, "writer");
        assert_eq!(loaded.tags, ["favorite".into()].into());
        assert_eq!(loaded.links, updated.links);
        assert!(
            !storage
                .save_book_metadata(&BookMetadata {
                    book_id: "missing".into(),
                    ..updated
                })
                .await
                .unwrap()
        );

        let existing = storage.find_series_metadata(&series_id).await.unwrap().unwrap();
        let updated = existing
            .apply_patch(SeriesMetadataPatch {
                genres: MetadataUpdate::Set(["Action".into()].into()),
                sharing_labels: MetadataUpdate::Set(["Teen".into()].into()),
                links: MetadataUpdate::Set(vec![WebLink {
                    label: "Catalog".into(),
                    url: "https://example.org/series".into(),
                }]),
                alternate_titles: MetadataUpdate::Set(vec![AlternateTitle {
                    label: "fr".into(),
                    title: "La Serie".into(),
                }]),
                ..Default::default()
            })
            .unwrap();
        assert!(storage.save_series_metadata(&updated).await.unwrap());
        let loaded = storage.find_series_metadata(&series_id).await.unwrap().unwrap();
        assert_eq!(loaded.genres, ["action".into()].into());
        assert_eq!(loaded.sharing_labels, ["teen".into()].into());
        assert_eq!(loaded.links, updated.links);
        assert_eq!(loaded.alternate_titles, updated.alternate_titles);
    }

    #[tokio::test]
    async fn aggregation_rebuilds_from_all_series_book_metadata() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let (series_id, books) = fixture(&storage, temporary.path()).await;
        for (index, book) in books.iter().enumerate() {
            let existing = storage.find_book_metadata(&book.id).await.unwrap().unwrap();
            let updated = existing
                .apply_patch(BookMetadataPatch {
                    summary: if index == 0 {
                        MetadataUpdate::Set("First summary".into())
                    } else {
                        MetadataUpdate::Keep
                    },
                    release_date: MetadataUpdate::Set(
                        NaiveDate::from_ymd_opt(2020 + i32::try_from(index).unwrap(), 1, 1).unwrap(),
                    ),
                    authors: MetadataUpdate::Set(vec![Author {
                        name: "Shared".into(),
                        role: "writer".into(),
                    }]),
                    tags: MetadataUpdate::Set([format!("tag-{index}")].into()),
                    ..Default::default()
                })
                .unwrap();
            storage.save_book_metadata(&updated).await.unwrap();
        }
        assert!(storage.rebuild_book_metadata_aggregation(&series_id).await.unwrap());
        let aggregation = storage
            .find_book_metadata_aggregation(&series_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(aggregation.authors.len(), 1);
        assert_eq!(aggregation.tags, ["tag-0".into(), "tag-1".into()].into());
        assert_eq!(aggregation.release_date, NaiveDate::from_ymd_opt(2020, 1, 1));
        assert_eq!(aggregation.summary, "First summary");
        assert_eq!(aggregation.summary_number, "1");
        assert!(!storage.rebuild_book_metadata_aggregation("missing").await.unwrap());
    }
}
