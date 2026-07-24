use std::collections::BTreeSet;

use komga_core::{
    AllowExclude, AlternateTitleDto, AuthorDto, BookCondition, BookDto, BookMetadataAggregationDto, BookMetadataDto,
    BookSearch, BooleanOperator, ContentPage, EqualityOperator, MediaDto, MediaProfile, PageRequest, ReadProgressDto,
    ReadStatus, ReadStatusOperator, SeriesCondition, SeriesDto, SeriesMetadataDto, SeriesSearch, SortDirection,
    StringOperator, User, WebLinkDto,
};
use serde::Deserialize;
use sqlx::{FromRow, QueryBuilder, Sqlite};
use url::Url;

use crate::{Result, Storage, StorageError};

#[derive(Debug, FromRow)]
struct SeriesQueryRow {
    id: String,
    library_id: String,
    name: String,
    url: String,
    created: String,
    last_modified: String,
    file_last_modified: String,
    books_count: i32,
    deleted: bool,
    oneshot: bool,
    books_read_count: i32,
    books_in_progress_count: i32,
    metadata_status: String,
    metadata_status_lock: bool,
    metadata_title: String,
    metadata_title_lock: bool,
    metadata_title_sort: String,
    metadata_title_sort_lock: bool,
    metadata_summary: String,
    metadata_summary_lock: bool,
    metadata_reading_direction: String,
    metadata_reading_direction_lock: bool,
    metadata_publisher: String,
    metadata_publisher_lock: bool,
    metadata_age_rating: Option<i32>,
    metadata_age_rating_lock: bool,
    metadata_language: String,
    metadata_language_lock: bool,
    metadata_genres_lock: bool,
    metadata_tags_lock: bool,
    metadata_total_book_count: Option<i32>,
    metadata_total_book_count_lock: bool,
    metadata_sharing_labels_lock: bool,
    metadata_links_lock: bool,
    metadata_alternate_titles_lock: bool,
    metadata_created: String,
    metadata_last_modified: String,
    genres_json: String,
    tags_json: String,
    sharing_labels_json: String,
    links_json: String,
    alternate_titles_json: String,
    aggregation_authors_json: String,
    aggregation_tags_json: String,
    aggregation_release_date: Option<String>,
    aggregation_summary: String,
    aggregation_summary_number: String,
    aggregation_created: String,
    aggregation_last_modified: String,
}

