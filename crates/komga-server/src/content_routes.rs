use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
};
use komga_core::{
    BookDto, BookSearch, BooleanOperator, ContentPage, EqualityOperator, GroupCountDto, PageRequest, SeriesCondition,
    SeriesDto, SeriesSearch, SortDirection, SortOrder, User,
};
use komga_search::EntityKind;
use komga_storage::LegacySeriesRegex;

use crate::{
    AppState,
    auth::AuthContext,
    content_access::{
        load_visible_book, load_visible_series, redact_book_source_for_user, redact_series_source_for_user,
    },
    error::ApiError,
    legacy_content_query::{parse_legacy_book_query, parse_legacy_series_query},
    pagination::{ContentQueryParams, SpringPage, parse_content_query, parse_page_request},
    routes::ApiJson,
    search_query::{non_empty, search_hits},
};

pub(crate) async fn series_alphabetical_groups(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(search): ApiJson<SeriesSearch>,
) -> Result<Json<Vec<GroupCountDto>>, ApiError> {
    Ok(Json(execute_series_groups(&state, &auth.user, &search, None).await?))
}

pub(crate) async fn legacy_series_alphabetical_groups(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<Vec<GroupCountDto>>, ApiError> {
    let query = parse_legacy_series_query(raw_query.as_deref())?;
    Ok(Json(
        execute_series_groups(&state, &auth.user, &query.search, query.regex.as_ref()).await?,
    ))
}

async fn execute_series_groups(
    state: &AppState,
    user: &User,
    search: &SeriesSearch,
    regex: Option<&LegacySeriesRegex>,
) -> Result<Vec<GroupCountDto>, ApiError> {
    let groups = if let Some(query) = non_empty(search.full_text_search.as_deref()) {
        let hits = search_hits(state, EntityKind::Series, query).await;
        let mut database_search = search.clone();
        database_search.full_text_search = None;
        if regex.is_some() {
            state
                .storage
                .series_alphabetical_groups_legacy(&database_search, user, Some(&hits), regex)
                .await?
        } else {
            state
                .storage
                .series_alphabetical_groups(&database_search, user, Some(&hits))
                .await?
        }
    } else if regex.is_some() {
        state
            .storage
            .series_alphabetical_groups_legacy(search, user, None, regex)
            .await?
    } else {
        state.storage.series_alphabetical_groups(search, user, None).await?
    };
    Ok(groups)
}

pub(crate) async fn latest_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    series_discovery(&state, &auth.user, raw_query.as_deref(), "lastModified", false).await
}

pub(crate) async fn new_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    series_discovery(&state, &auth.user, raw_query.as_deref(), "created", false).await
}

pub(crate) async fn updated_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    series_discovery(&state, &auth.user, raw_query.as_deref(), "lastModified", true).await
}

async fn series_discovery(
    state: &AppState,
    user: &User,
    raw_query: Option<&str>,
    sort_property: &str,
    updated_only: bool,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    let params = parse_content_query(raw_query)?;
    let search = discovery_series_search(&params);
    let page = fixed_sort(params.page, sort_property);
    let mut result = if updated_only {
        state
            .storage
            .query_recently_updated_series(&search, &page, user)
            .await?
    } else {
        state.storage.query_series(&search, &page, user, None).await?
    };
    result
        .content
        .iter_mut()
        .for_each(|series| redact_series_source_for_user(series, user));
    Ok(Json(SpringPage::from_content(result)))
}

fn discovery_series_search(params: &ContentQueryParams) -> SeriesSearch {
    let mut conditions = Vec::new();
    if !params.library_ids.is_empty() {
        conditions.push(SeriesCondition::AnyOf {
            conditions: params
                .library_ids
                .iter()
                .map(|library_id| SeriesCondition::LibraryId {
                    operator: EqualityOperator::Is {
                        value: library_id.clone(),
                    },
                })
                .collect(),
        });
    }
    if let Some(deleted) = params.deleted {
        conditions.push(SeriesCondition::Deleted {
            operator: if deleted {
                BooleanOperator::IsTrue
            } else {
                BooleanOperator::IsFalse
            },
        });
    }
    if let Some(oneshot) = params.oneshot {
        conditions.push(SeriesCondition::OneShot {
            operator: if oneshot {
                BooleanOperator::IsTrue
            } else {
                BooleanOperator::IsFalse
            },
        });
    }
    SeriesSearch {
        condition: Some(SeriesCondition::AllOf { conditions }),
        full_text_search: None,
    }
}

fn fixed_sort(mut page: PageRequest, property: &str) -> PageRequest {
    page.sort = vec![SortOrder {
        property: property.to_owned(),
        direction: SortDirection::Descending,
    }];
    page
}

