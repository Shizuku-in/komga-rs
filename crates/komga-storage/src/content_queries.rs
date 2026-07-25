use std::collections::BTreeSet;

use komga_core::{
    AlternateTitleDto, AuthorDto, AuthorMatch, BookCondition, BookDto, BookMetadataAggregationDto, BookMetadataDto,
    BookSearch, BooleanOperator, ContentPage, DateOperator, EPUB_MEDIA_TYPE, EqualityOperator, GroupCountDto, MediaDto,
    MediaProfile, MediaStatus, NullableEqualityOperator, NullableNumericOperator, NumericOperator, PDF_MEDIA_TYPE,
    PageRequest, PosterMatch, RAR_MEDIA_TYPE, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, ReadProgressDto, ReadStatus,
    SeriesCondition, SeriesDto, SeriesMetadataDto, SeriesSearch, SeriesStatus, SortDirection, StringOperator, User,
    WebLinkDto, ZIP_MEDIA_TYPE,
};
use serde::Deserialize;
use sqlx::{FromRow, QueryBuilder, Sqlite};
use url::Url;

use crate::{
    Result, Storage, StorageError,
    query_support::{content_page, push_book_visibility, push_pagination, push_series_visibility, validate_page},
};

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

/// A visible read-list member projected for archive generation.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ReadListFile {
    /// Stable book identifier.
    pub book_id: String,
    /// Original persisted book URL.
    pub url: String,
    /// Zero-based stored position in the read list.
    pub number: i64,
}

/// Legacy Series metadata field accepted by the deprecated regex parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySeriesRegexField {
    /// Match `SERIES_METADATA.TITLE`.
    Title,
    /// Match `SERIES_METADATA.TITLE_SORT`.
    TitleSort,
}

