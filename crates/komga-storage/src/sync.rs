//! Transactional SyncPoint snapshots and incremental page claiming.

use std::{io::Read, num::NonZeroUsize};

use chrono::{NaiveDate, NaiveDateTime, Utc};
use flate2::read::GzDecoder;
use komga_core::{
    EPUB_EXTENSION_CLASS, EPUB_MEDIA_TYPE, MediaExtensionEpub, ON_DECK_READ_LIST_ID, SyncBatch, SyncBookChange,
    SyncBookProjection, SyncChangeKind, SyncPoint, SyncPointBook, SyncPointReadList, SyncReadListChange, User, new_id,
};
use sqlx::{FromRow, QueryBuilder, Sqlite, Transaction};

use crate::{Result, Storage, StorageError, query_support::push_book_visibility, read_progress::ReadProgressRow};

#[derive(Debug, FromRow)]
struct SyncPointRow {
    id: String,
    user_id: String,
    api_key_id: Option<String>,
    created_date: NaiveDateTime,
}

#[derive(Debug, FromRow)]
#[sqlx(rename_all = "SCREAMING_SNAKE_CASE")]
struct SyncPointBookRow {
    sync_point_id: String,
    book_id: String,
    book_created_date: NaiveDateTime,
    book_last_modified_date: NaiveDateTime,
    book_file_last_modified: NaiveDateTime,
    book_file_size: i64,
    book_file_hash: String,
    book_metadata_last_modified_date: NaiveDateTime,
    book_read_progress_last_modified_date: Option<NaiveDateTime>,
    book_thumbnail_id: Option<String>,
    synced: bool,
}

#[derive(Debug, FromRow)]
#[sqlx(rename_all = "SCREAMING_SNAKE_CASE")]
struct SyncPointReadListRow {
    sync_point_id: String,
    readlist_id: String,
    readlist_name: String,
    readlist_created_date: NaiveDateTime,
    readlist_last_modified_date: NaiveDateTime,
    synced: bool,
}

#[derive(Debug, FromRow)]
struct BookProjectionRow {
    book_id: String,
    series_id: String,
    title: String,
    number: String,
    number_sort: f32,
    isbn: String,
    summary: String,
    release_date: Option<NaiveDate>,
    metadata_created_date: NaiveDateTime,
    series_title: String,
    publisher: String,
    language: String,
    oneshot: bool,
    epub_is_kepub: bool,
    extension_class: Option<String>,
    extension_value_blob: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy)]
enum ClaimCategory {
    InitialBooks,
    InitialReadLists,
    BooksAdded,
    BooksChanged,
    BooksRemoved,
    ProgressChanged,
    ReadListsAdded,
    ReadListsChanged,
    ReadListsRemoved,
}

