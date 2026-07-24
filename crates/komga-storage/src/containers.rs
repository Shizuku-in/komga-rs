//! Transactional collections and read lists.

use std::collections::{BTreeSet, HashSet};

use chrono::Utc;
use komga_core::{
    BookCondition, BookDto, BookSearch, CollectionCreation, CollectionDto, ContentPage, EqualityOperator, PageRequest,
    ReadListCreation, ReadListDto, ReadListReadProgressDto, SeriesCondition, SeriesDto, SeriesSearch, SortDirection,
    SortOrder, User,
};
use sqlx::{FromRow, Row, Sqlite, Transaction};
use unicode_normalization::UnicodeNormalization;

use crate::{Result, Storage, StorageError, read_progress::aggregate_series_progress_by_series};

#[derive(Debug, FromRow)]
struct CollectionRow {
    id: String,
    name: String,
    ordered: bool,
    series_count: i32,
    created_date: String,
    last_modified_date: String,
}

#[derive(Debug, FromRow)]
struct ReadListRow {
    id: String,
    name: String,
    summary: String,
    ordered: bool,
    book_count: i32,
    created_date: String,
    last_modified_date: String,
}

/// Persisted container thumbnail used by the HTTP adapter.
#[derive(Debug, Clone, FromRow)]
pub struct ContainerThumbnail {
    /// Stable identifier.
    pub id: String,
    /// Inline image bytes.
    pub thumbnail: Option<Vec<u8>>,
    /// Optional filesystem URL.
    pub url: Option<String>,
    /// Selection state.
    pub selected: bool,
    /// Kotlin type.
    pub thumbnail_type: String,
    /// Media type.
    pub media_type: String,
    /// Encoded size.
    pub file_size: i64,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
    /// Parent identifier.
    pub parent_id: String,
}

fn normalized_name(name: &str) -> String {
    name.nfkc().flat_map(char::to_lowercase).collect()
}

fn validate_members(name: &str, members: &[String]) -> Result<()> {
    if name.trim().is_empty() {
        return Err(StorageError::InvalidQuery("name must not be blank".into()));
    }
    if members.is_empty() {
        return Err(StorageError::InvalidQuery("members must not be empty".into()));
    }
    if members.iter().collect::<HashSet<_>>().len() != members.len() {
        return Err(StorageError::InvalidQuery("members must not contain duplicates".into()));
    }
    Ok(())
}