#[derive(Debug, FromRow)]
struct BookQueryRow {
    id: String,
    series_id: String,
    series_title: String,
    library_id: String,
    name: String,
    url: String,
    number: i32,
    created: String,
    last_modified: String,
    file_last_modified: String,
    size_bytes: i64,
    deleted: bool,
    file_hash: String,
    oneshot: bool,
    media_status: String,
    media_type: String,
    media_pages_count: i32,
    media_comment: String,
    media_epub_divina_compatible: bool,
    media_epub_is_kepub: bool,
    metadata_title: String,
    metadata_title_lock: bool,
    metadata_summary: String,
    metadata_summary_lock: bool,
    metadata_number: String,
    metadata_number_lock: bool,
    metadata_number_sort: f64,
    metadata_number_sort_lock: bool,
    metadata_release_date: Option<String>,
    metadata_release_date_lock: bool,
    metadata_authors_lock: bool,
    metadata_tags_lock: bool,
    metadata_isbn: String,
    metadata_isbn_lock: bool,
    metadata_links_lock: bool,
    metadata_created: String,
    metadata_last_modified: String,
    authors_json: String,
    tags_json: String,
    links_json: String,
    progress_user_id: Option<String>,
    progress_page: Option<i32>,
    progress_completed: Option<bool>,
    progress_read_date: Option<String>,
    progress_created: Option<String>,
    progress_last_modified: Option<String>,
    progress_device_id: Option<String>,
    progress_device_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorJson {
    name: String,
    role: String,
}

#[derive(Debug, Deserialize)]
struct LinkJson {
    label: String,
    url: String,
}

#[derive(Debug, Deserialize)]
struct AlternateTitleJson {
    label: String,
    title: String,
}

const SERIES_SELECT: &str = r#"
SELECT s.ID AS id, s.LIBRARY_ID AS library_id, s.NAME AS name, s.URL AS url,
       CAST(s.CREATED_DATE AS TEXT) AS created,
       CAST(s.LAST_MODIFIED_DATE AS TEXT) AS last_modified,
       CAST(s.FILE_LAST_MODIFIED AS TEXT) AS file_last_modified,
       s.BOOK_COUNT AS books_count, s.DELETED_DATE IS NOT NULL AS deleted,
       s.ONESHOT AS oneshot,
       COALESCE(rps.READ_COUNT, 0) AS books_read_count,
       COALESCE(rps.IN_PROGRESS_COUNT, 0) AS books_in_progress_count,
       sm.STATUS AS metadata_status, sm.STATUS_LOCK AS metadata_status_lock,
       sm.TITLE AS metadata_title, sm.TITLE_LOCK AS metadata_title_lock,
       sm.TITLE_SORT AS metadata_title_sort, sm.TITLE_SORT_LOCK AS metadata_title_sort_lock,
       sm.SUMMARY AS metadata_summary, sm.SUMMARY_LOCK AS metadata_summary_lock,
       COALESCE(sm.READING_DIRECTION, '') AS metadata_reading_direction,
       sm.READING_DIRECTION_LOCK AS metadata_reading_direction_lock,
       sm.PUBLISHER AS metadata_publisher, sm.PUBLISHER_LOCK AS metadata_publisher_lock,
       sm.AGE_RATING AS metadata_age_rating, sm.AGE_RATING_LOCK AS metadata_age_rating_lock,
       sm.LANGUAGE AS metadata_language, sm.LANGUAGE_LOCK AS metadata_language_lock,
       sm.GENRES_LOCK AS metadata_genres_lock, sm.TAGS_LOCK AS metadata_tags_lock,
       sm.TOTAL_BOOK_COUNT AS metadata_total_book_count,
       sm.TOTAL_BOOK_COUNT_LOCK AS metadata_total_book_count_lock,
       sm.SHARING_LABELS_LOCK AS metadata_sharing_labels_lock,
       sm.LINKS_LOCK AS metadata_links_lock,
       sm.ALTERNATE_TITLES_LOCK AS metadata_alternate_titles_lock,
       CAST(sm.CREATED_DATE AS TEXT) AS metadata_created,
       CAST(sm.LAST_MODIFIED_DATE AS TEXT) AS metadata_last_modified,
       (SELECT json_group_array(GENRE) FROM SERIES_METADATA_GENRE WHERE SERIES_ID = s.ID) AS genres_json,
       (SELECT json_group_array(TAG) FROM SERIES_METADATA_TAG WHERE SERIES_ID = s.ID) AS tags_json,
       (SELECT json_group_array(LABEL) FROM SERIES_METADATA_SHARING WHERE SERIES_ID = s.ID) AS sharing_labels_json,
       (SELECT json_group_array(json_object('label', LABEL, 'url', URL)) FROM SERIES_METADATA_LINK WHERE SERIES_ID = s.ID) AS links_json,
       (SELECT json_group_array(json_object('label', LABEL, 'title', TITLE)) FROM SERIES_METADATA_ALTERNATE_TITLE WHERE SERIES_ID = s.ID) AS alternate_titles_json,
       (SELECT json_group_array(json_object('name', NAME, 'role', ROLE)) FROM BOOK_METADATA_AGGREGATION_AUTHOR WHERE SERIES_ID = s.ID) AS aggregation_authors_json,
       (SELECT json_group_array(TAG) FROM BOOK_METADATA_AGGREGATION_TAG WHERE SERIES_ID = s.ID) AS aggregation_tags_json,
       CAST(bma.RELEASE_DATE AS TEXT) AS aggregation_release_date,
       bma.SUMMARY AS aggregation_summary, bma.SUMMARY_NUMBER AS aggregation_summary_number,
       CAST(bma.CREATED_DATE AS TEXT) AS aggregation_created,
       CAST(bma.LAST_MODIFIED_DATE AS TEXT) AS aggregation_last_modified
  FROM SERIES s
  JOIN SERIES_METADATA sm ON sm.SERIES_ID = s.ID
  JOIN BOOK_METADATA_AGGREGATION bma ON bma.SERIES_ID = s.ID
  LEFT JOIN READ_PROGRESS_SERIES rps ON rps.SERIES_ID = s.ID AND rps.USER_ID = "#;

const BOOK_SELECT: &str = r#"
SELECT b.ID AS id, b.SERIES_ID AS series_id, sm.TITLE AS series_title,
       b.LIBRARY_ID AS library_id, b.NAME AS name, b.URL AS url, b.NUMBER AS number,
       CAST(b.CREATED_DATE AS TEXT) AS created,
       CAST(b.LAST_MODIFIED_DATE AS TEXT) AS last_modified,
       CAST(b.FILE_LAST_MODIFIED AS TEXT) AS file_last_modified,
       b.FILE_SIZE AS size_bytes, b.DELETED_DATE IS NOT NULL AS deleted,
       b.FILE_HASH AS file_hash, b.ONESHOT AS oneshot,
       m.STATUS AS media_status, COALESCE(m.MEDIA_TYPE, '') AS media_type,
       m.PAGE_COUNT AS media_pages_count, COALESCE(m.COMMENT, '') AS media_comment,
       m.EPUB_DIVINA_COMPATIBLE AS media_epub_divina_compatible,
       m.EPUB_IS_KEPUB AS media_epub_is_kepub,
       bm.TITLE AS metadata_title, bm.TITLE_LOCK AS metadata_title_lock,
       bm.SUMMARY AS metadata_summary, bm.SUMMARY_LOCK AS metadata_summary_lock,
       bm.NUMBER AS metadata_number, bm.NUMBER_LOCK AS metadata_number_lock,
       bm.NUMBER_SORT AS metadata_number_sort, bm.NUMBER_SORT_LOCK AS metadata_number_sort_lock,
       CAST(bm.RELEASE_DATE AS TEXT) AS metadata_release_date,
       bm.RELEASE_DATE_LOCK AS metadata_release_date_lock,
       bm.AUTHORS_LOCK AS metadata_authors_lock, bm.TAGS_LOCK AS metadata_tags_lock,
       bm.ISBN AS metadata_isbn, bm.ISBN_LOCK AS metadata_isbn_lock,
       bm.LINKS_LOCK AS metadata_links_lock,
       CAST(bm.CREATED_DATE AS TEXT) AS metadata_created,
       CAST(bm.LAST_MODIFIED_DATE AS TEXT) AS metadata_last_modified,
       (SELECT json_group_array(json_object('name', NAME, 'role', ROLE)) FROM BOOK_METADATA_AUTHOR WHERE BOOK_ID = b.ID) AS authors_json,
       (SELECT json_group_array(TAG) FROM BOOK_METADATA_TAG WHERE BOOK_ID = b.ID) AS tags_json,
       (SELECT json_group_array(json_object('label', LABEL, 'url', URL)) FROM BOOK_METADATA_LINK WHERE BOOK_ID = b.ID) AS links_json,
       rp.USER_ID AS progress_user_id, rp.PAGE AS progress_page,
       rp.COMPLETED AS progress_completed, CAST(rp.READ_DATE AS TEXT) AS progress_read_date,
       CAST(rp.CREATED_DATE AS TEXT) AS progress_created,
       CAST(rp.LAST_MODIFIED_DATE AS TEXT) AS progress_last_modified,
       rp.DEVICE_ID AS progress_device_id, rp.DEVICE_NAME AS progress_device_name
  FROM BOOK b
  JOIN MEDIA m ON m.BOOK_ID = b.ID
  JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
  JOIN SERIES_METADATA sm ON sm.SERIES_ID = b.SERIES_ID
  LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = "#;

impl Storage {
    /// Lists visible series using the supported Kotlin search-condition subset.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unsupported full-text search or sort
    /// fields, and a database error when the query cannot be executed.
    pub async fn query_series(
        &self,
        search: &SeriesSearch,
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<SeriesDto>> {
        validate_page(page)?;
        reject_full_text(search.full_text_search.as_deref())?;
        validate_series_sorts(page)?;

        let mut count = series_count_builder(user);
        push_series_visibility(&mut count, user);
        if let Some(condition) = &search.condition {
            count.push(" AND ");
            push_series_condition(&mut count, condition);
        }
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;
        let total = u64::try_from(total).map_err(|_| StorageError::InvalidData("negative series count".into()))?;

        let mut query = series_query_builder(user);
        push_series_visibility(&mut query, user);
        if let Some(condition) = &search.condition {
            query.push(" AND ");
            push_series_condition(&mut query, condition);
        }
        push_series_order(&mut query, page);
        push_pagination(&mut query, page)?;
        let rows = query.build_query_as::<SeriesQueryRow>().fetch_all(&self.pool).await?;
        let content = rows.into_iter().map(SeriesQueryRow::into_dto).collect::<Result<_>>()?;

        Ok(content_page(content, total, page))
    }

    /// Finds a series by ID without applying user visibility rules.
    ///
    /// The caller uses the returned metadata to distinguish Kotlin's `403`
    /// response for restricted content from `404` for a missing row.
    ///
    /// # Errors
    ///
    /// Returns an error when the row cannot be queried or hydrated.
    pub async fn find_series_dto_by_id(&self, id: &str, user_id: &str) -> Result<Option<SeriesDto>> {
        let mut query = series_query_builder_for_user_id(user_id);
        query.push(" AND s.ID = ").push_bind(id.to_owned());
        query
            .build_query_as::<SeriesQueryRow>()
            .fetch_optional(&self.pool)
            .await?
            .map(SeriesQueryRow::into_dto)
            .transpose()
    }

    /// Lists visible books using the supported Kotlin search-condition subset.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unsupported full-text search or sort
    /// fields, and a database error when the query cannot be executed.
    pub async fn query_books(
        &self,
        search: &BookSearch,
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<BookDto>> {
        validate_page(page)?;
        reject_full_text(search.full_text_search.as_deref())?;
        validate_book_sorts(page)?;

        let mut count = book_count_builder(user);
        push_book_visibility(&mut count, user);
        if let Some(condition) = &search.condition {
            count.push(" AND ");
            push_book_condition(&mut count, condition);
        }
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;
        let total = u64::try_from(total).map_err(|_| StorageError::InvalidData("negative book count".into()))?;

        let mut query = book_query_builder(user);
        push_book_visibility(&mut query, user);
        if let Some(condition) = &search.condition {
            query.push(" AND ");
            push_book_condition(&mut query, condition);
        }
        push_book_order(&mut query, page);
        push_pagination(&mut query, page)?;
        let rows = query.build_query_as::<BookQueryRow>().fetch_all(&self.pool).await?;
        let content = rows.into_iter().map(BookQueryRow::into_dto).collect::<Result<_>>()?;

        Ok(content_page(content, total, page))
    }

    /// Finds a book by ID without applying user visibility rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the row cannot be queried or hydrated.
    pub async fn find_book_dto_by_id(&self, id: &str, user_id: &str) -> Result<Option<BookDto>> {
        let mut query = book_query_builder_for_user_id(user_id);
        query.push(" AND b.ID = ").push_bind(id.to_owned());
        query
            .build_query_as::<BookQueryRow>()
            .fetch_optional(&self.pool)
            .await?
            .map(BookQueryRow::into_dto)
            .transpose()
    }

    /// Finds the previous or next book in metadata-number order.
    ///
    /// # Errors
    ///
    /// Returns an error when the source or sibling query fails.
    pub async fn find_book_sibling_dto(&self, book_id: &str, user_id: &str, next: bool) -> Result<Option<BookDto>> {
        let source = sqlx::query_as::<_, (String, f64)>(
            "SELECT b.SERIES_ID, bm.NUMBER_SORT FROM BOOK b JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID WHERE b.ID = ?",
        )
        .bind(book_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((series_id, number_sort)) = source else {
            return Ok(None);
        };
        let comparison = if next { ">" } else { "<" };
        let direction = if next { "ASC" } else { "DESC" };
        let sibling_id = sqlx::query_scalar::<_, String>(&format!(
            "SELECT b.ID FROM BOOK b JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID \
             WHERE b.SERIES_ID = ? AND bm.NUMBER_SORT {comparison} ? \
             ORDER BY bm.NUMBER_SORT {direction} LIMIT 1"
        ))
        .bind(series_id)
        .bind(number_sort)
        .fetch_optional(&self.pool)
        .await?;
        match sibling_id {
            Some(id) => self.find_book_dto_by_id(&id, user_id).await,
            None => Ok(None),
        }
    }
}

fn series_query_builder(user: &User) -> QueryBuilder<'static, Sqlite> {
    series_query_builder_for_user_id(&user.id)
}

fn series_query_builder_for_user_id(user_id: &str) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(SERIES_SELECT);
    query.push_bind(user_id.to_owned()).push(" WHERE 1 = 1");
    query
}

fn series_count_builder(user: &User) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(
        r#"SELECT COUNT(*)
             FROM SERIES s
             JOIN SERIES_METADATA sm ON sm.SERIES_ID = s.ID
             JOIN BOOK_METADATA_AGGREGATION bma ON bma.SERIES_ID = s.ID
             LEFT JOIN READ_PROGRESS_SERIES rps ON rps.SERIES_ID = s.ID AND rps.USER_ID = "#,
    );
    query.push_bind(user.id.clone()).push(" WHERE 1 = 1");
    query
}

fn book_query_builder(user: &User) -> QueryBuilder<'static, Sqlite> {
    book_query_builder_for_user_id(&user.id)
}

