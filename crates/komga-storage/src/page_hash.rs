use komga_core::{
    ContentPage, HashedPageTarget, PageHashAction, PageHashKnown, PageHashMatch, PageHashUnknown, PageRequest,
    SortDirection,
};
use sqlx::{FromRow, QueryBuilder, Sqlite};
use url::Url;

use crate::{Result, Storage, StorageError, query_support::validate_page, sqlite_datetime_to_utc};

#[derive(Debug, FromRow)]
struct KnownRow {
    hash: String,
    size: Option<i64>,
    action: String,
    delete_count: i32,
    match_count: i32,
    created: String,
    last_modified: String,
}

#[derive(Debug, FromRow)]
struct UnknownRow {
    hash: String,
    size: Option<i64>,
    match_count: i32,
}

#[derive(Debug, FromRow)]
struct MatchRow {
    book_id: String,
    url: String,
    page_number: i32,
    file_name: String,
    file_size: i64,
    media_type: String,
}

impl Storage {
    /// Lists classified page hashes, optionally restricted by action.
    pub async fn list_known_page_hashes(
        &self,
        actions: &[PageHashAction],
        page: &PageRequest,
    ) -> Result<ContentPage<PageHashKnown>> {
        validate_page(page)?;
        validate_sorts(page, known_sort_column)?;
        let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM PAGE_HASH ph");
        push_action_filter(&mut count, actions);
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;

        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT ph.HASH AS hash, ph.SIZE AS size, ph.ACTION AS action,
                      ph.DELETE_COUNT AS delete_count, COUNT(mp.FILE_HASH) AS match_count,
                      CAST(ph.CREATED_DATE AS TEXT) AS created,
                      CAST(ph.LAST_MODIFIED_DATE AS TEXT) AS last_modified
                 FROM PAGE_HASH ph
                 LEFT JOIN MEDIA_PAGE mp ON mp.FILE_HASH = ph.HASH"#,
        );
        push_action_filter(&mut query, actions);
        query.push(" GROUP BY ph.HASH, ph.SIZE, ph.ACTION, ph.DELETE_COUNT, ph.CREATED_DATE, ph.LAST_MODIFIED_DATE");
        push_order(&mut query, page, known_sort_column, "ph.HASH");
        push_page(&mut query, page)?;
        let content = query
            .build_query_as::<KnownRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(KnownRow::into_domain)
            .collect::<Result<_>>()?;
        Ok(page_result(content, total, page))
    }

    /// Lists unclassified hashes that currently occur more than once.
    pub async fn list_unknown_page_hashes(&self, page: &PageRequest) -> Result<ContentPage<PageHashUnknown>> {
        validate_page(page)?;
        validate_sorts(page, unknown_sort_column)?;
        let grouped = r#"SELECT mp.FILE_HASH
                           FROM MEDIA_PAGE mp
                          WHERE mp.FILE_HASH <> ''
                            AND NOT EXISTS (SELECT 1 FROM PAGE_HASH ph WHERE ph.HASH = mp.FILE_HASH)
                          GROUP BY mp.FILE_HASH HAVING COUNT(*) > 1"#;
        let total = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM ({grouped})"))
            .fetch_one(&self.pool)
            .await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT mp.FILE_HASH AS hash, MAX(mp.FILE_SIZE) AS size, COUNT(*) AS match_count
                 FROM MEDIA_PAGE mp
                WHERE mp.FILE_HASH <> ''
                  AND NOT EXISTS (SELECT 1 FROM PAGE_HASH ph WHERE ph.HASH = mp.FILE_HASH)
                GROUP BY mp.FILE_HASH HAVING COUNT(*) > 1"#,
        );
        push_order(&mut query, page, unknown_sort_column, "mp.FILE_HASH");
        push_page(&mut query, page)?;
        let content = query
            .build_query_as::<UnknownRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| PageHashUnknown {
                hash: row.hash,
                size: normalize_size(row.size),
                match_count: row.match_count,
            })
            .collect();
        Ok(page_result(content, total, page))
    }

    /// Lists analyzed pages matching one hash.
    pub async fn list_page_hash_matches(
        &self,
        page_hash: &str,
        page: &PageRequest,
    ) -> Result<ContentPage<PageHashMatch>> {
        validate_page(page)?;
        validate_sorts(page, match_sort_column)?;
        let total = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM MEDIA_PAGE WHERE FILE_HASH = ?")
            .bind(page_hash)
            .fetch_one(&self.pool)
            .await?;
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT mp.BOOK_ID AS book_id, b.URL AS url, mp.NUMBER + 1 AS page_number,
                      mp.FILE_NAME AS file_name, COALESCE(mp.FILE_SIZE, 0) AS file_size,
                      CASE WHEN m.MEDIA_TYPE = 'application/pdf' THEN 'image/jpeg'
                           ELSE mp.MEDIA_TYPE END AS media_type
                 FROM MEDIA_PAGE mp JOIN BOOK b ON b.ID = mp.BOOK_ID
                 JOIN MEDIA m ON m.BOOK_ID = mp.BOOK_ID
                WHERE mp.FILE_HASH = "#,
        );
        query.push_bind(page_hash.to_owned());
        push_order(&mut query, page, match_sort_column, "mp.BOOK_ID, mp.NUMBER");
        push_page(&mut query, page)?;
        let content = query
            .build_query_as::<MatchRow>()
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(MatchRow::into_domain)
            .collect::<Result<_>>()?;
        Ok(page_result(content, total, page))
    }

    /// Loads one classified hash.
    pub async fn find_known_page_hash(&self, page_hash: &str) -> Result<Option<PageHashKnown>> {
        sqlx::query_as::<_, KnownRow>(
            r#"SELECT ph.HASH AS hash, ph.SIZE AS size, ph.ACTION AS action,
                      ph.DELETE_COUNT AS delete_count,
                      (SELECT COUNT(*) FROM MEDIA_PAGE mp WHERE mp.FILE_HASH = ph.HASH) AS match_count,
                      CAST(ph.CREATED_DATE AS TEXT) AS created,
                      CAST(ph.LAST_MODIFIED_DATE AS TEXT) AS last_modified
                 FROM PAGE_HASH ph WHERE ph.HASH = ?"#,
        )
        .bind(page_hash)
        .fetch_optional(&self.pool)
        .await?
        .map(KnownRow::into_domain)
        .transpose()
    }

    /// Creates a rule or updates only the action of an existing rule.
    pub async fn create_or_update_page_hash(
        &self,
        page_hash: &str,
        size: Option<i64>,
        action: PageHashAction,
        thumbnail: Option<&[u8]>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let updated =
            sqlx::query("UPDATE PAGE_HASH SET ACTION = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE HASH = ?")
                .bind(action.as_str())
                .bind(page_hash)
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() == 0 {
            sqlx::query("INSERT INTO PAGE_HASH (HASH, SIZE, ACTION) VALUES (?, ?, ?)")
                .bind(page_hash)
                .bind(normalize_size(size))
                .bind(action.as_str())
                .execute(&mut *tx)
                .await?;
            if let Some(thumbnail) = thumbnail {
                sqlx::query("INSERT INTO PAGE_HASH_THUMBNAIL (HASH, THUMBNAIL) VALUES (?, ?)")
                    .bind(page_hash)
                    .bind(thumbnail)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Loads a persisted known-hash JPEG thumbnail.
    pub async fn page_hash_thumbnail(&self, page_hash: &str) -> Result<Option<Vec<u8>>> {
        Ok(
            sqlx::query_scalar("SELECT THUMBNAIL FROM PAGE_HASH_THUMBNAIL WHERE HASH = ?")
                .bind(page_hash)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Lists snapshot-bound pages selected by known actions and optional library.
    pub async fn page_targets_by_actions(
        &self,
        actions: &[PageHashAction],
        library_id: Option<&str>,
    ) -> Result<Vec<(String, HashedPageTarget)>> {
        if actions.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::<Sqlite>::new(
            r#"SELECT mp.BOOK_ID, mp.FILE_NAME,
                      CASE WHEN m.MEDIA_TYPE = 'application/pdf' THEN 'image/jpeg'
                           ELSE mp.MEDIA_TYPE END,
                      mp.FILE_HASH,
                      COALESCE(mp.FILE_SIZE, 0), mp.NUMBER + 1
                 FROM MEDIA_PAGE mp JOIN PAGE_HASH ph ON ph.HASH = mp.FILE_HASH
                 JOIN BOOK b ON b.ID = mp.BOOK_ID
                 JOIN MEDIA m ON m.BOOK_ID = mp.BOOK_ID WHERE ph.ACTION IN ("#,
        );
        let mut separated = query.separated(", ");
        for action in actions {
            separated.push_bind(action.as_str());
        }
        separated.push_unseparated(")");
        if let Some(library_id) = library_id {
            query.push(" AND b.LIBRARY_ID = ").push_bind(library_id.to_owned());
        }
        query.push(" AND b.DELETED_DATE IS NULL ORDER BY mp.BOOK_ID, mp.NUMBER");
        let rows = query
            .build_query_as::<(String, String, String, String, i64, i32)>()
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(book_id, file_name, media_type, file_hash, file_size, page_number)| {
                (
                    book_id,
                    HashedPageTarget {
                        file_name,
                        media_type,
                        file_hash,
                        file_size,
                        page_number,
                    },
                )
            })
            .collect())
    }
}

impl KnownRow {
    fn into_domain(self) -> Result<PageHashKnown> {
        Ok(PageHashKnown {
            hash: self.hash,
            size: normalize_size(self.size),
            action: PageHashAction::try_from(self.action.as_str())
                .map_err(|()| StorageError::InvalidData(format!("invalid page hash action: {}", self.action)))?,
            delete_count: self.delete_count,
            match_count: self.match_count,
            created: sqlite_datetime_to_utc(self.created),
            last_modified: sqlite_datetime_to_utc(self.last_modified),
        })
    }
}

impl MatchRow {
    fn into_domain(self) -> Result<PageHashMatch> {
        let url =
            Url::parse(&self.url).map_err(|error| StorageError::InvalidData(format!("invalid book URL: {error}")))?;
        let path = url
            .to_file_path()
            .map_err(|()| StorageError::InvalidData("page hash match URL is not a file URL".into()))?;
        Ok(PageHashMatch {
            book_id: self.book_id,
            url: path.to_string_lossy().into_owned(),
            page_number: self.page_number,
            file_name: self.file_name,
            file_size: self.file_size,
            media_type: self.media_type,
        })
    }
}

fn normalize_size(size: Option<i64>) -> Option<i64> {
    size.filter(|value| *value >= 0)
}

fn page_result<T>(content: Vec<T>, total: i64, page: &PageRequest) -> ContentPage<T> {
    ContentPage {
        content,
        total_elements: u64::try_from(total).unwrap_or_default(),
        page: if page.unpaged { 0 } else { page.page },
        size: if page.unpaged {
            u64::try_from(total).unwrap_or(20).max(20)
        } else {
            page.size
        },
        sort: page.sort.clone(),
    }
}

fn push_action_filter(query: &mut QueryBuilder<'_, Sqlite>, actions: &[PageHashAction]) {
    if actions.is_empty() {
        return;
    }
    query.push(" WHERE ph.ACTION IN (");
    let mut separated = query.separated(", ");
    for action in actions {
        separated.push_bind(action.as_str());
    }
    separated.push_unseparated(")");
}

fn validate_sorts(page: &PageRequest, column: fn(&str) -> Option<&'static str>) -> Result<()> {
    if let Some(sort) = page.sort.iter().find(|sort| column(&sort.property).is_none()) {
        return Err(StorageError::InvalidQuery(format!(
            "unsupported page-hash sort property: {}",
            sort.property
        )));
    }
    Ok(())
}

fn push_order(
    query: &mut QueryBuilder<'_, Sqlite>,
    page: &PageRequest,
    column: fn(&str) -> Option<&'static str>,
    fallback: &str,
) {
    query.push(" ORDER BY ");
    for (index, sort) in page.sort.iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query.push(column(&sort.property).expect("sorts validated"));
        query.push(match sort.direction {
            SortDirection::Ascending => " ASC",
            SortDirection::Descending => " DESC",
        });
    }
    if !page.sort.is_empty() {
        query.push(", ");
    }
    query.push(fallback).push(" ASC");
}

fn push_page(query: &mut QueryBuilder<'_, Sqlite>, page: &PageRequest) -> Result<()> {
    if !page.unpaged {
        let size = i64::try_from(page.size).map_err(|_| StorageError::InvalidQuery("page size is too large".into()))?;
        let offset = page
            .page
            .checked_mul(page.size)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| StorageError::InvalidQuery("page offset is too large".into()))?;
        query.push(" LIMIT ").push_bind(size).push(" OFFSET ").push_bind(offset);
    }
    Ok(())
}

fn known_sort_column(property: &str) -> Option<&'static str> {
    match property {
        "hash" => Some("ph.HASH"),
        "matchCount" => Some("match_count"),
        "deleteCount" => Some("ph.DELETE_COUNT"),
        "deleteSize" => Some("COALESCE(ph.SIZE, 0) * ph.DELETE_COUNT"),
        "fileSize" => Some("ph.SIZE"),
        "createdDate" => Some("ph.CREATED_DATE"),
        "lastModifiedDate" => Some("ph.LAST_MODIFIED_DATE"),
        _ => None,
    }
}