impl Storage {
    /// Loads the live metadata projection used by Kobo resource endpoints.
    ///
    /// Callers must apply user visibility policy before exposing this result.
    pub async fn find_sync_book_projection(&self, book_id: &str) -> Result<Option<SyncBookProjection>> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM BOOK WHERE ID = ?)")
            .bind(book_id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            return Ok(None);
        }
        let projection = load_book_projection(&mut tx, book_id).await?;
        tx.commit().await?;
        Ok(Some(projection))
    }

    /// Captures the current user's visible, ready EPUB library in one transaction.
    ///
    /// The standard file hash is copied as-is, including an empty value when
    /// background file hashing is disabled. The virtual On Deck tag is included
    /// only when its current member set is non-empty.
    pub async fn create_sync_point(&self, user: &User, api_key_id: Option<&str>) -> Result<SyncPoint> {
        let mut tx = self.pool.begin().await?;
        let point = SyncPoint {
            id: new_id(),
            user_id: user.id.clone(),
            api_key_id: api_key_id.map(str::to_owned),
            created_date: Utc::now().naive_utc(),
        };
        sqlx::query("INSERT INTO SYNC_POINT (ID, CREATED_DATE, USER_ID, API_KEY_ID) VALUES (?, ?, ?, ?)")
            .bind(&point.id)
            .bind(point.created_date)
            .bind(&point.user_id)
            .bind(&point.api_key_id)
            .execute(&mut *tx)
            .await?;

        let mut books = QueryBuilder::<Sqlite>::new(
            r#"INSERT INTO SYNC_POINT_BOOK
               (SYNC_POINT_ID, BOOK_ID, BOOK_CREATED_DATE, BOOK_LAST_MODIFIED_DATE,
                BOOK_FILE_LAST_MODIFIED, BOOK_FILE_SIZE, BOOK_FILE_HASH,
                BOOK_METADATA_LAST_MODIFIED_DATE, BOOK_READ_PROGRESS_LAST_MODIFIED_DATE,
                BOOK_THUMBNAIL_ID, SYNCED)
               SELECT "#,
        );
        books
            .push_bind(point.id.clone())
            .push(
                r#", b.ID, b.CREATED_DATE, b.LAST_MODIFIED_DATE, b.FILE_LAST_MODIFIED,
                    b.FILE_SIZE, b.FILE_HASH, bm.LAST_MODIFIED_DATE, rp.LAST_MODIFIED_DATE,
                    bt.ID, 0
                   FROM BOOK b
                   JOIN MEDIA m ON m.BOOK_ID = b.ID
                   JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
                   JOIN SERIES_METADATA sm ON sm.SERIES_ID = b.SERIES_ID
                   LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = "#,
            )
            .push_bind(user.id.clone())
            .push(
                r#" LEFT JOIN THUMBNAIL_BOOK bt ON bt.BOOK_ID = b.ID AND bt.SELECTED = 1
                    WHERE b.DELETED_DATE IS NULL AND m.STATUS = 'READY' AND m.MEDIA_TYPE = "#,
            )
            .push_bind(EPUB_MEDIA_TYPE.to_owned());
        push_book_visibility(&mut books, user);
        books.push(" ORDER BY b.ID");
        books.build().execute(&mut *tx).await?;

        capture_on_deck(&mut tx, &point, user).await?;
        tx.commit().await?;
        Ok(point)
    }

    /// Finds a SyncPoint only when it belongs to the supplied user.
    pub async fn find_sync_point_for_user(&self, sync_point_id: &str, user_id: &str) -> Result<Option<SyncPoint>> {
        let row = sqlx::query_as::<_, SyncPointRow>(
            r#"SELECT ID AS id, USER_ID AS user_id, API_KEY_ID AS api_key_id,
                      CREATED_DATE AS created_date
                 FROM SYNC_POINT WHERE ID = ? AND USER_ID = ?"#,
        )
        .bind(sync_point_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(SyncPointRow::into_domain))
    }

    /// Deletes all snapshots owned by one user, optionally restricted to API key IDs.
    pub async fn delete_sync_points_for_user(&self, user_id: &str, api_key_ids: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let ids = if api_key_ids.is_empty() {
            sqlx::query_scalar::<_, String>("SELECT ID FROM SYNC_POINT WHERE USER_ID = ?")
                .bind(user_id)
                .fetch_all(&mut *tx)
                .await?
        } else {
            let mut query = QueryBuilder::<Sqlite>::new("SELECT ID FROM SYNC_POINT WHERE USER_ID = ");
            query.push_bind(user_id.to_owned()).push(" AND API_KEY_ID IN (");
            let mut separated = query.separated(",");
            for id in api_key_ids {
                separated.push_bind(id.clone());
            }
            query.push(")");
            query.build_query_scalar::<String>().fetch_all(&mut *tx).await?
        };
        delete_sync_points(&mut tx, &ids).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes one snapshot and all child rows.
    pub async fn delete_sync_point(&self, sync_point_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        delete_sync_points(&mut tx, &[sync_point_id.to_owned()]).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Atomically claims and hydrates one page of synchronization source items.
    ///
    /// Categories are visited in Kobo protocol order. The limit counts source
    /// books and tags, so a changed book may later expand into several response
    /// objects without consuming extra capacity.
    pub async fn take_sync_batch(
        &self,
        from_sync_point_id: Option<&str>,
        to_sync_point_id: &str,
        user_id: &str,
        limit: NonZeroUsize,
    ) -> Result<SyncBatch> {
        let mut tx = self.pool.begin().await?;
        let mut remaining = limit.get();
        let mut books = Vec::new();
        let mut read_lists = Vec::new();
        let categories: &[ClaimCategory] = if from_sync_point_id.is_some() {
            &[
                ClaimCategory::BooksAdded,
                ClaimCategory::BooksChanged,
                ClaimCategory::BooksRemoved,
                ClaimCategory::ProgressChanged,
                ClaimCategory::ReadListsAdded,
                ClaimCategory::ReadListsChanged,
                ClaimCategory::ReadListsRemoved,
            ]
        } else {
            &[ClaimCategory::InitialBooks, ClaimCategory::InitialReadLists]
        };

        for category in categories {
            if remaining == 0 {
                break;
            }
            match category {
                ClaimCategory::InitialBooks
                | ClaimCategory::BooksAdded
                | ClaimCategory::BooksChanged
                | ClaimCategory::BooksRemoved
                | ClaimCategory::ProgressChanged => {
                    let rows = claim_books(&mut tx, *category, from_sync_point_id, to_sync_point_id, remaining).await?;
                    remaining -= rows.len();
                    for (kind, row) in rows {
                        let projection = if kind == SyncChangeKind::Removed {
                            None
                        } else {
                            Some(load_book_projection(&mut tx, &row.book_id).await?)
                        };
                        let read_progress = if kind == SyncChangeKind::Removed {
                            None
                        } else {
                            load_read_progress(&mut tx, &row.book_id, user_id).await?
                        };
                        books.push(SyncBookChange {
                            kind,
                            snapshot: row.into_domain(),
                            projection,
                            read_progress,
                        });
                    }
                }
                ClaimCategory::InitialReadLists
                | ClaimCategory::ReadListsAdded
                | ClaimCategory::ReadListsChanged
                | ClaimCategory::ReadListsRemoved => {
                    let rows =
                        claim_read_lists(&mut tx, *category, from_sync_point_id, to_sync_point_id, remaining).await?;
                    remaining -= rows.len();
                    for (kind, row) in rows {
                        let book_ids = if kind == SyncChangeKind::Removed {
                            Vec::new()
                        } else {
                            sqlx::query_scalar::<_, String>(
                                "SELECT BOOK_ID FROM SYNC_POINT_READLIST_BOOK \
                                 WHERE SYNC_POINT_ID = ? AND READLIST_ID = ? ORDER BY BOOK_ID",
                            )
                            .bind(to_sync_point_id)
                            .bind(&row.readlist_id)
                            .fetch_all(&mut *tx)
                            .await?
                        };
                        read_lists.push(SyncReadListChange {
                            kind,
                            snapshot: row.into_domain(),
                            book_ids,
                        });
                    }
                }
            }
        }

        let has_more = has_unclaimed(&mut tx, from_sync_point_id, to_sync_point_id).await?;
        tx.commit().await?;
        Ok(SyncBatch {
            books,
            read_lists,
            has_more,
        })
    }
}

async fn capture_on_deck(tx: &mut Transaction<'_, Sqlite>, point: &SyncPoint, user: &User) -> Result<()> {
    let mut query = QueryBuilder::<Sqlite>::new(
        r#"WITH eligible_series AS MATERIALIZED (
           SELECT s.ID AS series_id, rps.MOST_RECENT_READ_DATE AS most_recent_read_date
             FROM SERIES s
             JOIN SERIES_METADATA sm ON sm.SERIES_ID = s.ID
             JOIN READ_PROGRESS_SERIES rps ON rps.SERIES_ID = s.ID AND rps.USER_ID = "#,
    );
    query
        .push_bind(user.id.clone())
        .push(" WHERE rps.READ_COUNT > 0 AND rps.IN_PROGRESS_COUNT = 0 AND rps.READ_COUNT <> s.BOOK_COUNT");
    crate::query_support::push_series_visibility(&mut query, user);
    query
        .push(
            r#"), unread_books AS MATERIALIZED (
               SELECT b.ID AS book_id, b.SERIES_ID AS series_id, bm.NUMBER_SORT AS number_sort,
                      eligible_series.most_recent_read_date AS most_recent_read_date
                 FROM BOOK b
                 JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
                 JOIN eligible_series ON eligible_series.series_id = b.SERIES_ID
                 LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = "#,
        )
        .push_bind(user.id.clone())
        .push(
            r#" WHERE rp.COMPLETED IS NULL
               ), ranked_books AS (
               SELECT book_id, most_recent_read_date,
                      ROW_NUMBER() OVER (PARTITION BY series_id ORDER BY number_sort, book_id) AS book_rank
                 FROM unread_books)
               SELECT book_id, most_recent_read_date FROM ranked_books
                WHERE book_rank = 1 ORDER BY book_id"#,
        );
    let members = query
        .build_query_as::<(String, Option<NaiveDateTime>)>()
        .fetch_all(&mut **tx)
        .await?;
    if members.is_empty() {
        return Ok(());
    }
    for (book_id, _) in &members {
        sqlx::query("INSERT INTO SYNC_POINT_READLIST_BOOK (SYNC_POINT_ID, READLIST_ID, BOOK_ID) VALUES (?, ?, ?)")
            .bind(&point.id)
            .bind(ON_DECK_READ_LIST_ID)
            .bind(book_id)
            .execute(&mut **tx)
            .await?;
    }
    let last_modified = members
        .iter()
        .filter_map(|(_, modified)| *modified)
        .max()
        .unwrap_or(point.created_date);
    sqlx::query(
        r#"INSERT INTO SYNC_POINT_READLIST
           (SYNC_POINT_ID, READLIST_ID, READLIST_NAME, READLIST_CREATED_DATE,
            READLIST_LAST_MODIFIED_DATE, SYNCED) VALUES (?, ?, 'On Deck', ?, ?, 0)"#,
    )
    .bind(&point.id)
    .bind(ON_DECK_READ_LIST_ID)
    .bind(point.created_date)
    .bind(last_modified)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn claim_books(
    tx: &mut Transaction<'_, Sqlite>,
    category: ClaimCategory,
    from: Option<&str>,
    to: &str,
    limit: usize,
) -> Result<Vec<(SyncChangeKind, SyncPointBookRow)>> {
    let (kind, sql) = match category {
        ClaimCategory::InitialBooks => (
            SyncChangeKind::Added,
            "SELECT current.* FROM SYNC_POINT_BOOK current \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 \
             ORDER BY current.BOOK_ID LIMIT ?",
        ),
        ClaimCategory::BooksAdded => (
            SyncChangeKind::Added,
            "SELECT current.* FROM SYNC_POINT_BOOK current \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 \
             AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_BOOK previous \
               WHERE previous.SYNC_POINT_ID = ? AND previous.BOOK_ID = current.BOOK_ID) \
             ORDER BY current.BOOK_ID LIMIT ?",
        ),
        ClaimCategory::BooksChanged => (
            SyncChangeKind::Changed,
            "SELECT current.* FROM SYNC_POINT_BOOK current JOIN SYNC_POINT_BOOK previous \
               ON previous.BOOK_ID = current.BOOK_ID AND previous.SYNC_POINT_ID = ? \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 AND (\
               NOT (current.BOOK_FILE_LAST_MODIFIED IS previous.BOOK_FILE_LAST_MODIFIED) OR \
               NOT (current.BOOK_FILE_SIZE IS previous.BOOK_FILE_SIZE) OR \
               NOT (current.BOOK_FILE_HASH IS previous.BOOK_FILE_HASH) OR \
               NOT (current.BOOK_METADATA_LAST_MODIFIED_DATE IS previous.BOOK_METADATA_LAST_MODIFIED_DATE) OR \
               NOT (current.BOOK_THUMBNAIL_ID IS previous.BOOK_THUMBNAIL_ID)) \
             ORDER BY current.BOOK_ID LIMIT ?",
        ),
        ClaimCategory::BooksRemoved => (
            SyncChangeKind::Removed,
            "SELECT previous.* FROM SYNC_POINT_BOOK previous \
             WHERE previous.SYNC_POINT_ID = ? \
             AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_BOOK current \
               WHERE current.SYNC_POINT_ID = ? AND current.BOOK_ID = previous.BOOK_ID) \
             AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_BOOK_REMOVED_SYNCED done \
               WHERE done.SYNC_POINT_ID = ? AND done.BOOK_ID = previous.BOOK_ID) \
             ORDER BY previous.BOOK_ID LIMIT ?",
        ),
        ClaimCategory::ProgressChanged => (
            SyncChangeKind::ProgressChanged,
            "SELECT current.* FROM SYNC_POINT_BOOK current JOIN SYNC_POINT_BOOK previous \
               ON previous.BOOK_ID = current.BOOK_ID AND previous.SYNC_POINT_ID = ? \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 \
             AND current.BOOK_FILE_LAST_MODIFIED IS previous.BOOK_FILE_LAST_MODIFIED \
             AND current.BOOK_FILE_SIZE IS previous.BOOK_FILE_SIZE \
             AND current.BOOK_FILE_HASH IS previous.BOOK_FILE_HASH \
             AND current.BOOK_METADATA_LAST_MODIFIED_DATE IS previous.BOOK_METADATA_LAST_MODIFIED_DATE \
             AND current.BOOK_THUMBNAIL_ID IS previous.BOOK_THUMBNAIL_ID \
             AND NOT (current.BOOK_READ_PROGRESS_LAST_MODIFIED_DATE IS previous.BOOK_READ_PROGRESS_LAST_MODIFIED_DATE) \
             ORDER BY current.BOOK_ID LIMIT ?",
        ),
        _ => unreachable!("read-list category passed to claim_books"),
    };
    let limit = i64::try_from(limit).map_err(|_| StorageError::InvalidQuery("sync limit is too large".into()))?;
    let rows = match category {
        ClaimCategory::InitialBooks => {
            sqlx::query_as::<_, SyncPointBookRow>(sql)
                .bind(to)
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        ClaimCategory::BooksAdded => {
            sqlx::query_as::<_, SyncPointBookRow>(sql)
                .bind(to)
                .bind(from.expect("incremental category has a source"))
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        ClaimCategory::BooksChanged | ClaimCategory::ProgressChanged => {
            sqlx::query_as::<_, SyncPointBookRow>(sql)
                .bind(from.expect("incremental category has a source"))
                .bind(to)
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        ClaimCategory::BooksRemoved => {
            sqlx::query_as::<_, SyncPointBookRow>(sql)
                .bind(from.expect("incremental category has a source"))
                .bind(to)
                .bind(to)
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        _ => unreachable!(),
    };
    if !rows.is_empty() {
        if kind == SyncChangeKind::Removed {
            for row in &rows {
                sqlx::query(
                    "INSERT OR IGNORE INTO SYNC_POINT_BOOK_REMOVED_SYNCED (SYNC_POINT_ID, BOOK_ID) VALUES (?, ?)",
                )
                .bind(to)
                .bind(&row.book_id)
                .execute(&mut **tx)
                .await?;
            }
        } else {
            for row in &rows {
                sqlx::query("UPDATE SYNC_POINT_BOOK SET SYNCED = 1 WHERE SYNC_POINT_ID = ? AND BOOK_ID = ?")
                    .bind(to)
                    .bind(&row.book_id)
                    .execute(&mut **tx)
                    .await?;
            }
        }
    }
    Ok(rows.into_iter().map(|row| (kind, row)).collect())
}

async fn claim_read_lists(
    tx: &mut Transaction<'_, Sqlite>,
    category: ClaimCategory,
    from: Option<&str>,
    to: &str,
    limit: usize,
) -> Result<Vec<(SyncChangeKind, SyncPointReadListRow)>> {
    let (kind, sql) = match category {
        ClaimCategory::InitialReadLists => (
            SyncChangeKind::Added,
            "SELECT current.* FROM SYNC_POINT_READLIST current \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 \
             ORDER BY current.READLIST_ID LIMIT ?",
        ),
        ClaimCategory::ReadListsAdded => (
            SyncChangeKind::Added,
            "SELECT current.* FROM SYNC_POINT_READLIST current \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 \
             AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_READLIST previous \
               WHERE previous.SYNC_POINT_ID = ? AND previous.READLIST_ID = current.READLIST_ID) \
             ORDER BY current.READLIST_ID LIMIT ?",
        ),
        ClaimCategory::ReadListsChanged => (
            SyncChangeKind::Changed,
            "SELECT current.* FROM SYNC_POINT_READLIST current JOIN SYNC_POINT_READLIST previous \
               ON previous.READLIST_ID = current.READLIST_ID AND previous.SYNC_POINT_ID = ? \
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 AND (\
               NOT (current.READLIST_NAME IS previous.READLIST_NAME) OR \
               NOT (current.READLIST_LAST_MODIFIED_DATE IS previous.READLIST_LAST_MODIFIED_DATE)) \
             ORDER BY current.READLIST_ID LIMIT ?",
        ),
        ClaimCategory::ReadListsRemoved => (
            SyncChangeKind::Removed,
            "SELECT previous.* FROM SYNC_POINT_READLIST previous \
             WHERE previous.SYNC_POINT_ID = ? \
             AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_READLIST current \
               WHERE current.SYNC_POINT_ID = ? AND current.READLIST_ID = previous.READLIST_ID) \
             AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_READLIST_REMOVED_SYNCED done \
               WHERE done.SYNC_POINT_ID = ? AND done.READLIST_ID = previous.READLIST_ID) \
             ORDER BY previous.READLIST_ID LIMIT ?",
        ),
        _ => unreachable!("book category passed to claim_read_lists"),
    };
    let limit = i64::try_from(limit).map_err(|_| StorageError::InvalidQuery("sync limit is too large".into()))?;
    let rows = match category {
        ClaimCategory::InitialReadLists => {
            sqlx::query_as::<_, SyncPointReadListRow>(sql)
                .bind(to)
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        ClaimCategory::ReadListsAdded => {
            sqlx::query_as::<_, SyncPointReadListRow>(sql)
                .bind(to)
                .bind(from.expect("incremental category has a source"))
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        ClaimCategory::ReadListsChanged => {
            sqlx::query_as::<_, SyncPointReadListRow>(sql)
                .bind(from.expect("incremental category has a source"))
                .bind(to)
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        ClaimCategory::ReadListsRemoved => {
            sqlx::query_as::<_, SyncPointReadListRow>(sql)
                .bind(from.expect("incremental category has a source"))
                .bind(to)
                .bind(to)
                .bind(limit)
                .fetch_all(&mut **tx)
                .await?
        }
        _ => unreachable!(),
    };
    if !rows.is_empty() {
        if kind == SyncChangeKind::Removed {
            for row in &rows {
                sqlx::query(
                    "INSERT OR IGNORE INTO SYNC_POINT_READLIST_REMOVED_SYNCED (SYNC_POINT_ID, READLIST_ID) VALUES (?, ?)",
                )
                .bind(to)
                .bind(&row.readlist_id)
                .execute(&mut **tx)
                .await?;
            }
        } else {
            for row in &rows {
                sqlx::query("UPDATE SYNC_POINT_READLIST SET SYNCED = 1 WHERE SYNC_POINT_ID = ? AND READLIST_ID = ?")
                    .bind(to)
                    .bind(&row.readlist_id)
                    .execute(&mut **tx)
                    .await?;
            }
        }
    }
    Ok(rows.into_iter().map(|row| (kind, row)).collect())
}

async fn load_book_projection(tx: &mut Transaction<'_, Sqlite>, book_id: &str) -> Result<SyncBookProjection> {
    let row = sqlx::query_as::<_, BookProjectionRow>(
        r#"SELECT b.ID AS book_id, b.SERIES_ID AS series_id, bm.TITLE AS title,
                  bm.NUMBER AS number, bm.NUMBER_SORT AS number_sort, bm.ISBN AS isbn,
                  bm.SUMMARY AS summary, bm.RELEASE_DATE AS release_date,
                  bm.CREATED_DATE AS metadata_created_date, sm.TITLE AS series_title,
                  sm.PUBLISHER AS publisher, sm.LANGUAGE AS language, b.ONESHOT AS oneshot,
                  m.EPUB_IS_KEPUB AS epub_is_kepub, m.EXTENSION_CLASS AS extension_class,
                  m.EXTENSION_VALUE_BLOB AS extension_value_blob
             FROM BOOK b JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
             JOIN SERIES_METADATA sm ON sm.SERIES_ID = b.SERIES_ID
             JOIN MEDIA m ON m.BOOK_ID = b.ID WHERE b.ID = ?"#,
    )
    .bind(book_id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| StorageError::InvalidData(format!("sync projection missing for book {book_id}")))?;
    let authors =
        sqlx::query_scalar::<_, String>("SELECT NAME FROM BOOK_METADATA_AUTHOR WHERE BOOK_ID = ? ORDER BY rowid")
            .bind(book_id)
            .fetch_all(&mut **tx)
            .await?;
    let epub_is_fixed_layout =
        decode_epub_extension(row.extension_class.as_deref(), row.extension_value_blob.as_deref())
            .is_some_and(|extension| extension.is_fixed_layout);
    Ok(SyncBookProjection {
        book_id: row.book_id,
        series_id: row.series_id,
        title: row.title,
        number: row.number,
        number_sort: row.number_sort,
        isbn: (!row.isbn.trim().is_empty()).then_some(row.isbn),
        summary: row.summary,
        release_date: row.release_date,
        metadata_created_date: row.metadata_created_date,
        series_title: row.series_title,
        publisher: row.publisher,
        language: row.language,
        oneshot: row.oneshot,
        epub_is_kepub: row.epub_is_kepub,
        epub_is_fixed_layout,
        authors,
    })
}

async fn load_read_progress(
    tx: &mut Transaction<'_, Sqlite>,
    book_id: &str,
    user_id: &str,
) -> Result<Option<komga_core::SyncReadProgress>> {
    let row = sqlx::query_as::<_, ReadProgressRow>(
        r#"SELECT BOOK_ID AS book_id, USER_ID AS user_id, PAGE AS page,
                  COMPLETED AS completed, READ_DATE AS read_date,
                  CREATED_DATE AS created_date,
                  LAST_MODIFIED_DATE AS last_modified_date,
                  DEVICE_ID AS device_id, DEVICE_NAME AS device_name, LOCATOR AS locator
             FROM READ_PROGRESS WHERE BOOK_ID = ? AND USER_ID = ?"#,
    )
    .bind(book_id)
    .bind(user_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(ReadProgressRow::into_sync_domain))
}

fn decode_epub_extension(class: Option<&str>, bytes: Option<&[u8]>) -> Option<MediaExtensionEpub> {
    if class != Some(EPUB_EXTENSION_CLASS) {
        return None;
    }
    let mut decoder = GzDecoder::new(bytes?);
    let mut json = Vec::new();
    decoder.read_to_end(&mut json).ok()?;
    serde_json::from_slice(&json).ok()
}

async fn has_unclaimed(tx: &mut Transaction<'_, Sqlite>, from: Option<&str>, to: &str) -> Result<bool> {
    let Some(from) = from else {
        let unsynced_current = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM SYNC_POINT_BOOK WHERE SYNC_POINT_ID = ? AND SYNCED = 0)",
        )
        .bind(to)
        .fetch_one(&mut **tx)
        .await?;
        let unsynced_lists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM SYNC_POINT_READLIST WHERE SYNC_POINT_ID = ? AND SYNCED = 0)",
        )
        .bind(to)
        .fetch_one(&mut **tx)
        .await?;
        return Ok(unsynced_current || unsynced_lists);
    };
    let added_books = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM SYNC_POINT_BOOK current
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0
               AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_BOOK previous
                   WHERE previous.SYNC_POINT_ID = ? AND previous.BOOK_ID = current.BOOK_ID))"#,
    )
    .bind(to)
    .bind(from)
    .fetch_one(&mut **tx)
    .await?;
    let changed_books = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM SYNC_POINT_BOOK current
             JOIN SYNC_POINT_BOOK previous ON previous.BOOK_ID = current.BOOK_ID
                  AND previous.SYNC_POINT_ID = ?
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 AND (
               NOT (current.BOOK_FILE_LAST_MODIFIED IS previous.BOOK_FILE_LAST_MODIFIED) OR
               NOT (current.BOOK_FILE_SIZE IS previous.BOOK_FILE_SIZE) OR
               NOT (current.BOOK_FILE_HASH IS previous.BOOK_FILE_HASH) OR
               NOT (current.BOOK_METADATA_LAST_MODIFIED_DATE IS previous.BOOK_METADATA_LAST_MODIFIED_DATE) OR
               NOT (current.BOOK_THUMBNAIL_ID IS previous.BOOK_THUMBNAIL_ID)))"#,
    )
    .bind(from)
    .bind(to)
    .fetch_one(&mut **tx)
    .await?;
    let changed_progress = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM SYNC_POINT_BOOK current
             JOIN SYNC_POINT_BOOK previous ON previous.BOOK_ID = current.BOOK_ID
                  AND previous.SYNC_POINT_ID = ?
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0
               AND current.BOOK_FILE_LAST_MODIFIED IS previous.BOOK_FILE_LAST_MODIFIED
               AND current.BOOK_FILE_SIZE IS previous.BOOK_FILE_SIZE
               AND current.BOOK_FILE_HASH IS previous.BOOK_FILE_HASH
               AND current.BOOK_METADATA_LAST_MODIFIED_DATE IS previous.BOOK_METADATA_LAST_MODIFIED_DATE
               AND current.BOOK_THUMBNAIL_ID IS previous.BOOK_THUMBNAIL_ID
               AND NOT (current.BOOK_READ_PROGRESS_LAST_MODIFIED_DATE IS previous.BOOK_READ_PROGRESS_LAST_MODIFIED_DATE))"#,
    )
    .bind(from)
    .bind(to)
    .fetch_one(&mut **tx)
    .await?;
    let removed_books = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
             SELECT 1 FROM SYNC_POINT_BOOK previous WHERE previous.SYNC_POINT_ID = ?
              AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_BOOK current
                  WHERE current.SYNC_POINT_ID = ? AND current.BOOK_ID = previous.BOOK_ID)
              AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_BOOK_REMOVED_SYNCED done
                  WHERE done.SYNC_POINT_ID = ? AND done.BOOK_ID = previous.BOOK_ID))"#,
    )
    .bind(from)
    .bind(to)
    .bind(to)
    .fetch_one(&mut **tx)
    .await?;
    let added_lists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM SYNC_POINT_READLIST current
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0
               AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_READLIST previous
                   WHERE previous.SYNC_POINT_ID = ? AND previous.READLIST_ID = current.READLIST_ID))"#,
    )
    .bind(to)
    .bind(from)
    .fetch_one(&mut **tx)
    .await?;
    let changed_lists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(SELECT 1 FROM SYNC_POINT_READLIST current
             JOIN SYNC_POINT_READLIST previous ON previous.READLIST_ID = current.READLIST_ID
                  AND previous.SYNC_POINT_ID = ?
             WHERE current.SYNC_POINT_ID = ? AND current.SYNCED = 0 AND (
               NOT (current.READLIST_NAME IS previous.READLIST_NAME) OR
               NOT (current.READLIST_LAST_MODIFIED_DATE IS previous.READLIST_LAST_MODIFIED_DATE)))"#,
    )
    .bind(from)
    .bind(to)
    .fetch_one(&mut **tx)
    .await?;
    let removed_lists = sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
             SELECT 1 FROM SYNC_POINT_READLIST previous WHERE previous.SYNC_POINT_ID = ?
              AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_READLIST current
                  WHERE current.SYNC_POINT_ID = ? AND current.READLIST_ID = previous.READLIST_ID)
              AND NOT EXISTS (SELECT 1 FROM SYNC_POINT_READLIST_REMOVED_SYNCED done
                  WHERE done.SYNC_POINT_ID = ? AND done.READLIST_ID = previous.READLIST_ID))"#,
    )
    .bind(from)
    .bind(to)
    .bind(to)
    .fetch_one(&mut **tx)
    .await?;
    Ok(added_books
        || changed_books
        || removed_books
        || changed_progress
        || added_lists
        || changed_lists
        || removed_lists)
}