fn book_query_builder_for_user_id(user_id: &str) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(BOOK_SELECT);
    query.push_bind(user_id.to_owned()).push(" WHERE 1 = 1");
    query
}

fn book_count_builder(user: &User) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(
        r#"SELECT COUNT(*)
             FROM BOOK b
             JOIN MEDIA m ON m.BOOK_ID = b.ID
             JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
             JOIN SERIES_METADATA sm ON sm.SERIES_ID = b.SERIES_ID
             LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = "#,
    );
    query.push_bind(user.id.clone()).push(" WHERE 1 = 1");
    query
}

fn push_series_visibility(query: &mut QueryBuilder<'static, Sqlite>, user: &User) {
    push_library_visibility(query, "s.LIBRARY_ID", user);
    push_content_visibility(query, "s.ID", user);
}

fn push_book_visibility(query: &mut QueryBuilder<'static, Sqlite>, user: &User) {
    push_library_visibility(query, "b.LIBRARY_ID", user);
    push_content_visibility(query, "b.SERIES_ID", user);
}

fn push_library_visibility(query: &mut QueryBuilder<'static, Sqlite>, column: &str, user: &User) {
    if user.is_admin() || user.shared_all_libraries {
        return;
    }
    if user.shared_library_ids.is_empty() {
        query.push(" AND 0 = 1");
        return;
    }
    query.push(" AND ").push(column).push(" IN (");
    push_string_values(query, user.shared_library_ids.iter());
    query.push(")");
}