impl Storage {
    /// Lists uploaded thumbnails for a collection or read list.
    pub async fn list_container_thumbnails(&self, read_list: bool, parent_id: &str) -> Result<Vec<ContainerThumbnail>> {
        let (table, column) = container_thumbnail_table(read_list);
        Ok(sqlx::query_as(&format!("SELECT ID id,THUMBNAIL thumbnail,URL url,SELECTED selected,TYPE thumbnail_type,MEDIA_TYPE media_type,FILE_SIZE file_size,WIDTH width,HEIGHT height,{column} parent_id FROM {table} WHERE {column}=? ORDER BY CREATED_DATE,ID")).bind(parent_id).fetch_all(&self.pool).await?)
    }
    /// Finds a parent-owned thumbnail.
    pub async fn find_container_thumbnail(
        &self,
        read_list: bool,
        parent_id: &str,
        id: &str,
    ) -> Result<Option<ContainerThumbnail>> {
        let (table, column) = container_thumbnail_table(read_list);
        Ok(sqlx::query_as(&format!("SELECT ID id,THUMBNAIL thumbnail,URL url,SELECTED selected,TYPE thumbnail_type,MEDIA_TYPE media_type,FILE_SIZE file_size,WIDTH width,HEIGHT height,{column} parent_id FROM {table} WHERE {column}=? AND ID=?")).bind(parent_id).bind(id).fetch_optional(&self.pool).await?)
    }
    /// Finds the selected parent-owned thumbnail.
    pub async fn selected_container_thumbnail(
        &self,
        read_list: bool,
        parent_id: &str,
    ) -> Result<Option<ContainerThumbnail>> {
        let (table, column) = container_thumbnail_table(read_list);
        Ok(sqlx::query_as(&format!("SELECT ID id,THUMBNAIL thumbnail,URL url,SELECTED selected,TYPE thumbnail_type,MEDIA_TYPE media_type,FILE_SIZE file_size,WIDTH width,HEIGHT height,{column} parent_id FROM {table} WHERE {column}=? AND SELECTED=1 LIMIT 1")).bind(parent_id).fetch_optional(&self.pool).await?)
    }
    /// Inserts a user upload and maintains one selected thumbnail.
    pub async fn insert_container_thumbnail(
        &self,
        read_list: bool,
        value: &ContainerThumbnail,
        selected: bool,
    ) -> Result<ContainerThumbnail> {
        let (table, column) = container_thumbnail_table(read_list);
        let mut tx = self.pool.begin().await?;
        sqlx::query(&format!("INSERT INTO {table}(ID,THUMBNAIL,URL,SELECTED,TYPE,{column},WIDTH,HEIGHT,MEDIA_TYPE,FILE_SIZE) VALUES(?,?,?,0,'USER_UPLOADED',?,?,?,?,?)")).bind(&value.id).bind(&value.thumbnail).bind(&value.url).bind(&value.parent_id).bind(value.width).bind(value.height).bind(&value.media_type).bind(value.file_size).execute(&mut *tx).await?;
        let becomes_selected = selected
            || !sqlx::query_scalar::<_, bool>(&format!(
                "SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}=? AND SELECTED=1)"
            ))
            .bind(&value.parent_id)
            .fetch_one(&mut *tx)
            .await?;
        if becomes_selected {
            select_container(&mut tx, table, column, &value.parent_id, &value.id).await?
        }
        tx.commit().await?;
        let mut saved = value.clone();
        saved.selected = becomes_selected;
        Ok(saved)
    }
    /// Selects a parent-owned thumbnail exclusively.
    pub async fn select_container_thumbnail(&self, read_list: bool, parent_id: &str, id: &str) -> Result<bool> {
        let (table, column) = container_thumbnail_table(read_list);
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, bool>(&format!(
            "SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}=? AND ID=?)"
        ))
        .bind(parent_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if exists {
            select_container(&mut tx, table, column, parent_id, id).await?
        }
        tx.commit().await?;
        Ok(exists)
    }
    /// Deletes an upload and selects the oldest remaining thumbnail.
    pub async fn delete_container_thumbnail(&self, read_list: bool, parent_id: &str, id: &str) -> Result<bool> {
        let (table, column) = container_thumbnail_table(read_list);
        let mut tx = self.pool.begin().await?;
        let was_selected = sqlx::query_scalar::<_, bool>(&format!(
            "SELECT COALESCE((SELECT SELECTED FROM {table} WHERE {column}=? AND ID=?),0)"
        ))
        .bind(parent_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        let deleted = sqlx::query(&format!(
            "DELETE FROM {table} WHERE {column}=? AND ID=? AND TYPE='USER_UPLOADED'"
        ))
        .bind(parent_id)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if deleted
            && was_selected
            && let Some(next) = sqlx::query_scalar::<_, String>(&format!(
                "SELECT ID FROM {table} WHERE {column}=? ORDER BY CREATED_DATE,ID LIMIT 1"
            ))
            .bind(parent_id)
            .fetch_optional(&mut *tx)
            .await?
        {
            select_container(&mut tx, table, column, parent_id, &next).await?
        }
        tx.commit().await?;
        Ok(deleted)
    }
    /// Creates a collection and all ordered members atomically.
    pub async fn create_collection(&self, id: &str, value: &CollectionCreation) -> Result<()> {
        validate_members(&value.name, &value.series_ids)?;
        let mut tx = self.pool.begin().await?;
        ensure_unique_name(&mut tx, "COLLECTION", id, &value.name).await?;
        ensure_members_exist(&mut tx, "SERIES", &value.series_ids).await?;
        sqlx::query("INSERT INTO COLLECTION (ID, NAME, ORDERED, SERIES_COUNT) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(value.name.trim())
            .bind(value.ordered)
            .bind(value.series_ids.len() as i32)
            .execute(&mut *tx)
            .await?;
        replace_members(
            &mut tx,
            "COLLECTION_SERIES",
            "COLLECTION_ID",
            "SERIES_ID",
            id,
            &value.series_ids,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces a collection atomically; `None` fields preserve their value.
    pub async fn update_collection(
        &self,
        id: &str,
        name: Option<String>,
        ordered: Option<bool>,
        members: Option<Vec<String>>,
    ) -> Result<bool> {
        let Some(current) = self.collection_row(id).await? else {
            return Ok(false);
        };
        let name = name.unwrap_or(current.name);
        let ordered = ordered.unwrap_or(current.ordered);
        let members = match members {
            Some(v) => v,
            None => member_ids(&self.pool, "COLLECTION_SERIES", "COLLECTION_ID", "SERIES_ID", id).await?,
        };
        validate_members(&name, &members)?;
        let mut tx = self.pool.begin().await?;
        ensure_unique_name(&mut tx, "COLLECTION", id, &name).await?;
        ensure_members_exist(&mut tx, "SERIES", &members).await?;
        sqlx::query(
            "UPDATE COLLECTION SET NAME=?, ORDERED=?, SERIES_COUNT=?, LAST_MODIFIED_DATE=CURRENT_TIMESTAMP WHERE ID=?",
        )
        .bind(name.trim())
        .bind(ordered)
        .bind(members.len() as i32)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        replace_members(&mut tx, "COLLECTION_SERIES", "COLLECTION_ID", "SERIES_ID", id, &members).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Deletes a collection.
    pub async fn delete_collection(&self, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM THUMBNAIL_COLLECTION WHERE COLLECTION_ID=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM COLLECTION_SERIES WHERE COLLECTION_ID=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM COLLECTION WHERE ID=?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        tx.commit().await?;
        Ok(deleted)
    }

    /// Loads a visible collection projection.
    pub async fn find_collection(&self, id: &str, user: &User) -> Result<Option<CollectionDto>> {
        let Some(row) = self.collection_row(id).await? else {
            return Ok(None);
        };
        self.project_collection(row, user).await
    }

    /// Lists visible collection projections with stable Unicode-aware name search.
    pub async fn list_collections(
        &self,
        search: Option<&str>,
        library_id: Option<&str>,
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<CollectionDto>> {
        let rows=sqlx::query_as::<_,CollectionRow>("SELECT ID id,NAME name,ORDERED ordered,SERIES_COUNT series_count,CAST(CREATED_DATE AS TEXT) created_date,CAST(LAST_MODIFIED_DATE AS TEXT) last_modified_date FROM COLLECTION").fetch_all(&self.pool).await?;
        let needle = search.map(normalized_name);
        let mut out = Vec::new();
        for row in rows {
            if needle.as_ref().is_some_and(|n| !normalized_name(&row.name).contains(n)) {
                continue;
            }
            if let Some(dto) = self.project_collection(row, user).await? {
                let matches_library = match library_id {
                    Some(library) => collection_has_library(&self.pool, &dto.id, library).await?,
                    None => true,
                };
                if matches_library {
                    out.push(dto);
                }
            }
        }
        for sort in &page.sort {
            if !matches!(
                sort.property.as_str(),
                "name" | "createdDate" | "created" | "lastModifiedDate" | "lastModified"
            ) {
                return Err(StorageError::InvalidQuery(format!(
                    "unsupported read-list sort property: {}",
                    sort.property
                )));
            }
        }
        out.sort_by(|a, b| {
            for sort in &page.sort {
                let ordering = match sort.property.as_str() {
                    "name" => normalized_name(&a.name).cmp(&normalized_name(&b.name)),
                    "createdDate" | "created" => a.created_date.cmp(&b.created_date),
                    "lastModifiedDate" | "lastModified" => a.last_modified_date.cmp(&b.last_modified_date),
                    _ => unreachable!("sort fields were validated"),
                };
                let ordering = match sort.direction {
                    SortDirection::Ascending => ordering,
                    SortDirection::Descending => ordering.reverse(),
                };
                if !ordering.is_eq() {
                    return ordering;
                }
            }
            normalized_name(&a.name)
                .cmp(&normalized_name(&b.name))
                .then(a.id.cmp(&b.id))
        });
        Ok(page_vec(out, page))
    }

    /// Lists visible collection members using the shared content query engine.
    pub async fn collection_series(
        &self,
        id: &str,
        user: &User,
        mut page: PageRequest,
    ) -> Result<ContentPage<SeriesDto>> {
        if page.sort.is_empty() {
            let ordered = sqlx::query_scalar::<_, bool>("SELECT ORDERED FROM COLLECTION WHERE ID=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or(false);
            page.sort = vec![if ordered {
                SortOrder {
                    property: "collection.number".into(),
                    direction: SortDirection::Ascending,
                }
            } else {
                SortOrder {
                    property: "metadata.titleSort".into(),
                    direction: SortDirection::Ascending,
                }
            }];
        }
        self.query_series(
            &SeriesSearch {
                condition: Some(SeriesCondition::CollectionId {
                    operator: EqualityOperator::Is { value: id.into() },
                }),
                full_text_search: None,
            },
            &page,
            user,
        )
        .await
    }

    /// Lists visible collections containing a series.
    pub async fn collections_for_series(&self, series_id: &str, user: &User) -> Result<Vec<CollectionDto>> {
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT COLLECTION_ID FROM COLLECTION_SERIES WHERE SERIES_ID=? ORDER BY NUMBER,COLLECTION_ID",
        )
        .bind(series_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(dto) = self.find_collection(&id, user).await? {
                out.push(dto)
            }
        }
        Ok(out)
    }

    /// Creates a read list and all ordered members atomically.
    pub async fn create_read_list(&self, id: &str, value: &ReadListCreation) -> Result<()> {
        validate_members(&value.name, &value.book_ids)?;
        let mut tx = self.pool.begin().await?;
        ensure_unique_name(&mut tx, "READLIST", id, &value.name).await?;
        ensure_members_exist(&mut tx, "BOOK", &value.book_ids).await?;
        sqlx::query("INSERT INTO READLIST (ID,NAME,SUMMARY,ORDERED,BOOK_COUNT) VALUES (?,?,?,?,?)")
            .bind(id)
            .bind(value.name.trim())
            .bind(&value.summary)
            .bind(value.ordered)
            .bind(value.book_ids.len() as i32)
            .execute(&mut *tx)
            .await?;
        replace_members(&mut tx, "READLIST_BOOK", "READLIST_ID", "BOOK_ID", id, &value.book_ids).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces a read list atomically; `None` fields preserve their value.
    pub async fn update_read_list(
        &self,
        id: &str,
        name: Option<String>,
        summary: Option<String>,
        ordered: Option<bool>,
        members: Option<Vec<String>>,
    ) -> Result<bool> {
        let Some(cur) = self.read_list_row(id).await? else {
            return Ok(false);
        };
        let name = name.unwrap_or(cur.name);
        let summary = summary.unwrap_or(cur.summary);
        let ordered = ordered.unwrap_or(cur.ordered);
        let members = match members {
            Some(v) => v,
            None => member_ids(&self.pool, "READLIST_BOOK", "READLIST_ID", "BOOK_ID", id).await?,
        };
        validate_members(&name, &members)?;
        let mut tx = self.pool.begin().await?;
        ensure_unique_name(&mut tx, "READLIST", id, &name).await?;
        ensure_members_exist(&mut tx, "BOOK", &members).await?;
        sqlx::query("UPDATE READLIST SET NAME=?,SUMMARY=?,ORDERED=?,BOOK_COUNT=?,LAST_MODIFIED_DATE=CURRENT_TIMESTAMP WHERE ID=?").bind(name.trim()).bind(summary).bind(ordered).bind(members.len() as i32).bind(id).execute(&mut *tx).await?;
        replace_members(&mut tx, "READLIST_BOOK", "READLIST_ID", "BOOK_ID", id, &members).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Deletes a read list.
    pub async fn delete_read_list(&self, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM THUMBNAIL_READLIST WHERE READLIST_ID=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM READLIST_BOOK WHERE READLIST_ID=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM READLIST WHERE ID=?")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        tx.commit().await?;
        Ok(deleted)
    }
    /// Loads a visible read-list projection.
    pub async fn find_read_list(&self, id: &str, user: &User) -> Result<Option<ReadListDto>> {
        let Some(row) = self.read_list_row(id).await? else {
            return Ok(None);
        };
        self.project_read_list(row, user).await
    }
    /// Lists visible read-list projections.
    pub async fn list_read_lists(
        &self,
        search: Option<&str>,
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<ReadListDto>> {
        let rows=sqlx::query_as::<_,ReadListRow>("SELECT ID id,NAME name,SUMMARY summary,ORDERED ordered,BOOK_COUNT book_count,CAST(CREATED_DATE AS TEXT) created_date,CAST(LAST_MODIFIED_DATE AS TEXT) last_modified_date FROM READLIST").fetch_all(&self.pool).await?;
        let needle = search.map(normalized_name);
        let mut out = Vec::new();
        for row in rows {
            if needle.as_ref().is_some_and(|n| !normalized_name(&row.name).contains(n)) {
                continue;
            }
            if let Some(dto) = self.project_read_list(row, user).await? {
                out.push(dto)
            }
        }
        out.sort_by(|a, b| {
            normalized_name(&a.name)
                .cmp(&normalized_name(&b.name))
                .then(a.id.cmp(&b.id))
        });
        Ok(page_vec(out, page))
    }
    /// Lists visible read lists containing a book.
    pub async fn read_lists_for_book(&self, book_id: &str, user: &User) -> Result<Vec<ReadListDto>> {
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT READLIST_ID FROM READLIST_BOOK WHERE BOOK_ID=? ORDER BY NUMBER,READLIST_ID",
        )
        .bind(book_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(dto) = self.find_read_list(&id, user).await? {
                out.push(dto)
            }
        }
        Ok(out)
    }
    /// Lists visible read-list books using the shared query engine.
    pub async fn read_list_books(&self, id: &str, user: &User, mut page: PageRequest) -> Result<ContentPage<BookDto>> {
        if page.sort.is_empty() {
            let ordered = sqlx::query_scalar::<_, bool>("SELECT ORDERED FROM READLIST WHERE ID=?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or(false);
            page.sort = vec![SortOrder {
                property: if ordered {
                    "readList.number".into()
                } else {
                    "metadata.releaseDate".into()
                },
                direction: SortDirection::Ascending,
            }];
        }
        self.query_books(
            &BookSearch {
                condition: Some(BookCondition::ReadListId {
                    operator: EqualityOperator::Is { value: id.into() },
                }),
                full_text_search: None,
            },
            &page,
            user,
        )
        .await
    }

    /// Finds a visible adjacent member in stored order.
    pub async fn read_list_sibling(&self, id: &str, book_id: &str, user: &User, next: bool) -> Result<Option<BookDto>> {
        let books = self
            .read_list_books(
                id,
                user,
                PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "readList.number".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..Default::default()
                },
            )
            .await?
            .content;
        let Some(pos) = books.iter().position(|b| b.id == book_id) else {
            return Ok(None);
        };
        Ok(if next {
            books.into_iter().nth(pos + 1)
        } else {
            pos.checked_sub(1).and_then(|p| books.into_iter().nth(p))
        })
    }

    /// Computes Mihon progress over visible stored-order members.
    pub async fn read_list_progress(&self, id: &str, user: &User) -> Result<Option<ReadListReadProgressDto>> {
        if self.find_read_list(id, user).await?.is_none() {
            return Ok(None);
        }
        let books = self
            .read_list_books(
                id,
                user,
                PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "readList.number".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..Default::default()
                },
            )
            .await?
            .content;
        let mut read = 0;
        let mut progress = 0;
        let mut continuous = 0;
        let mut gap = false;
        for b in &books {
            match &b.read_progress {
                Some(p) if p.completed => {
                    read += 1;
                    if !gap {
                        continuous += 1
                    }
                }
                Some(_) => {
                    progress += 1;
                    gap = true
                }
                None => gap = true,
            }
        }
        let count = books.len() as i32;
        Ok(Some(ReadListReadProgressDto {
            books_count: count,
            books_read_count: read,
            books_unread_count: count - read - progress,
            books_in_progress_count: progress,
            last_read_continuous_index: continuous,
        }))
    }

    /// Completes the first N visible read-list members in one transaction.
    pub async fn complete_read_list(&self, id: &str, user: &User, last: u32) -> Result<bool> {
        if self.find_read_list(id, user).await?.is_none() {
            return Ok(false);
        }
        let books = self
            .read_list_books(
                id,
                user,
                PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "readList.number".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..Default::default()
                },
            )
            .await?
            .content;
        let targets = books.into_iter().take(last as usize).collect::<Vec<_>>();
        let mut tx = self.pool.begin().await?;
        let now = Utc::now().naive_utc();
        let mut series = BTreeSet::new();
        for b in targets {
            series.insert(b.series_id);
            sqlx::query("INSERT INTO READ_PROGRESS (BOOK_ID,USER_ID,PAGE,COMPLETED,READ_DATE,DEVICE_ID,DEVICE_NAME,LOCATOR) SELECT b.ID,?,m.PAGE_COUNT,1,?,'','',NULL FROM BOOK b JOIN MEDIA m ON m.BOOK_ID=b.ID LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID=b.ID AND rp.USER_ID=? WHERE b.ID=? AND (rp.BOOK_ID IS NULL OR rp.COMPLETED=0) ON CONFLICT(BOOK_ID,USER_ID) DO UPDATE SET PAGE=excluded.PAGE,COMPLETED=1,READ_DATE=excluded.READ_DATE,LAST_MODIFIED_DATE=CURRENT_TIMESTAMP,DEVICE_ID='',DEVICE_NAME='',LOCATOR=NULL").bind(&user.id).bind(now).bind(&user.id).bind(&b.id).execute(&mut *tx).await?;
        }
        for sid in series {
            aggregate_series_progress_by_series(&mut tx, &sid, &user.id).await?
        }
        tx.commit().await?;
        Ok(true)
    }

    async fn collection_row(&self, id: &str) -> Result<Option<CollectionRow>> {
        Ok(sqlx::query_as("SELECT ID id,NAME name,ORDERED ordered,SERIES_COUNT series_count,CAST(CREATED_DATE AS TEXT) created_date,CAST(LAST_MODIFIED_DATE AS TEXT) last_modified_date FROM COLLECTION WHERE ID=?").bind(id).fetch_optional(&self.pool).await?)
    }
    async fn read_list_row(&self, id: &str) -> Result<Option<ReadListRow>> {
        Ok(sqlx::query_as("SELECT ID id,NAME name,SUMMARY summary,ORDERED ordered,BOOK_COUNT book_count,CAST(CREATED_DATE AS TEXT) created_date,CAST(LAST_MODIFIED_DATE AS TEXT) last_modified_date FROM READLIST WHERE ID=?").bind(id).fetch_optional(&self.pool).await?)
    }
    async fn project_collection(&self, row: CollectionRow, user: &User) -> Result<Option<CollectionDto>> {
        let all = member_ids(&self.pool, "COLLECTION_SERIES", "COLLECTION_ID", "SERIES_ID", &row.id).await?;
        let visible = self
            .collection_series(
                &row.id,
                user,
                PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "collection.number".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..Default::default()
                },
            )
            .await?
            .content
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>();
        if visible.is_empty() && !user.is_admin() {
            return Ok(None);
        }
        Ok(Some(CollectionDto {
            id: row.id,
            name: row.name,
            ordered: row.ordered,
            filtered: row.series_count as usize != visible.len() || all.len() != visible.len(),
            series_ids: visible,
            created_date: row.created_date,
            last_modified_date: row.last_modified_date,
        }))
    }
    async fn project_read_list(&self, row: ReadListRow, user: &User) -> Result<Option<ReadListDto>> {
        let all = member_ids(&self.pool, "READLIST_BOOK", "READLIST_ID", "BOOK_ID", &row.id).await?;
        let visible = self
            .read_list_books(
                &row.id,
                user,
                PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "readList.number".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..Default::default()
                },
            )
            .await?
            .content
            .into_iter()
            .map(|b| b.id)
            .collect::<Vec<_>>();
        if visible.is_empty() && !user.is_admin() {
            return Ok(None);
        }
        Ok(Some(ReadListDto {
            id: row.id,
            name: row.name,
            summary: row.summary,
            ordered: row.ordered,
            filtered: row.book_count as usize != visible.len() || all.len() != visible.len(),
            book_ids: visible,
            created_date: row.created_date,
            last_modified_date: row.last_modified_date,
        }))
    }
}

fn container_thumbnail_table(read_list: bool) -> (&'static str, &'static str) {
    if read_list {
        ("THUMBNAIL_READLIST", "READLIST_ID")
    } else {
        ("THUMBNAIL_COLLECTION", "COLLECTION_ID")
    }
}
async fn select_container(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    column: &str,
    parent: &str,
    id: &str,
) -> Result<()> {
    sqlx::query(&format!(
        "UPDATE {table} SET SELECTED=(ID=?),LAST_MODIFIED_DATE=CURRENT_TIMESTAMP WHERE {column}=?"
    ))
    .bind(id)
    .bind(parent)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn ensure_unique_name(tx: &mut Transaction<'_, Sqlite>, table: &str, id: &str, name: &str) -> Result<()> {
    let rows = sqlx::query(&format!("SELECT ID,NAME FROM {table} WHERE ID<>?"))
        .bind(id)
        .fetch_all(&mut **tx)
        .await?;
    if rows
        .iter()
        .any(|r| normalized_name(r.get::<String, _>("NAME").as_str()) == normalized_name(name))
    {
        return Err(StorageError::InvalidQuery("name already exists".into()));
    }
    Ok(())
}
async fn ensure_members_exist(tx: &mut Transaction<'_, Sqlite>, table: &str, ids: &[String]) -> Result<()> {
    for id in ids {
        if !sqlx::query_scalar::<_, bool>(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE ID=?)"))
            .bind(id)
            .fetch_one(&mut **tx)
            .await?
        {
            return Err(StorageError::InvalidQuery("member does not exist".into()));
        }
    }
    Ok(())
}
async fn replace_members(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    parent: &str,
    member: &str,
    id: &str,
    ids: &[String],
) -> Result<()> {
    sqlx::query(&format!("DELETE FROM {table} WHERE {parent}=?"))
        .bind(id)
        .execute(&mut **tx)
        .await?;
    for (number, member_id) in ids.iter().enumerate() {
        sqlx::query(&format!(
            "INSERT INTO {table} ({parent},{member},NUMBER) VALUES (?,?,?)"
        ))
        .bind(id)
        .bind(member_id)
        .bind(number as i32)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
async fn member_ids(pool: &sqlx::SqlitePool, table: &str, parent: &str, member: &str, id: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT {member} FROM {table} WHERE {parent}=? ORDER BY NUMBER"
    ))
    .bind(id)
    .fetch_all(pool)
    .await?)
}
async fn collection_has_library(pool: &sqlx::SqlitePool, id: &str, library: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM COLLECTION_SERIES cs JOIN SERIES s ON s.ID=cs.SERIES_ID WHERE cs.COLLECTION_ID=? AND s.LIBRARY_ID=?)").bind(id).bind(library).fetch_one(pool).await?)
}
fn page_vec<T>(mut rows: Vec<T>, page: &PageRequest) -> ContentPage<T> {
    let total = rows.len() as u64;
    if !page.unpaged {
        let start = (page.page * page.size) as usize;
        rows = rows.into_iter().skip(start).take(page.size as usize).collect()
    }
    ContentPage {
        content: rows,
        total_elements: total,
        page: if page.unpaged { 0 } else { page.page },
        size: if page.unpaged { total.max(20) } else { page.size },
        sort: page.sort.clone(),
    }
}