async fn delete_sync_points(tx: &mut Transaction<'_, Sqlite>, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    for table in [
        "SYNC_POINT_READLIST_REMOVED_SYNCED",
        "SYNC_POINT_READLIST_BOOK",
        "SYNC_POINT_READLIST",
        "SYNC_POINT_BOOK_REMOVED_SYNCED",
        "SYNC_POINT_BOOK",
    ] {
        let mut query = QueryBuilder::<Sqlite>::new(format!("DELETE FROM {table} WHERE SYNC_POINT_ID IN ("));
        let mut separated = query.separated(",");
        for id in ids {
            separated.push_bind(id.clone());
        }
        query.push(")");
        query.build().execute(&mut **tx).await?;
    }
    let mut query = QueryBuilder::<Sqlite>::new("DELETE FROM SYNC_POINT WHERE ID IN (");
    let mut separated = query.separated(",");
    for id in ids {
        separated.push_bind(id.clone());
    }
    query.push(")");
    query.build().execute(&mut **tx).await?;
    Ok(())
}

impl SyncPointRow {
    fn into_domain(self) -> SyncPoint {
        SyncPoint {
            id: self.id,
            user_id: self.user_id,
            api_key_id: self.api_key_id,
            created_date: self.created_date,
        }
    }
}

impl SyncPointBookRow {
    fn into_domain(self) -> SyncPointBook {
        SyncPointBook {
            sync_point_id: self.sync_point_id,
            book_id: self.book_id,
            created_date: self.book_created_date,
            last_modified_date: self.book_last_modified_date,
            file_last_modified: self.book_file_last_modified,
            file_size: self.book_file_size,
            file_hash: self.book_file_hash,
            metadata_last_modified_date: self.book_metadata_last_modified_date,
            read_progress_last_modified_date: self.book_read_progress_last_modified_date,
            thumbnail_id: self.book_thumbnail_id,
            synced: self.synced,
        }
    }
}