fn push_content_visibility(query: &mut QueryBuilder<'static, Sqlite>, series_id: &str, user: &User) {
    let restrictions = &user.restrictions;
    let allow_age = restrictions
        .age_restriction
        .as_ref()
        .filter(|restriction| restriction.restriction == AllowExclude::AllowOnly);
    let allow_labels = &restrictions.labels_allow;

    if allow_age.is_some() || !allow_labels.is_empty() {
        query.push(" AND (");
        let mut has_allow = false;
        if let Some(restriction) = allow_age {
            query
                .push("(sm.AGE_RATING IS NOT NULL AND sm.AGE_RATING <= ")
                .push_bind(i64::from(restriction.age))
                .push(")");
            has_allow = true;
        }
        if !allow_labels.is_empty() {
            if has_allow {
                query.push(" OR ");
            }
            query
                .push("EXISTS (SELECT 1 FROM SERIES_METADATA_SHARING visibility_allow ")
                .push("WHERE visibility_allow.SERIES_ID = ")
                .push(series_id)
                .push(" AND lower(visibility_allow.LABEL) IN (");
            push_string_values(query, allow_labels.iter());
            query.push("))");
        }
        query.push(")");
    }

    if let Some(restriction) = restrictions
        .age_restriction
        .as_ref()
        .filter(|restriction| restriction.restriction == AllowExclude::Exclude)
    {
        query
            .push(" AND NOT (sm.AGE_RATING IS NOT NULL AND sm.AGE_RATING >= ")
            .push_bind(i64::from(restriction.age))
            .push(")");
    }
    if !restrictions.labels_exclude.is_empty() {
        query
            .push(" AND NOT EXISTS (SELECT 1 FROM SERIES_METADATA_SHARING visibility_exclude ")
            .push("WHERE visibility_exclude.SERIES_ID = ")
            .push(series_id)
            .push(" AND lower(visibility_exclude.LABEL) IN (");
        push_string_values(query, restrictions.labels_exclude.iter());
        query.push("))");
    }
}

fn push_string_values<'a>(query: &mut QueryBuilder<'static, Sqlite>, values: impl IntoIterator<Item = &'a String>) {
    let mut separated = query.separated(", ");
    for value in values {
        separated.push_bind(value.clone());
    }
}