/// Regex filter used only by deprecated Series GET queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySeriesRegex {
    /// Rust-regex pattern, evaluated case-insensitively.
    pub pattern: String,
    /// Series metadata field to match.
    pub field: LegacySeriesRegexField,
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
    /// Returns visible read-list source files in stored member order.
    pub async fn read_list_files(&self, read_list_id: &str, user: &User) -> Result<Vec<ReadListFile>> {
        let mut query = QueryBuilder::new(
            r#"SELECT b.ID book_id,b.URL url,rb.NUMBER number
                 FROM READLIST_BOOK rb
                 JOIN BOOK b ON b.ID=rb.BOOK_ID
                 JOIN MEDIA m ON m.BOOK_ID=b.ID
                 JOIN BOOK_METADATA bm ON bm.BOOK_ID=b.ID
                 JOIN SERIES_METADATA sm ON sm.SERIES_ID=b.SERIES_ID
                WHERE rb.READLIST_ID="#,
        );
        query.push_bind(read_list_id.to_owned());
        push_book_visibility(&mut query, user);
        query.push(" ORDER BY rb.NUMBER,b.ID");
        Ok(query.build_query_as().fetch_all(&self.pool).await?)
    }

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
        ranked_ids: Option<&[String]>,
    ) -> Result<ContentPage<SeriesDto>> {
        self.query_series_with_regex(search, page, user, ranked_ids, None).await
    }

    /// Lists visible series with the regex filter accepted by deprecated GET APIs.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unsupported search or paging values,
    /// or a database error when the query or regex cannot be executed.
    pub async fn query_series_legacy(
        &self,
        search: &SeriesSearch,
        page: &PageRequest,
        user: &User,
        ranked_ids: Option<&[String]>,
        regex: Option<&LegacySeriesRegex>,
    ) -> Result<ContentPage<SeriesDto>> {
        self.query_series_with_regex(search, page, user, ranked_ids, regex)
            .await
    }

    async fn query_series_with_regex(
        &self,
        search: &SeriesSearch,
        page: &PageRequest,
        user: &User,
        ranked_ids: Option<&[String]>,
        regex: Option<&LegacySeriesRegex>,
    ) -> Result<ContentPage<SeriesDto>> {
        validate_page(page)?;
        reject_full_text(search.full_text_search.as_deref())?;
        validate_series_sorts(page, ranked_ids.is_some())?;

        let mut count = series_count_builder(user, ranked_ids);
        push_series_visibility(&mut count, user);
        push_series_regex(&mut count, regex);
        if let Some(condition) = &search.condition {
            count.push(" AND ");
            push_series_condition(&mut count, condition);
        }
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;
        let total = u64::try_from(total).map_err(|_| StorageError::InvalidData("negative series count".into()))?;

        let mut query = series_query_builder(user, ranked_ids);
        push_series_visibility(&mut query, user);
        push_series_regex(&mut query, regex);
        if let Some(condition) = &search.condition {
            query.push(" AND ");
            push_series_condition(&mut query, condition);
        }
        push_series_order(&mut query, page, ranked_ids.is_some());
        push_pagination(&mut query, page)?;
        let rows = query.build_query_as::<SeriesQueryRow>().fetch_all(&self.pool).await?;
        let content = rows.into_iter().map(SeriesQueryRow::into_dto).collect::<Result<_>>()?;

        Ok(content_page(content, total, page))
    }

    /// Counts visible series by the first lowercase character of their sort title.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error when full-text search has not been resolved
    /// to ranked IDs, or a database error when grouping fails.
    pub async fn series_alphabetical_groups(
        &self,
        search: &SeriesSearch,
        user: &User,
        ranked_ids: Option<&[String]>,
    ) -> Result<Vec<GroupCountDto>> {
        self.series_alphabetical_groups_with_regex(search, user, ranked_ids, None)
            .await
    }

    /// Counts visible legacy-regex matches by the first character of their sort title.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error when full-text search has not been resolved,
    /// or a database error when grouping or regex evaluation fails.
    pub async fn series_alphabetical_groups_legacy(
        &self,
        search: &SeriesSearch,
        user: &User,
        ranked_ids: Option<&[String]>,
        regex: Option<&LegacySeriesRegex>,
    ) -> Result<Vec<GroupCountDto>> {
        self.series_alphabetical_groups_with_regex(search, user, ranked_ids, regex)
            .await
    }

    async fn series_alphabetical_groups_with_regex(
        &self,
        search: &SeriesSearch,
        user: &User,
        ranked_ids: Option<&[String]>,
        regex: Option<&LegacySeriesRegex>,
    ) -> Result<Vec<GroupCountDto>> {
        reject_full_text(search.full_text_search.as_deref())?;
        let group = "lower(substr(sm.TITLE_SORT, 1, 1))";
        let mut query = QueryBuilder::new("SELECT ");
        query
            .push(group)
            .push(" AS group_name, COUNT(*) AS group_count FROM SERIES s ")
            .push("JOIN SERIES_METADATA sm ON sm.SERIES_ID = s.ID ")
            .push("JOIN BOOK_METADATA_AGGREGATION bma ON bma.SERIES_ID = s.ID ")
            .push("LEFT JOIN READ_PROGRESS_SERIES rps ON rps.SERIES_ID = s.ID AND rps.USER_ID = ")
            .push_bind(user.id.clone());
        push_ranked_join(&mut query, ranked_ids, "s.ID");
        query.push(" WHERE 1 = 1");
        push_series_visibility(&mut query, user);
        push_series_regex(&mut query, regex);
        if let Some(condition) = &search.condition {
            query.push(" AND ");
            push_series_condition(&mut query, condition);
        }
        query
            .push(" GROUP BY ")
            .push(group)
            .push(" ORDER BY ")
            .push(group)
            .push(" ASC");
        let rows = query.build_query_as::<(String, i64)>().fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|(group, count)| GroupCountDto { group, count })
            .collect())
    }

    /// Lists visible series that were modified after their creation.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for unsupported sort or paging values, or
    /// a database error when the query cannot be executed.
    pub async fn query_recently_updated_series(
        &self,
        search: &SeriesSearch,
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<SeriesDto>> {
        validate_page(page)?;
        reject_full_text(search.full_text_search.as_deref())?;
        validate_series_sorts(page, false)?;

        let mut count = series_count_builder(user, None);
        push_series_visibility(&mut count, user);
        count.push(" AND s.CREATED_DATE <> s.LAST_MODIFIED_DATE");
        if let Some(condition) = &search.condition {
            count.push(" AND ");
            push_series_condition(&mut count, condition);
        }
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;
        let total = u64::try_from(total).map_err(|_| StorageError::InvalidData("negative series count".into()))?;

        let mut query = series_query_builder(user, None);
        push_series_visibility(&mut query, user);
        query.push(" AND s.CREATED_DATE <> s.LAST_MODIFIED_DATE");
        if let Some(condition) = &search.condition {
            query.push(" AND ");
            push_series_condition(&mut query, condition);
        }
        push_series_order(&mut query, page, false);
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
        let mut query = series_query_builder_for_user_id(user_id, None);
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
        ranked_ids: Option<&[String]>,
    ) -> Result<ContentPage<BookDto>> {
        validate_page(page)?;
        reject_full_text(search.full_text_search.as_deref())?;
        validate_book_sorts(page, ranked_ids.is_some())?;

        let mut count = book_count_builder(user, ranked_ids);
        push_book_visibility(&mut count, user);
        if let Some(condition) = &search.condition {
            count.push(" AND ");
            push_book_condition(&mut count, condition);
        }
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;
        let total = u64::try_from(total).map_err(|_| StorageError::InvalidData("negative book count".into()))?;

        let mut query = book_query_builder(user, ranked_ids);
        push_book_visibility(&mut query, user);
        if let Some(condition) = &search.condition {
            query.push(" AND ");
            push_book_condition(&mut query, condition);
        }
        push_book_order(&mut query, page, ranked_ids.is_some());
        push_pagination(&mut query, page)?;
        let rows = query.build_query_as::<BookQueryRow>().fetch_all(&self.pool).await?;
        let content = rows.into_iter().map(BookQueryRow::into_dto).collect::<Result<_>>()?;

        Ok(content_page(content, total, page))
    }

    /// Lists the first unread book from each eligible on-deck series.
    ///
    /// # Errors
    ///
    /// Returns an invalid-query error for invalid paging values, or a database
    /// error when the candidate or hydration query fails.
    pub async fn query_books_on_deck(
        &self,
        library_ids: &[String],
        page: &PageRequest,
        user: &User,
    ) -> Result<ContentPage<BookDto>> {
        validate_page(page)?;
        let mut count = on_deck_builder(user, library_ids, true);
        let total = count.build_query_scalar::<i64>().fetch_one(&self.pool).await?;
        let total = u64::try_from(total).map_err(|_| StorageError::InvalidData("negative on-deck count".into()))?;

        let mut candidates = on_deck_builder(user, library_ids, false);
        candidates.push(" ORDER BY most_recent_read_date DESC, book_id ASC");
        push_pagination(&mut candidates, page)?;
        let ids = candidates.build_query_scalar::<String>().fetch_all(&self.pool).await?;
        let hydration = self
            .query_books(
                &BookSearch::default(),
                &PageRequest {
                    unpaged: true,
                    ..PageRequest::default()
                },
                user,
                Some(&ids),
            )
            .await?;
        Ok(content_page(hydration.content, total, page))
    }

    /// Finds a book by ID without applying user visibility rules.
    ///
    /// # Errors
    ///
    /// Returns an error when the row cannot be queried or hydrated.
    pub async fn find_book_dto_by_id(&self, id: &str, user_id: &str) -> Result<Option<BookDto>> {
        let mut query = book_query_builder_for_user_id(user_id, None);
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

fn on_deck_builder(user: &User, library_ids: &[String], count: bool) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(
        "WITH eligible_series AS MATERIALIZED (\
         SELECT s.ID AS series_id, rps.MOST_RECENT_READ_DATE AS most_recent_read_date \
         FROM SERIES s \
         JOIN SERIES_METADATA sm ON sm.SERIES_ID = s.ID \
         JOIN READ_PROGRESS_SERIES rps ON rps.SERIES_ID = s.ID AND rps.USER_ID = ",
    );
    query.push_bind(user.id.clone()).push(
        " WHERE rps.READ_COUNT > 0 \
         AND rps.IN_PROGRESS_COUNT = 0 AND rps.READ_COUNT <> s.BOOK_COUNT",
    );
    push_series_visibility(&mut query, user);
    if !library_ids.is_empty() {
        query.push(" AND s.LIBRARY_ID IN (");
        let mut separated = query.separated(",");
        for library_id in library_ids {
            separated.push_bind(library_id.clone());
        }
        query.push(")");
    }
    query
        .push(
            "), unread_books AS MATERIALIZED (\
         SELECT b.ID AS book_id, b.SERIES_ID AS series_id, bm.NUMBER_SORT AS number_sort, \
                eligible_series.most_recent_read_date AS most_recent_read_date \
         FROM BOOK b \
         JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID \
         JOIN eligible_series ON eligible_series.series_id = b.SERIES_ID \
         LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = ",
        )
        .push_bind(user.id.clone())
        .push(
            " WHERE rp.COMPLETED IS NULL\
         ), ranked_books AS (\
         SELECT book_id, most_recent_read_date, \
                ROW_NUMBER() OVER (PARTITION BY series_id ORDER BY number_sort ASC, book_id ASC) AS book_rank \
         FROM unread_books) SELECT ",
        );
    if count {
        query.push("COUNT(*)");
    } else {
        query.push("book_id");
    }
    query.push(" FROM ranked_books WHERE book_rank = 1");
    query
}

fn series_query_builder(user: &User, ranked_ids: Option<&[String]>) -> QueryBuilder<'static, Sqlite> {
    series_query_builder_for_user_id(&user.id, ranked_ids)
}

