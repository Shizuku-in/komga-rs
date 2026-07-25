use komga_core::{AllowExclude, ContentPage, PageRequest, User};
use sqlx::{QueryBuilder, Sqlite};

use crate::{Result, StorageError};

pub(crate) fn push_series_visibility(query: &mut QueryBuilder<'static, Sqlite>, user: &User) {
    push_library_visibility(query, "s.LIBRARY_ID", user);
    push_content_visibility(query, "s.ID", user);
}

pub(crate) fn push_book_visibility(query: &mut QueryBuilder<'static, Sqlite>, user: &User) {
    push_library_visibility(query, "b.LIBRARY_ID", user);
    push_content_visibility(query, "b.SERIES_ID", user);
}

pub(crate) fn push_library_visibility(query: &mut QueryBuilder<'static, Sqlite>, column: &str, user: &User) {
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

pub(crate) fn push_content_visibility(query: &mut QueryBuilder<'static, Sqlite>, series_id: &str, user: &User) {
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

pub(crate) fn validate_page(page: &PageRequest) -> Result<()> {
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

pub(crate) fn push_pagination(query: &mut QueryBuilder<'static, Sqlite>, page: &PageRequest) -> Result<()> {
    if page.unpaged {
        return Ok(());
    }
    let size = i64::try_from(page.size).map_err(|_| StorageError::InvalidQuery("page size is too large".into()))?;
    let offset = i64::try_from(page.page * page.size)
        .map_err(|_| StorageError::InvalidQuery("page offset is too large".into()))?;
    query.push(" LIMIT ").push_bind(size).push(" OFFSET ").push_bind(offset);
    Ok(())
}

pub(crate) fn content_page<T>(content: Vec<T>, total_elements: u64, request: &PageRequest) -> ContentPage<T> {
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
