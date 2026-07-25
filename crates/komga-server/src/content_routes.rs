use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
};
use komga_core::{BookDto, BookSearch, SeriesDto, SeriesSearch, User};
use komga_search::EntityKind;

use crate::{
    AppState,
    auth::AuthContext,
    content_access::{
        load_visible_book, load_visible_series, redact_book_source_for_user, redact_series_source_for_user,
    },
    error::ApiError,
    pagination::{SpringPage, parse_page_request},
    routes::ApiJson,
    search_query::{non_empty, search_hits},
};

pub(crate) async fn list_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
    ApiJson(search): ApiJson<SeriesSearch>,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    let page = parse_page_request(raw_query.as_deref())?;
    let mut result = if let Some(query) = non_empty(search.full_text_search.as_deref()) {
        let hits = search_hits(&state, EntityKind::Series, query).await;
        let mut database_search = search.clone();
        database_search.full_text_search = None;
        state
            .storage
            .query_series(&database_search, &page, &auth.user, Some(&hits))
            .await?
    } else {
        state.storage.query_series(&search, &page, &auth.user, None).await?
    };
    result
        .content
        .iter_mut()
        .for_each(|series| redact_series_source_for_user(series, &auth.user));
    Ok(Json(SpringPage::from_content(result)))
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
    let mut result = if let Some(query) = non_empty(search.full_text_search.as_deref()) {
        let hits = search_hits(&state, EntityKind::Book, query).await;
        let mut database_search = search.clone();
        database_search.full_text_search = None;
        state
            .storage
            .query_books(&database_search, &page, &auth.user, Some(&hits))
            .await?
    } else {
        state.storage.query_books(&search, &page, &auth.user, None).await?
    };
    result
        .content
        .iter_mut()
        .for_each(|book| redact_book_source_for_user(book, &auth.user));
    Ok(Json(SpringPage::from_content(result)))
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