fn series_query_builder_for_user_id(user_id: &str, ranked_ids: Option<&[String]>) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(SERIES_SELECT);
    query.push_bind(user_id.to_owned());
    push_ranked_join(&mut query, ranked_ids, "s.ID");
    query.push(" WHERE 1 = 1");
    query
}

fn series_count_builder(user: &User, ranked_ids: Option<&[String]>) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(
        r#"SELECT COUNT(*)
             FROM SERIES s
             JOIN SERIES_METADATA sm ON sm.SERIES_ID = s.ID
             JOIN BOOK_METADATA_AGGREGATION bma ON bma.SERIES_ID = s.ID
             LEFT JOIN READ_PROGRESS_SERIES rps ON rps.SERIES_ID = s.ID AND rps.USER_ID = "#,
    );
    query.push_bind(user.id.clone());
    push_ranked_join(&mut query, ranked_ids, "s.ID");
    query.push(" WHERE 1 = 1");
    query
}

fn book_query_builder(user: &User, ranked_ids: Option<&[String]>) -> QueryBuilder<'static, Sqlite> {
    book_query_builder_for_user_id(&user.id, ranked_ids)
}

fn book_query_builder_for_user_id(user_id: &str, ranked_ids: Option<&[String]>) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(BOOK_SELECT);
    query.push_bind(user_id.to_owned());
    push_ranked_join(&mut query, ranked_ids, "b.ID");
    query.push(" WHERE 1 = 1");
    query
}