fn push_series_condition(query: &mut QueryBuilder<'static, Sqlite>, condition: &SeriesCondition) {
    match condition {
        SeriesCondition::AllOf { conditions } => {
            push_condition_group(query, conditions, " AND ", push_series_condition);
        }
        SeriesCondition::AnyOf { conditions } => {
            push_condition_group(query, conditions, " OR ", push_series_condition);
        }
        SeriesCondition::LibraryId { operator } => push_equality(query, "s.LIBRARY_ID", operator),
        SeriesCondition::CollectionId { operator } => {
            query.push("EXISTS(SELECT 1 FROM COLLECTION_SERIES cs WHERE cs.SERIES_ID = s.ID AND ");
            push_equality(query, "cs.COLLECTION_ID", operator);
            query.push(")");
        }
        SeriesCondition::Deleted { deleted } => push_optional_boolean(query, "s.DELETED_DATE", deleted),
        SeriesCondition::OneShot { operator } => push_boolean(query, "s.ONESHOT", operator),
        SeriesCondition::Title { title } => push_string_operator(query, "sm.TITLE", title),
        SeriesCondition::TitleSort { operator } => push_string_operator(query, "sm.TITLE_SORT", operator),
        SeriesCondition::SeriesStatus { operator } => push_equality(query, "sm.STATUS", operator),
        SeriesCondition::ReadStatus { operator } => push_series_read_status(query, operator),
    }
}

fn push_book_condition(query: &mut QueryBuilder<'static, Sqlite>, condition: &BookCondition) {
    match condition {
        BookCondition::AllOf { conditions } => {
            push_condition_group(query, conditions, " AND ", push_book_condition);
        }
        BookCondition::AnyOf { conditions } => {
            push_condition_group(query, conditions, " OR ", push_book_condition);
        }
        BookCondition::LibraryId { operator } => push_equality(query, "b.LIBRARY_ID", operator),
        BookCondition::SeriesId { operator } => push_equality(query, "b.SERIES_ID", operator),
        BookCondition::ReadListId { operator } => {
            query.push("EXISTS(SELECT 1 FROM READLIST_BOOK rb WHERE rb.BOOK_ID = b.ID AND ");
            push_equality(query, "rb.READLIST_ID", operator);
            query.push(")");
        }
        BookCondition::Deleted { deleted } => push_optional_boolean(query, "b.DELETED_DATE", deleted),
        BookCondition::OneShot { operator } => push_boolean(query, "b.ONESHOT", operator),
        BookCondition::Title { title } => push_string_operator(query, "bm.TITLE", title),
        BookCondition::MediaStatus { operator } => push_equality(query, "m.STATUS", operator),
        BookCondition::ReadStatus { operator } => push_book_read_status(query, operator),
    }
}

fn push_condition_group<T>(
    query: &mut QueryBuilder<'static, Sqlite>,
    conditions: &[T],
    separator: &str,
    push: fn(&mut QueryBuilder<'static, Sqlite>, &T),
) {
    if conditions.is_empty() {
        query.push("1 = 1");
        return;
    }
    query.push("(");
    for (index, condition) in conditions.iter().enumerate() {
        if index > 0 {
            query.push(separator);
        }
        push(query, condition);
    }
    query.push(")");
}

fn push_equality(query: &mut QueryBuilder<'static, Sqlite>, column: &str, operator: &EqualityOperator) {
    match operator {
        EqualityOperator::Is { value } => {
            query.push(column).push(" = ").push_bind(value.clone());
        }
        EqualityOperator::IsNot { value } => {
            query.push(column).push(" <> ").push_bind(value.clone());
        }
    }
}

fn push_optional_boolean(query: &mut QueryBuilder<'static, Sqlite>, column: &str, operator: &BooleanOperator) {
    query.push(column).push(match operator {
        BooleanOperator::IsTrue => " IS NOT NULL",
        BooleanOperator::IsFalse => " IS NULL",
    });
}

fn push_boolean(query: &mut QueryBuilder<'static, Sqlite>, column: &str, operator: &BooleanOperator) {
    query.push(column).push(match operator {
        BooleanOperator::IsTrue => " = 1",
        BooleanOperator::IsFalse => " = 0",
    });
}

fn push_string_operator(query: &mut QueryBuilder<'static, Sqlite>, column: &str, operator: &StringOperator) {
    let (comparison, value) = match operator {
        StringOperator::Is { value } => (" = ", value.to_lowercase()),
        StringOperator::IsNot { value } => (" <> ", value.to_lowercase()),
        StringOperator::Contains { value } => (" LIKE ", like_pattern(value, true, true)),
        StringOperator::DoesNotContain { value } => (" NOT LIKE ", like_pattern(value, true, true)),
        StringOperator::BeginsWith { value } => (" LIKE ", like_pattern(value, false, true)),
        StringOperator::DoesNotBeginWith { value } => (" NOT LIKE ", like_pattern(value, false, true)),
        StringOperator::EndsWith { value } => (" LIKE ", like_pattern(value, true, false)),
        StringOperator::DoesNotEndWith { value } => (" NOT LIKE ", like_pattern(value, true, false)),
    };
    query
        .push("lower(")
        .push(column)
        .push(")")
        .push(comparison)
        .push_bind(value);
    if comparison.contains("LIKE") {
        query.push(" ESCAPE '\\'");
    }
}

fn like_pattern(value: &str, prefix: bool, suffix: bool) -> String {
    let escaped = value
        .to_lowercase()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!(
        "{}{}{}",
        if prefix { "%" } else { "" },
        escaped,
        if suffix { "%" } else { "" }
    )
}

fn push_series_read_status(query: &mut QueryBuilder<'static, Sqlite>, operator: &ReadStatusOperator) {
    let expression = match operator {
        ReadStatusOperator::Is {
            value: ReadStatus::Unread,
        } => "rps.READ_COUNT IS NULL",
        ReadStatusOperator::Is {
            value: ReadStatus::Read,
        } => "rps.READ_COUNT = s.BOOK_COUNT",
        ReadStatusOperator::Is {
            value: ReadStatus::InProgress,
        } => "rps.READ_COUNT <> s.BOOK_COUNT",
        ReadStatusOperator::IsNot {
            value: ReadStatus::Unread,
        } => "rps.READ_COUNT IS NOT NULL",
        ReadStatusOperator::IsNot {
            value: ReadStatus::Read,
        } => "(rps.READ_COUNT <> s.BOOK_COUNT OR rps.READ_COUNT IS NULL)",
        ReadStatusOperator::IsNot {
            value: ReadStatus::InProgress,
        } => "(rps.READ_COUNT = s.BOOK_COUNT OR rps.READ_COUNT IS NULL)",
    };
    query.push(expression);
}

fn push_book_read_status(query: &mut QueryBuilder<'static, Sqlite>, operator: &ReadStatusOperator) {
    let expression = match operator {
        ReadStatusOperator::Is {
            value: ReadStatus::Unread,
        } => "rp.COMPLETED IS NULL",
        ReadStatusOperator::Is {
            value: ReadStatus::Read,
        } => "rp.COMPLETED = 1",
        ReadStatusOperator::Is {
            value: ReadStatus::InProgress,
        } => "rp.COMPLETED = 0",
        ReadStatusOperator::IsNot {
            value: ReadStatus::Unread,
        } => "rp.COMPLETED IS NOT NULL",
        ReadStatusOperator::IsNot {
            value: ReadStatus::Read,
        } => "(rp.COMPLETED IS NULL OR rp.COMPLETED = 0)",
        ReadStatusOperator::IsNot {
            value: ReadStatus::InProgress,
        } => "(rp.COMPLETED IS NULL OR rp.COMPLETED = 1)",
    };
    query.push(expression);
}

fn validate_page(page: &PageRequest) -> Result<()> {
    if !page.unpaged && page.size == 0 {
        return Err(StorageError::InvalidQuery("page size must be greater than zero".into()));
    }
    if page.size > i64::MAX as u64
        || page
            .page
            .checked_mul(page.size)
            .is_none_or(|value| value > i64::MAX as u64)
    {
        return Err(StorageError::InvalidQuery("page range is too large".into()));
    }
    Ok(())
}

fn reject_full_text(value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| !value.trim().is_empty()) {
        return Err(StorageError::InvalidQuery(
            "fullTextSearch is not available until the search phase".into(),
        ));
    }
    Ok(())
}