pub(crate) async fn latest_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<BookDto>>, ApiError> {
    let params = parse_content_query(raw_query.as_deref())?;
    let page = fixed_sort(params.page, "lastModifiedDate");
    let mut result = state
        .storage
        .query_books(&BookSearch::default(), &page, &auth.user, None)
        .await?;
    result
        .content
        .iter_mut()
        .for_each(|book| redact_book_source_for_user(book, &auth.user));
    Ok(Json(SpringPage::from_content(result)))
}

pub(crate) async fn books_on_deck(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<BookDto>>, ApiError> {
    let mut params = parse_content_query(raw_query.as_deref())?;
    params.page.sort.clear();
    let mut result = state
        .storage
        .query_books_on_deck(&params.library_ids, &params.page, &auth.user)
        .await?;
    result
        .content
        .iter_mut()
        .for_each(|book| redact_book_source_for_user(book, &auth.user));
    Ok(Json(SpringPage::from_content(result)))
}

pub(crate) async fn list_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
    ApiJson(search): ApiJson<SeriesSearch>,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    let page = parse_page_request(raw_query.as_deref())?;
    Ok(Json(SpringPage::from_content(
        execute_series_query(&state, &auth.user, &search, &page, None).await?,
    )))
}

pub(crate) async fn legacy_list_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    let query = parse_legacy_series_query(raw_query.as_deref())?;
    Ok(Json(SpringPage::from_content(
        execute_series_query(&state, &auth.user, &query.search, &query.page, query.regex.as_ref()).await?,
    )))
}

async fn execute_series_query(
    state: &AppState,
    user: &User,
    search: &SeriesSearch,
    page: &PageRequest,
    regex: Option<&LegacySeriesRegex>,
) -> Result<ContentPage<SeriesDto>, ApiError> {
    let mut result = if let Some(query) = non_empty(search.full_text_search.as_deref()) {
        let hits = search_hits(state, EntityKind::Series, query).await;
        let mut database_search = search.clone();
        database_search.full_text_search = None;
        if regex.is_some() {
            state
                .storage
                .query_series_legacy(&database_search, page, user, Some(&hits), regex)
                .await?
        } else {
            state
                .storage
                .query_series(&database_search, page, user, Some(&hits))
                .await?
        }
    } else if regex.is_some() {
        state
            .storage
            .query_series_legacy(search, page, user, None, regex)
            .await?
    } else {
        state.storage.query_series(search, page, user, None).await?
    };
    result
        .content
        .iter_mut()
        .for_each(|series| redact_series_source_for_user(series, user));
    Ok(result)
}

pub(crate) async fn get_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<Json<SeriesDto>, ApiError> {
    let mut series = load_visible_series(&state, &auth.user, &series_id).await?;
    redact_series_source_for_user(&mut series, &auth.user);
    Ok(Json(series))
}

pub(crate) async fn list_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
    ApiJson(search): ApiJson<BookSearch>,
) -> Result<Json<SpringPage<BookDto>>, ApiError> {
    let page = parse_page_request(raw_query.as_deref())?;
    Ok(Json(SpringPage::from_content(
        execute_book_query(&state, &auth.user, &search, &page).await?,
    )))
}

pub(crate) async fn legacy_list_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<SpringPage<BookDto>>, ApiError> {
    let query = parse_legacy_book_query(raw_query.as_deref())?;
    Ok(Json(SpringPage::from_content(
        execute_book_query(&state, &auth.user, &query.search, &query.page).await?,
    )))
}

async fn execute_book_query(
    state: &AppState,
    user: &User,
    search: &BookSearch,
    page: &PageRequest,
) -> Result<ContentPage<BookDto>, ApiError> {
    let mut result = if let Some(query) = non_empty(search.full_text_search.as_deref()) {
        let hits = search_hits(state, EntityKind::Book, query).await;
        let mut database_search = search.clone();
        database_search.full_text_search = None;
        state
            .storage
            .query_books(&database_search, page, user, Some(&hits))
            .await?
    } else {
        state.storage.query_books(search, page, user, None).await?
    };
    result
        .content
        .iter_mut()
        .for_each(|book| redact_book_source_for_user(book, user));
    Ok(result)
}

pub(crate) async fn get_book(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Json<BookDto>, ApiError> {
    let mut book = load_visible_book(&state, &auth.user, &book_id).await?;
    redact_book_source_for_user(&mut book, &auth.user);
    Ok(Json(book))
}

pub(crate) async fn get_previous_book(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Json<BookDto>, ApiError> {
    get_sibling(&state, &auth.user, &book_id, false).await.map(Json)
}

pub(crate) async fn get_next_book(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Json<BookDto>, ApiError> {
    get_sibling(&state, &auth.user, &book_id, true).await.map(Json)
}

async fn get_sibling(state: &AppState, user: &User, book_id: &str, next: bool) -> Result<BookDto, ApiError> {
    load_visible_book(state, user, book_id).await?;
    let mut sibling = state
        .storage
        .find_book_sibling_dto(book_id, &user.id, next)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    redact_book_source_for_user(&mut sibling, user);
    Ok(sibling)
}