fn book_count_builder(user: &User, ranked_ids: Option<&[String]>) -> QueryBuilder<'static, Sqlite> {
    let mut query = QueryBuilder::new(
        r#"SELECT COUNT(*)
             FROM BOOK b
             JOIN MEDIA m ON m.BOOK_ID = b.ID
             JOIN BOOK_METADATA bm ON bm.BOOK_ID = b.ID
             JOIN SERIES_METADATA sm ON sm.SERIES_ID = b.SERIES_ID
             LEFT JOIN READ_PROGRESS rp ON rp.BOOK_ID = b.ID AND rp.USER_ID = "#,
    );
    query.push_bind(user.id.clone());
    push_ranked_join(&mut query, ranked_ids, "b.ID");
    query.push(" WHERE 1 = 1");
    query
}

fn push_ranked_join(query: &mut QueryBuilder<'static, Sqlite>, ranked_ids: Option<&[String]>, id_column: &str) {
    if let Some(ids) = ranked_ids {
        let json = serde_json::to_string(ids).expect("string identifiers always serialize");
        query
            .push(" JOIN json_each(")
            .push_bind(json)
            .push(") ranked ON ranked.value = ")
            .push(id_column);
    }
}

fn push_series_regex(query: &mut QueryBuilder<'static, Sqlite>, regex: Option<&LegacySeriesRegex>) {
    let Some(regex) = regex else {
        return;
    };
    let column = match regex.field {
        LegacySeriesRegexField::Title => "sm.TITLE",
        LegacySeriesRegexField::TitleSort => "sm.TITLE_SORT",
    };
    query
        .push(" AND ")
        .push(column)
        .push(" REGEXP ")
        .push_bind(format!("(?i:{})", regex.pattern));
}