fn unknown_sort_column(property: &str) -> Option<&'static str> {
    match property {
        "hash" => Some("mp.FILE_HASH"),
        "fileSize" => Some("MAX(mp.FILE_SIZE)"),
        "matchCount" => Some("match_count"),
        "totalSize" => Some("COUNT(*) * COALESCE(MAX(mp.FILE_SIZE), 0)"),
        _ => None,
    }
}

fn match_sort_column(property: &str) -> Option<&'static str> {
    match property {
        "hash" => Some("mp.FILE_HASH"),
        "fileSize" => Some("mp.FILE_SIZE"),
        "url" => Some("b.URL"),
        "bookId" => Some("mp.BOOK_ID"),
        "pageNumber" => Some("mp.NUMBER"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{Library, PageHashAction, PageRequest, SortDirection, SortOrder, scan_library_files};
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn known_unknown_matches_and_updates_follow_kotlin_contract() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/A.cbz"), b"a").unwrap();
        fs::write(root.join("Series/B.cbz"), b"b").unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap();
        let books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
        for book in &books {
            sqlx::query("UPDATE MEDIA SET STATUS='READY', MEDIA_TYPE='application/zip', PAGE_COUNT=2 WHERE BOOK_ID=?")
                .bind(&book.id)
                .execute(&storage.pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO MEDIA_PAGE (BOOK_ID,NUMBER,FILE_NAME,MEDIA_TYPE,FILE_HASH,FILE_SIZE) VALUES (?,0,'page.jpg','image/jpeg','duplicate',10)",
            )
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        }
        sqlx::query("UPDATE MEDIA SET MEDIA_TYPE='application/pdf' WHERE BOOK_ID=?")
            .bind(&books[1].id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE MEDIA_PAGE SET FILE_NAME='1',MEDIA_TYPE='',FILE_SIZE=NULL WHERE BOOK_ID=?")
            .bind(&books[1].id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO MEDIA_PAGE (BOOK_ID,NUMBER,FILE_NAME,MEDIA_TYPE,FILE_HASH,FILE_SIZE) VALUES (?,1,'blank.jpg','image/jpeg','',5)",
        )
        .bind(&books[0].id)
        .execute(&storage.pool)
        .await
        .unwrap();

        let unknown = storage
            .list_unknown_page_hashes(&PageRequest {
                sort: vec![SortOrder {
                    property: "matchCount".into(),
                    direction: SortDirection::Descending,
                }],
                ..PageRequest::default()
            })
            .await
            .unwrap();
        assert_eq!(unknown.total_elements, 1);
        assert_eq!(unknown.content[0].hash, "duplicate");
        assert_eq!(unknown.content[0].match_count, 2);

        let matches = storage
            .list_page_hash_matches("duplicate", &PageRequest::default())
            .await
            .unwrap();
        assert_eq!(matches.content.len(), 2);
        assert_eq!(matches.content[0].page_number, 1);
        assert!(matches.content[0].url.ends_with(".cbz"));
        let pdf_match = matches
            .content
            .iter()
            .find(|matched| matched.book_id == books[1].id)
            .unwrap();
        assert_eq!(pdf_match.file_name, "1");
        assert_eq!(pdf_match.file_size, 0);
        assert_eq!(pdf_match.media_type, komga_core::JPEG_MEDIA_TYPE);

        storage
            .create_or_update_page_hash("duplicate", Some(10), PageHashAction::DeleteAuto, Some(b"jpeg"))
            .await
            .unwrap();
        assert!(
            storage
                .list_unknown_page_hashes(&PageRequest::default())
                .await
                .unwrap()
                .content
                .is_empty()
        );
        assert_eq!(
            storage.page_hash_thumbnail("duplicate").await.unwrap().unwrap(),
            b"jpeg"
        );
        sqlx::query("UPDATE PAGE_HASH SET DELETE_COUNT=7 WHERE HASH='duplicate'")
            .execute(&storage.pool)
            .await
            .unwrap();
        storage
            .create_or_update_page_hash("duplicate", Some(99), PageHashAction::Ignore, None)
            .await
            .unwrap();
        let known = storage.find_known_page_hash("duplicate").await.unwrap().unwrap();
        assert_eq!(known.size, Some(10));
        assert_eq!(known.delete_count, 7);
        assert_eq!(known.match_count, 2);
        assert_eq!(known.action, PageHashAction::Ignore);

        let targets = storage
            .page_targets_by_actions(&[PageHashAction::Ignore], Some(&library.id))
            .await
            .unwrap();
        assert_eq!(targets.len(), 2);
        assert!(targets.iter().all(|(_, page)| page.page_number == 1));
        let pdf_target = targets.iter().find(|(book_id, _)| *book_id == books[1].id).unwrap();
        assert_eq!(pdf_target.1.file_size, 0);
        assert_eq!(pdf_target.1.media_type, komga_core::JPEG_MEDIA_TYPE);

        storage
            .create_or_update_page_hash("negative", Some(-1), PageHashAction::DeleteManual, None)
            .await
            .unwrap();
        assert_eq!(
            storage.find_known_page_hash("negative").await.unwrap().unwrap().size,
            None
        );
    }
}