fn validate_series_sorts(page: &PageRequest) -> Result<()> {
    for sort in &page.sort {
        series_sort_column(&sort.property).ok_or_else(|| {
            StorageError::InvalidQuery(format!("unsupported series sort property: {}", sort.property))
        })?;
    }
    Ok(())
}

fn validate_book_sorts(page: &PageRequest) -> Result<()> {
    for sort in &page.sort {
        book_sort_column(&sort.property)
            .ok_or_else(|| StorageError::InvalidQuery(format!("unsupported book sort property: {}", sort.property)))?;
    }
    Ok(())
}

fn series_sort_column(property: &str) -> Option<&'static str> {
    match property {
        "metadata.titleSort" => Some("sm.TITLE_SORT COLLATE NOCASE"),
        "created" | "createdDate" => Some("s.CREATED_DATE"),
        "lastModified" | "lastModifiedDate" => Some("s.LAST_MODIFIED_DATE"),
        "booksMetadata.releaseDate" => Some("bma.RELEASE_DATE"),
        "readDate" => Some("rps.MOST_RECENT_READ_DATE"),
        "name" => Some("s.NAME COLLATE NOCASE"),
        "booksCount" => Some("s.BOOK_COUNT"),
        "collection.number" => Some("(SELECT cs.NUMBER FROM COLLECTION_SERIES cs WHERE cs.SERIES_ID = s.ID)"),
        _ => None,
    }
}

fn book_sort_column(property: &str) -> Option<&'static str> {
    match property {
        "name" => Some("b.NAME COLLATE NOCASE"),
        "series" => Some("sm.TITLE_SORT COLLATE NOCASE"),
        "created" | "createdDate" => Some("b.CREATED_DATE"),
        "lastModified" | "lastModifiedDate" => Some("b.LAST_MODIFIED_DATE"),
        "fileSize" | "size" => Some("b.FILE_SIZE"),
        "fileHash" => Some("b.FILE_HASH"),
        "url" => Some("b.URL COLLATE NOCASE"),
        "media.status" => Some("m.STATUS COLLATE NOCASE"),
        "media.comment" => Some("m.COMMENT COLLATE NOCASE"),
        "media.mediaType" => Some("m.MEDIA_TYPE COLLATE NOCASE"),
        "media.pagesCount" => Some("m.PAGE_COUNT"),
        "metadata.title" => Some("bm.TITLE COLLATE NOCASE"),
        "metadata.numberSort" => Some("bm.NUMBER_SORT"),
        "metadata.releaseDate" => Some("bm.RELEASE_DATE"),
        "readProgress.lastModified" => Some("rp.LAST_MODIFIED_DATE"),
        "readProgress.readDate" => Some("rp.READ_DATE"),
        "readList.number" => Some("(SELECT rb.NUMBER FROM READLIST_BOOK rb WHERE rb.BOOK_ID = b.ID)"),
        _ => None,
    }
}

fn push_series_order(query: &mut QueryBuilder<'static, Sqlite>, page: &PageRequest) {
    push_order(query, page, series_sort_column, "s.ID");
}

fn push_book_order(query: &mut QueryBuilder<'static, Sqlite>, page: &PageRequest) {
    push_order(query, page, book_sort_column, "b.ID");
}