fn push_series_condition(query: &mut QueryBuilder<'static, Sqlite>, condition: &SeriesCondition) {
    match condition {
        SeriesCondition::AllOf { conditions } => {
            push_condition_group(query, conditions, " AND ", push_series_condition);
        }
        SeriesCondition::AnyOf { conditions } => {
            push_condition_group(query, conditions, " OR ", push_series_condition);
        }
        SeriesCondition::LibraryId { operator } => push_string_equality(query, "s.LIBRARY_ID", operator, false),
        SeriesCondition::CollectionId { operator } => push_relation_equality(
            query,
            "COLLECTION_SERIES",
            "SERIES_ID",
            "s.ID",
            "COLLECTION_ID",
            operator,
        ),
        SeriesCondition::Deleted { operator } => push_optional_boolean(query, "s.DELETED_DATE", operator),
        SeriesCondition::Complete { operator } => {
            query.push("(sm.TOTAL_BOOK_COUNT IS NOT NULL AND sm.TOTAL_BOOK_COUNT");
            query.push(match operator {
                BooleanOperator::IsTrue => " = s.BOOK_COUNT)",
                BooleanOperator::IsFalse => " <> s.BOOK_COUNT)",
            });
        }
        SeriesCondition::OneShot { operator } => push_boolean(query, "s.ONESHOT", operator),
        SeriesCondition::Title { operator } => push_string_operator(query, "sm.TITLE", operator),
        SeriesCondition::TitleSort { operator } => push_string_operator(query, "sm.TITLE_SORT", operator),
        SeriesCondition::ReleaseDate { operator } => push_date_operator(query, "bma.RELEASE_DATE", operator),
        SeriesCondition::Tag { operator } => push_series_tag(query, operator),
        SeriesCondition::SharingLabel { operator } => {
            push_nullable_relation(query, "SERIES_METADATA_SHARING", "SERIES_ID", "s.ID", "LABEL", operator)
        }
        SeriesCondition::Publisher { operator } => push_string_equality(query, "sm.PUBLISHER", operator, true),
        SeriesCondition::Language { operator } => push_string_equality(query, "sm.LANGUAGE", operator, true),
        SeriesCondition::Genre { operator } => {
            push_nullable_relation(query, "SERIES_METADATA_GENRE", "SERIES_ID", "s.ID", "GENRE", operator)
        }
        SeriesCondition::AgeRating { operator } => push_nullable_numeric(query, "sm.AGE_RATING", operator),
        SeriesCondition::SeriesStatus { operator } => push_series_status(query, operator),
        SeriesCondition::ReadStatus { operator } => push_series_read_status(query, operator),
        SeriesCondition::Author { operator } => {
            push_author(query, "BOOK_METADATA_AGGREGATION_AUTHOR", "SERIES_ID", "s.ID", operator);
        }
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
        BookCondition::LibraryId { operator } => push_string_equality(query, "b.LIBRARY_ID", operator, false),
        BookCondition::SeriesId { operator } => push_string_equality(query, "b.SERIES_ID", operator, false),
        BookCondition::ReadListId { operator } => {
            push_relation_equality(query, "READLIST_BOOK", "BOOK_ID", "b.ID", "READLIST_ID", operator)
        }
        BookCondition::Deleted { operator } => push_optional_boolean(query, "b.DELETED_DATE", operator),
        BookCondition::OneShot { operator } => push_boolean(query, "b.ONESHOT", operator),
        BookCondition::Title { operator } => push_string_operator(query, "bm.TITLE", operator),
        BookCondition::ReleaseDate { operator } => push_date_operator(query, "bm.RELEASE_DATE", operator),
        BookCondition::Tag { operator } => {
            push_nullable_relation(query, "BOOK_METADATA_TAG", "BOOK_ID", "b.ID", "TAG", operator);
        }
        BookCondition::NumberSort { operator } => push_numeric(query, "bm.NUMBER_SORT", operator),
        BookCondition::MediaStatus { operator } => push_media_status(query, operator),
        BookCondition::MediaProfile { operator } => push_media_profile(query, operator),
        BookCondition::ReadStatus { operator } => push_book_read_status(query, operator),
        BookCondition::Author { operator } => {
            push_author(query, "BOOK_METADATA_AUTHOR", "BOOK_ID", "b.ID", operator);
        }
        BookCondition::Poster { operator } => push_poster(query, operator),
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

fn push_string_equality(
    query: &mut QueryBuilder<'static, Sqlite>,
    column: &str,
    operator: &EqualityOperator,
    unicode_ignore_case: bool,
) {
    query.push(column);
    if unicode_ignore_case {
        query.push(" COLLATE KOMGA_UNICODE_NOCASE");
    }
    match operator {
        EqualityOperator::Is { value } => {
            query.push(" = ").push_bind(value.clone());
        }
        EqualityOperator::IsNot { value } => {
            query.push(" <> ").push_bind(value.clone());
        }
    }
}

fn push_relation_equality(
    query: &mut QueryBuilder<'static, Sqlite>,
    table: &str,
    parent_column: &str,
    parent_id: &str,
    value_column: &str,
    operator: &EqualityOperator,
) {
    let (negation, value) = match operator {
        EqualityOperator::Is { value } => ("", value),
        EqualityOperator::IsNot { value } => ("NOT ", value),
    };
    query
        .push(negation)
        .push("EXISTS(SELECT 1 FROM ")
        .push(table)
        .push(" relation_match WHERE relation_match.")
        .push(parent_column)
        .push(" = ")
        .push(parent_id)
        .push(" AND relation_match.")
        .push(value_column)
        .push(" = ")
        .push_bind(value.clone())
        .push(")");
}

fn push_nullable_relation(
    query: &mut QueryBuilder<'static, Sqlite>,
    table: &str,
    parent_column: &str,
    parent_id: &str,
    value_column: &str,
    operator: &NullableEqualityOperator<String>,
) {
    let (negation, value) = match operator {
        NullableEqualityOperator::Is { value } => ("", Some(value)),
        NullableEqualityOperator::IsNot { value } => ("NOT ", Some(value)),
        NullableEqualityOperator::IsNull => ("NOT ", None),
        NullableEqualityOperator::IsNotNull => ("", None),
    };
    query
        .push(negation)
        .push("EXISTS(SELECT 1 FROM ")
        .push(table)
        .push(" relation_match WHERE relation_match.")
        .push(parent_column)
        .push(" = ")
        .push(parent_id);
    if let Some(value) = value {
        query
            .push(" AND relation_match.")
            .push(value_column)
            .push(" COLLATE KOMGA_UNICODE_NOCASE = ")
            .push_bind(value.clone());
    }
    query.push(")");
}

fn push_series_tag(query: &mut QueryBuilder<'static, Sqlite>, operator: &NullableEqualityOperator<String>) {
    let (negation, value) = match operator {
        NullableEqualityOperator::Is { value } => ("", Some(value)),
        NullableEqualityOperator::IsNot { value } => ("NOT ", Some(value)),
        NullableEqualityOperator::IsNull => ("NOT ", None),
        NullableEqualityOperator::IsNotNull => ("", None),
    };
    query.push(negation).push("(");
    for (index, table) in ["SERIES_METADATA_TAG", "BOOK_METADATA_AGGREGATION_TAG"]
        .iter()
        .enumerate()
    {
        if index > 0 {
            query.push(" OR ");
        }
        query
            .push("EXISTS(SELECT 1 FROM ")
            .push(*table)
            .push(" tag_match WHERE tag_match.SERIES_ID = s.ID");
        if let Some(value) = value {
            query
                .push(" AND tag_match.TAG COLLATE KOMGA_UNICODE_NOCASE = ")
                .push_bind(value.clone());
        }
        query.push(")");
    }
    query.push(")");
}

fn push_author(
    query: &mut QueryBuilder<'static, Sqlite>,
    table: &str,
    parent_column: &str,
    parent_id: &str,
    operator: &EqualityOperator<AuthorMatch>,
) {
    let (negation, value) = match operator {
        EqualityOperator::Is { value } => ("", value),
        EqualityOperator::IsNot { value } => ("NOT ", value),
    };
    if value.name.is_none() && value.role.is_none() {
        query.push("1 = 1");
        return;
    }
    query
        .push(negation)
        .push("EXISTS(SELECT 1 FROM ")
        .push(table)
        .push(" author_match WHERE author_match.")
        .push(parent_column)
        .push(" = ")
        .push(parent_id);
    if let Some(name) = &value.name {
        query
            .push(" AND author_match.NAME COLLATE KOMGA_UNICODE_NOCASE = ")
            .push_bind(name.clone());
    }
    if let Some(role) = &value.role {
        query
            .push(" AND author_match.ROLE COLLATE KOMGA_UNICODE_NOCASE = ")
            .push_bind(role.clone());
    }
    query.push(")");
}

fn push_poster(query: &mut QueryBuilder<'static, Sqlite>, operator: &EqualityOperator<PosterMatch>) {
    let (negation, value) = match operator {
        EqualityOperator::Is { value } => ("", value),
        EqualityOperator::IsNot { value } => ("NOT ", value),
    };
    if value.poster_type.is_none() && value.selected.is_none() {
        query.push("1 = 1");
        return;
    }
    query
        .push(negation)
        .push("EXISTS(SELECT 1 FROM THUMBNAIL_BOOK poster_match WHERE poster_match.BOOK_ID = b.ID");
    if let Some(poster_type) = value.poster_type {
        query.push(" AND poster_match.TYPE = ").push_bind(poster_type.as_str());
    }
    if let Some(selected) = value.selected {
        query.push(" AND poster_match.SELECTED = ").push_bind(selected);
    }
    query.push(")");
}

fn push_numeric(query: &mut QueryBuilder<'static, Sqlite>, column: &str, operator: &NumericOperator<f64>) {
    let (comparison, value) = match operator {
        NumericOperator::Is { value } => (" = ", value),
        NumericOperator::IsNot { value } => (" <> ", value),
        NumericOperator::GreaterThan { value } => (" > ", value),
        NumericOperator::LessThan { value } => (" < ", value),
    };
    query.push(column).push(comparison).push_bind(*value);
}

fn push_nullable_numeric(
    query: &mut QueryBuilder<'static, Sqlite>,
    column: &str,
    operator: &NullableNumericOperator<i32>,
) {
    match operator {
        NullableNumericOperator::Is { value } => {
            query.push(column).push(" = ").push_bind(*value);
        }
        NullableNumericOperator::IsNot { value } => {
            query
                .push("(")
                .push(column)
                .push(" <> ")
                .push_bind(*value)
                .push(" OR ")
                .push(column)
                .push(" IS NULL)");
        }
        NullableNumericOperator::GreaterThan { value } => {
            query.push(column).push(" > ").push_bind(*value);
        }
        NullableNumericOperator::LessThan { value } => {
            query.push(column).push(" < ").push_bind(*value);
        }
        NullableNumericOperator::IsNull => {
            query.push(column).push(" IS NULL");
        }
        NullableNumericOperator::IsNotNull => {
            query.push(column).push(" IS NOT NULL");
        }
    }
}

fn push_date_operator(query: &mut QueryBuilder<'static, Sqlite>, column: &str, operator: &DateOperator) {
    use chrono::Utc;

    match operator {
        DateOperator::Before { date_time } => {
            query
                .push(column)
                .push(" < ")
                .push_bind(date_time.with_timezone(&Utc).date_naive().to_string());
        }
        DateOperator::After { date_time } => {
            query
                .push(column)
                .push(" > ")
                .push_bind(date_time.with_timezone(&Utc).date_naive().to_string());
        }
        DateOperator::IsInTheLast { days } => {
            query
                .push(column)
                .push(" > date('now', ")
                .push_bind(format!("-{days} days"))
                .push(")");
        }
        DateOperator::IsNotInTheLast { days } => {
            query
                .push(column)
                .push(" < date('now', ")
                .push_bind(format!("-{days} days"))
                .push(")");
        }
        DateOperator::IsNull => {
            query.push(column).push(" IS NULL");
        }
        DateOperator::IsNotNull => {
            query.push(column).push(" IS NOT NULL");
        }
    }
}

fn push_series_status(query: &mut QueryBuilder<'static, Sqlite>, operator: &EqualityOperator<SeriesStatus>) {
    let (comparison, value) = match operator {
        EqualityOperator::Is { value } => (" = ", value),
        EqualityOperator::IsNot { value } => (" <> ", value),
    };
    query.push("sm.STATUS").push(comparison).push_bind(value.as_str());
}

fn push_media_status(query: &mut QueryBuilder<'static, Sqlite>, operator: &EqualityOperator<MediaStatus>) {
    let (comparison, value) = match operator {
        EqualityOperator::Is { value } => (" = ", value),
        EqualityOperator::IsNot { value } => (" <> ", value),
    };
    query.push("m.STATUS").push(comparison).push_bind(value.as_str());
}

fn push_media_profile(query: &mut QueryBuilder<'static, Sqlite>, operator: &EqualityOperator<MediaProfile>) {
    let (negation, profile) = match operator {
        EqualityOperator::Is { value } => ("", value),
        EqualityOperator::IsNot { value } => (" NOT", value),
    };
    let media_types: &[&str] = match profile {
        MediaProfile::Divina => &[ZIP_MEDIA_TYPE, RAR_MEDIA_TYPE, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE],
        MediaProfile::Pdf => &[PDF_MEDIA_TYPE],
        MediaProfile::Epub => &[EPUB_MEDIA_TYPE],
    };
    query.push("m.MEDIA_TYPE").push(negation).push(" IN (");
    for (index, media_type) in media_types.iter().enumerate() {
        if index > 0 {
            query.push(",");
        }
        query.push_bind(*media_type);
    }
    query.push(")");
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

fn push_series_read_status(query: &mut QueryBuilder<'static, Sqlite>, operator: &EqualityOperator<ReadStatus>) {
    let expression = match operator {
        EqualityOperator::Is {
            value: ReadStatus::Unread,
        } => "rps.READ_COUNT IS NULL",
        EqualityOperator::Is {
            value: ReadStatus::Read,
        } => "rps.READ_COUNT = s.BOOK_COUNT",
        EqualityOperator::Is {
            value: ReadStatus::InProgress,
        } => "rps.READ_COUNT <> s.BOOK_COUNT",
        EqualityOperator::IsNot {
            value: ReadStatus::Unread,
        } => "rps.READ_COUNT IS NOT NULL",
        EqualityOperator::IsNot {
            value: ReadStatus::Read,
        } => "(rps.READ_COUNT <> s.BOOK_COUNT OR rps.READ_COUNT IS NULL)",
        EqualityOperator::IsNot {
            value: ReadStatus::InProgress,
        } => "(rps.READ_COUNT = s.BOOK_COUNT OR rps.READ_COUNT IS NULL)",
    };
    query.push(expression);
}

fn push_book_read_status(query: &mut QueryBuilder<'static, Sqlite>, operator: &EqualityOperator<ReadStatus>) {
    let expression = match operator {
        EqualityOperator::Is {
            value: ReadStatus::Unread,
        } => "rp.COMPLETED IS NULL",
        EqualityOperator::Is {
            value: ReadStatus::Read,
        } => "rp.COMPLETED = 1",
        EqualityOperator::Is {
            value: ReadStatus::InProgress,
        } => "rp.COMPLETED = 0",
        EqualityOperator::IsNot {
            value: ReadStatus::Unread,
        } => "rp.COMPLETED IS NOT NULL",
        EqualityOperator::IsNot {
            value: ReadStatus::Read,
        } => "(rp.COMPLETED IS NULL OR rp.COMPLETED = 0)",
        EqualityOperator::IsNot {
            value: ReadStatus::InProgress,
        } => "(rp.COMPLETED IS NULL OR rp.COMPLETED = 1)",
    };
    query.push(expression);
}

fn reject_full_text(value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| !value.trim().is_empty()) {
        return Err(StorageError::InvalidQuery(
            "fullTextSearch is not available until the search phase".into(),
        ));
    }
    Ok(())
}

fn validate_series_sorts(page: &PageRequest, ranked: bool) -> Result<()> {
    for sort in &page.sort {
        if sort.property == "relevance" {
            if ranked {
                continue;
            }
            return Err(StorageError::InvalidQuery(
                "relevance sort requires a full-text search".into(),
            ));
        }
        series_sort_column(&sort.property).ok_or_else(|| {
            StorageError::InvalidQuery(format!("unsupported series sort property: {}", sort.property))
        })?;
    }
    Ok(())
}

fn validate_book_sorts(page: &PageRequest, ranked: bool) -> Result<()> {
    for sort in &page.sort {
        if sort.property == "relevance" {
            if ranked {
                continue;
            }
            return Err(StorageError::InvalidQuery(
                "relevance sort requires a full-text search".into(),
            ));
        }
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
        "random" => Some("random()"),
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

fn push_series_order(query: &mut QueryBuilder<'static, Sqlite>, page: &PageRequest, ranked: bool) {
    push_order(query, page, series_sort_column, "s.ID", ranked);
}

fn push_book_order(query: &mut QueryBuilder<'static, Sqlite>, page: &PageRequest, ranked: bool) {
    push_order(query, page, book_sort_column, "b.ID", ranked);
}

fn push_order(
    query: &mut QueryBuilder<'static, Sqlite>,
    page: &PageRequest,
    column: fn(&str) -> Option<&'static str>,
    id_column: &str,
    ranked: bool,
) {
    query.push(" ORDER BY ");
    let explicit = page
        .sort
        .iter()
        .filter(|order| order.property != "relevance")
        .collect::<Vec<_>>();
    for (index, order) in explicit.iter().enumerate() {
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
    if !explicit.is_empty() {
        query.push(", ");
    }
    if ranked {
        query.push("CAST(ranked.key AS INTEGER) ASC, ");
    }
    query.push(id_column).push(" ASC");
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