impl SyncPointReadListRow {
    fn into_domain(self) -> SyncPointReadList {
        SyncPointReadList {
            sync_point_id: self.sync_point_id,
            read_list_id: self.readlist_id,
            read_list_name: self.readlist_name,
            created_date: self.readlist_created_date,
            last_modified_date: self.readlist_last_modified_date,
            synced: self.synced,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, num::NonZeroUsize};

    use chrono::Utc;
    use komga_core::{Library, ReadProgress, SyncChangeKind, User, scan_library_files};
    use url::Url;

    use super::*;

    async fn seeded_storage() -> (tempfile::TempDir, Storage, User, Vec<komga_core::Book>) {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/One.epub"), b"one").unwrap();
        fs::write(root.join("Series/Two.epub"), b"two").unwrap();
        fs::write(root.join("Series/Ignored.cbz"), b"ignored").unwrap();
        let library = Library::new("Sync".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let books = storage.list_active_books_by_library_id(&library.id).await.unwrap();
        for book in &books {
            let media_type = if book.name == "Ignored" {
                "application/zip"
            } else {
                EPUB_MEDIA_TYPE
            };
            sqlx::query("UPDATE MEDIA SET STATUS='READY', MEDIA_TYPE=? WHERE BOOK_ID=?")
                .bind(media_type)
                .bind(&book.id)
                .execute(storage.pool())
                .await
                .unwrap();
        }
        let user = User::new("sync@example.org".into(), "unused".into());
        storage.create_user(&user).await.unwrap();
        (temporary, storage, user, books)
    }

    #[tokio::test]
    async fn snapshot_filters_epubs_and_concurrent_claims_do_not_repeat_books() {
        let (_temporary, storage, user, _books) = seeded_storage().await;
        let point = storage.create_sync_point(&user, Some("device")).await.unwrap();
        let one = NonZeroUsize::new(1).unwrap();
        let (left, right) = tokio::join!(
            storage.take_sync_batch(None, &point.id, &user.id, one),
            storage.take_sync_batch(None, &point.id, &user.id, one),
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(left.books.len(), 1);
        assert_eq!(right.books.len(), 1);
        assert_ne!(left.books[0].snapshot.book_id, right.books[0].snapshot.book_id);
        assert!(left.books[0].snapshot.file_hash.is_empty());
        assert!(right.books[0].snapshot.file_hash.is_empty());
        let done = storage.take_sync_batch(None, &point.id, &user.id, one).await.unwrap();
        assert!(done.books.is_empty());
        assert!(!done.has_more);
    }

    #[tokio::test]
    async fn incremental_batches_separate_metadata_progress_and_removal_changes() {
        let (_temporary, storage, user, books) = seeded_storage().await;
        let epub_books = books.iter().filter(|book| book.name != "Ignored").collect::<Vec<_>>();
        let from = storage.create_sync_point(&user, None).await.unwrap();
        sqlx::query(
            "UPDATE BOOK_METADATA SET LAST_MODIFIED_DATE=datetime(LAST_MODIFIED_DATE, '+1 second') WHERE BOOK_ID=?",
        )
        .bind(&epub_books[0].id)
        .execute(storage.pool())
        .await
        .unwrap();
        let changed = storage.create_sync_point(&user, None).await.unwrap();
        let batch = storage
            .take_sync_batch(Some(&from.id), &changed.id, &user.id, NonZeroUsize::new(10).unwrap())
            .await
            .unwrap();
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].kind, SyncChangeKind::Changed);
        assert!(!batch.has_more);

        storage
            .save_read_progress(&ReadProgress {
                book_id: epub_books[1].id.clone(),
                user_id: user.id.clone(),
                page: 1,
                completed: false,
                read_date: Utc::now().naive_utc(),
                device_id: "device".into(),
                device_name: "reader".into(),
                locator: None,
            })
            .await
            .unwrap();
        let progress = storage.create_sync_point(&user, None).await.unwrap();
        let batch = storage
            .take_sync_batch(
                Some(&changed.id),
                &progress.id,
                &user.id,
                NonZeroUsize::new(10).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].kind, SyncChangeKind::ProgressChanged);

        sqlx::query("UPDATE MEDIA SET STATUS='ERROR' WHERE BOOK_ID=?")
            .bind(&epub_books[0].id)
            .execute(storage.pool())
            .await
            .unwrap();
        let removed = storage.create_sync_point(&user, None).await.unwrap();
        let batch = storage
            .take_sync_batch(
                Some(&progress.id),
                &removed.id,
                &user.id,
                NonZeroUsize::new(10).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(batch.books.len(), 1);
        assert_eq!(batch.books[0].kind, SyncChangeKind::Removed);
        assert!(batch.books[0].projection.is_none());
    }

    #[tokio::test]
    async fn sync_point_cleanup_is_scoped_by_user_and_api_key() {
        let (_temporary, storage, user, _books) = seeded_storage().await;
        let other = User::new("other@example.org".into(), "unused".into());
        storage.create_user(&other).await.unwrap();
        let first = storage.create_sync_point(&user, Some("first-key")).await.unwrap();
        let second = storage.create_sync_point(&user, Some("second-key")).await.unwrap();
        let foreign = storage.create_sync_point(&other, Some("first-key")).await.unwrap();

        storage
            .delete_sync_points_for_user(&user.id, &["first-key".into()])
            .await
            .unwrap();
        assert!(
            storage
                .find_sync_point_for_user(&first.id, &user.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .find_sync_point_for_user(&second.id, &user.id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            storage
                .find_sync_point_for_user(&foreign.id, &other.id)
                .await
                .unwrap()
                .is_some()
        );

        storage.delete_sync_points_for_user(&user.id, &[]).await.unwrap();
        assert!(
            storage
                .find_sync_point_for_user(&second.id, &user.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            storage
                .find_sync_point_for_user(&foreign.id, &other.id)
                .await
                .unwrap()
                .is_some()
        );
    }
}