fn push_order(
    query: &mut QueryBuilder<'static, Sqlite>,
    page: &PageRequest,
    column: fn(&str) -> Option<&'static str>,
    id_column: &str,
) {
    query.push(" ORDER BY ");
    for (index, order) in page.sort.iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        query
            .push(column(&order.property).expect("sort fields were validated"))
            .push(match order.direction {
                SortDirection::Ascending => " ASC",
                SortDirection::Descending => " DESC",
            });
    }
    if !page.sort.is_empty() {
        query.push(", ");
    }
    query.push(id_column).push(" ASC");
}

fn push_pagination(query: &mut QueryBuilder<'static, Sqlite>, page: &PageRequest) -> Result<()> {
    if page.unpaged {
        return Ok(());
    }
    let size = i64::try_from(page.size).map_err(|_| StorageError::InvalidQuery("page size is too large".into()))?;
    let offset = i64::try_from(page.page * page.size)
        .map_err(|_| StorageError::InvalidQuery("page offset is too large".into()))?;
    query.push(" LIMIT ").push_bind(size).push(" OFFSET ").push_bind(offset);
    Ok(())
}

fn content_page<T>(content: Vec<T>, total_elements: u64, request: &PageRequest) -> ContentPage<T> {
    ContentPage {
        content,
        total_elements,
        page: if request.unpaged { 0 } else { request.page },
        size: if request.unpaged {
            total_elements.max(20)
        } else {
            request.size
        },
        sort: request.sort.clone(),
    }
}

impl SeriesQueryRow {
    fn into_dto(self) -> Result<SeriesDto> {
        let genres = json_set(&self.genres_json, "series genres")?;
        let tags = json_set(&self.tags_json, "series tags")?;
        let sharing_labels = json_set(&self.sharing_labels_json, "series sharing labels")?;
        let links = json_links(&self.links_json, "series links")?;
        let alternate_titles = serde_json::from_str::<Vec<AlternateTitleJson>>(&self.alternate_titles_json)
            .map_err(|error| invalid_json("series alternate titles", error))?
            .into_iter()
            .map(|value| AlternateTitleDto {
                label: value.label,
                title: value.title,
            })
            .collect();
        let aggregation_authors = json_authors(&self.aggregation_authors_json, "series aggregated authors")?;
        let aggregation_tags = json_set(&self.aggregation_tags_json, "series aggregated tags")?;
        let books_unread_count = self.books_count - self.books_read_count - self.books_in_progress_count;

        Ok(SeriesDto {
            id: self.id,
            library_id: self.library_id,
            name: self.name,
            url: content_path(&self.url)?,
            created: content_datetime(self.created)?,
            last_modified: content_datetime(self.last_modified)?,
            file_last_modified: content_datetime(self.file_last_modified)?,
            books_count: self.books_count,
            books_read_count: self.books_read_count,
            books_unread_count,
            books_in_progress_count: self.books_in_progress_count,
            metadata: SeriesMetadataDto {
                status: self.metadata_status,
                status_lock: self.metadata_status_lock,
                title: self.metadata_title,
                title_lock: self.metadata_title_lock,
                title_sort: self.metadata_title_sort,
                title_sort_lock: self.metadata_title_sort_lock,
                summary: self.metadata_summary,
                summary_lock: self.metadata_summary_lock,
                reading_direction: self.metadata_reading_direction,
                reading_direction_lock: self.metadata_reading_direction_lock,
                publisher: self.metadata_publisher,
                publisher_lock: self.metadata_publisher_lock,
                age_rating: self.metadata_age_rating,
                age_rating_lock: self.metadata_age_rating_lock,
                language: self.metadata_language,
                language_lock: self.metadata_language_lock,
                genres,
                genres_lock: self.metadata_genres_lock,
                tags,
                tags_lock: self.metadata_tags_lock,
                total_book_count: self.metadata_total_book_count,
                total_book_count_lock: self.metadata_total_book_count_lock,
                sharing_labels,
                sharing_labels_lock: self.metadata_sharing_labels_lock,
                links,
                links_lock: self.metadata_links_lock,
                alternate_titles,
                alternate_titles_lock: self.metadata_alternate_titles_lock,
                created: content_datetime(self.metadata_created)?,
                last_modified: content_datetime(self.metadata_last_modified)?,
            },
            books_metadata: BookMetadataAggregationDto {
                authors: aggregation_authors,
                tags: aggregation_tags,
                release_date: self.aggregation_release_date,
                summary: self.aggregation_summary,
                summary_number: self.aggregation_summary_number,
                created: content_datetime(self.aggregation_created)?,
                last_modified: content_datetime(self.aggregation_last_modified)?,
            },
            deleted: self.deleted,
            oneshot: self.oneshot,
        })
    }
}

impl BookQueryRow {
    fn into_dto(self) -> Result<BookDto> {
        let read_progress = if self.progress_user_id.is_some() {
            let created = required_progress(self.progress_created, "created")?;
            let last_modified = required_progress(self.progress_last_modified, "last modified")?;
            Some(ReadProgressDto {
                page: required_progress(self.progress_page, "page")?,
                completed: required_progress(self.progress_completed, "completed")?,
                read_date: content_datetime(self.progress_read_date.unwrap_or_else(|| last_modified.clone()))?,
                created: content_datetime(created)?,
                last_modified: content_datetime(last_modified)?,
                device_id: self.progress_device_id.unwrap_or_default(),
                device_name: self.progress_device_name.unwrap_or_default(),
            })
        } else {
            None
        };
        let media_profile = media_profile(&self.media_type).to_owned();

        Ok(BookDto {
            id: self.id,
            series_id: self.series_id,
            series_title: self.series_title,
            library_id: self.library_id,
            name: self.name,
            url: content_path(&self.url)?,
            number: self.number,
            created: content_datetime(self.created)?,
            last_modified: content_datetime(self.last_modified)?,
            file_last_modified: content_datetime(self.file_last_modified)?,
            size_bytes: self.size_bytes,
            size: binary_size(self.size_bytes)?,
            media: MediaDto {
                status: self.media_status,
                media_type: self.media_type,
                pages_count: self.media_pages_count,
                comment: self.media_comment,
                media_profile,
                epub_divina_compatible: self.media_epub_divina_compatible,
                epub_is_kepub: self.media_epub_is_kepub,
            },
            metadata: BookMetadataDto {
                title: self.metadata_title,
                title_lock: self.metadata_title_lock,
                summary: self.metadata_summary,
                summary_lock: self.metadata_summary_lock,
                number: self.metadata_number,
                number_lock: self.metadata_number_lock,
                number_sort: self.metadata_number_sort,
                number_sort_lock: self.metadata_number_sort_lock,
                release_date: self.metadata_release_date,
                release_date_lock: self.metadata_release_date_lock,
                authors: json_authors(&self.authors_json, "book authors")?,
                authors_lock: self.metadata_authors_lock,
                tags: json_set(&self.tags_json, "book tags")?,
                tags_lock: self.metadata_tags_lock,
                isbn: self.metadata_isbn,
                isbn_lock: self.metadata_isbn_lock,
                links: json_links(&self.links_json, "book links")?,
                links_lock: self.metadata_links_lock,
                created: content_datetime(self.metadata_created)?,
                last_modified: content_datetime(self.metadata_last_modified)?,
            },
            read_progress,
            deleted: self.deleted,
            file_hash: self.file_hash,
            oneshot: self.oneshot,
        })
    }
}

fn json_set(value: &str, field: &str) -> Result<BTreeSet<String>> {
    serde_json::from_str::<Vec<String>>(value)
        .map(|values| values.into_iter().collect())
        .map_err(|error| invalid_json(field, error))
}

fn json_authors(value: &str, field: &str) -> Result<Vec<AuthorDto>> {
    serde_json::from_str::<Vec<AuthorJson>>(value)
        .map_err(|error| invalid_json(field, error))
        .map(|values| {
            values
                .into_iter()
                .map(|value| AuthorDto {
                    name: value.name,
                    role: value.role,
                })
                .collect()
        })
}

fn json_links(value: &str, field: &str) -> Result<Vec<WebLinkDto>> {
    serde_json::from_str::<Vec<LinkJson>>(value)
        .map_err(|error| invalid_json(field, error))
        .map(|values| {
            values
                .into_iter()
                .map(|value| WebLinkDto {
                    label: value.label,
                    url: value.url,
                })
                .collect()
        })
}

fn invalid_json(field: &str, error: serde_json::Error) -> StorageError {
    StorageError::InvalidData(format!("invalid {field}: {error}"))
}

fn required_progress<T>(value: Option<T>, field: &str) -> Result<T> {
    value.ok_or_else(|| StorageError::InvalidData(format!("read progress is missing {field}")))
}

fn content_path(value: &str) -> Result<String> {
    let url = Url::parse(value)
        .map_err(|error| StorageError::InvalidData(format!("invalid content URL {value:?}: {error}")))?;
    url.to_file_path()
        .map(|path| {
            path.components()
                .collect::<std::path::PathBuf>()
                .to_string_lossy()
                .into_owned()
        })
        .map_err(|()| StorageError::InvalidData(format!("content URL is not a file URL: {value:?}")))
}

fn content_datetime(value: String) -> Result<String> {
    let normalized = value.replace(' ', "T");
    let seconds = normalized.get(..19).ok_or_else(|| {
        StorageError::InvalidData(format!(
            "invalid content timestamp, expected at least seconds: {value:?}"
        ))
    })?;
    Ok(format!("{seconds}Z"))
}

fn media_profile(media_type: &str) -> &'static str {
    MediaProfile::from_media_type(media_type).map_or("", MediaProfile::as_str)
}

fn binary_size(bytes: i64) -> Result<String> {
    if bytes < 0 {
        return Err(StorageError::InvalidData(format!("negative book file size: {bytes}")));
    }
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let rendered = if unit == 0 {
        grouped_integer(bytes)
    } else {
        let rounded = (value * 10.0).round() / 10.0;
        if rounded.fract() == 0.0 {
            grouped_integer(rounded as i64)
        } else {
            format!("{rounded:.1}")
        }
    };
    Ok(format!("{rendered} {}", UNITS[unit]))
}

fn grouped_integer(value: i64) -> String {
    let digits = value.to_string();
    let mut rendered = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            rendered.push(',');
        }
        rendered.push(character);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_binary_sizes_like_byteunits() {
        assert_eq!(binary_size(0).unwrap(), "0 B");
        assert_eq!(binary_size(1023).unwrap(), "1,023 B");
        assert_eq!(binary_size(1024).unwrap(), "1 KiB");
        assert_eq!(binary_size(1536).unwrap(), "1.5 KiB");
        assert_eq!(binary_size(1024 * 1024).unwrap(), "1 MiB");
    }

    #[test]
    fn normalizes_content_paths_and_timestamps_like_kotlin_dtos() {
        assert_eq!(content_path("file:///tmp/Series/").unwrap(), "/tmp/Series");
        assert_eq!(
            content_datetime("2026-07-24 15:42:17.003326382".into()).unwrap(),
            "2026-07-24T15:42:17Z"
        );
    }
}
