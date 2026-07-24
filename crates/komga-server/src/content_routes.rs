use std::path::Path as FilePath;

use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
};
use komga_core::{
    BookDto, BookSearch, ContentPage, PageRequest, SeriesDto, SeriesSearch, SortDirection, SortOrder, User,
};
use serde::Serialize;

use crate::{AppState, auth::AuthContext, error::ApiError, routes::ApiJson};

pub(crate) async fn list_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
    ApiJson(search): ApiJson<SeriesSearch>,
) -> Result<Json<SpringPage<SeriesDto>>, ApiError> {
    let page = parse_page_request(raw_query.as_deref())?;
    let mut result = state.storage.query_series(&search, &page, &auth.user).await?;
    if !auth.user.is_admin() {
        result.content.iter_mut().for_each(|series| series.url.clear());
    }
    Ok(Json(SpringPage::from_content(result)))
}

pub(crate) async fn get_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<Json<SeriesDto>, ApiError> {
    let mut series = state
        .storage
        .find_series_dto_by_id(&series_id, &auth.user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    ensure_series_visible(&auth.user, &series)?;
    if !auth.user.is_admin() {
        series.url.clear();
    }
    Ok(Json(series))
}

pub(crate) async fn list_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw_query): RawQuery,
    ApiJson(search): ApiJson<BookSearch>,
) -> Result<Json<SpringPage<BookDto>>, ApiError> {
    let page = parse_page_request(raw_query.as_deref())?;
    let mut result = state.storage.query_books(&search, &page, &auth.user).await?;
    if !auth.user.is_admin() {
        result.content.iter_mut().for_each(restrict_book_url);
    }
    Ok(Json(SpringPage::from_content(result)))
}

pub(crate) async fn get_book(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Json<BookDto>, ApiError> {
    let mut book = load_visible_book(&state, &auth.user, &book_id).await?;
    if !auth.user.is_admin() {
        restrict_book_url(&mut book);
    }
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
    if !user.is_admin() {
        restrict_book_url(&mut sibling);
    }
    Ok(sibling)
}

/// Loads a book and rejects users who cannot access its containing series.
pub(crate) async fn load_visible_book(state: &AppState, user: &User, book_id: &str) -> Result<BookDto, ApiError> {
    load_visible_book_context(state, user, book_id)
        .await
        .map(|(book, _)| book)
}

/// Loads a book with its series and applies library, age, and label restrictions.
///
/// Missing rows remain `404`, while an existing but restricted series returns
/// `403`; callers should use this helper before exposing media-derived details.
pub(crate) async fn load_visible_book_context(
    state: &AppState,
    user: &User,
    book_id: &str,
) -> Result<(BookDto, SeriesDto), ApiError> {
    let book = state
        .storage
        .find_book_dto_by_id(book_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let series = state
        .storage
        .find_series_dto_by_id(&book.series_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Series not found"))?;
    ensure_series_visible(user, &series)?;
    Ok((book, series))
}

fn ensure_series_visible(user: &User, series: &SeriesDto) -> Result<(), ApiError> {
    let age_rating = series.metadata.age_rating.and_then(|value| u32::try_from(value).ok());
    if !user.can_access_library(&series.library_id)
        || !user.is_content_allowed(age_rating, &series.metadata.sharing_labels)
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

fn restrict_book_url(book: &mut BookDto) {
    book.url = FilePath::new(&book.url)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
}

fn parse_page_request(raw_query: Option<&str>) -> Result<PageRequest, ApiError> {
    let mut request = PageRequest::default();
    for (key, value) in url::form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "page" => {
                request.page = value
                    .parse()
                    .map_err(|_| ApiError::bad_request("page must be a non-negative integer"))?;
            }
            "size" => {
                request.size = value
                    .parse()
                    .map_err(|_| ApiError::bad_request("size must be a non-negative integer"))?;
            }
            "unpaged" => {
                request.unpaged = parse_boolean(&value, "unpaged")?;
            }
            "sort" => request.sort.push(parse_sort(&value)?),
            _ => {}
        }
    }
    if !request.unpaged && request.size == 0 {
        return Err(ApiError::bad_request("size must be greater than zero"));
    }
    Ok(request)
}

fn parse_boolean(value: &str, field: &str) -> Result<bool, ApiError> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ApiError::bad_request(format!("{field} must be true or false"))),
    }
}

fn parse_sort(value: &str) -> Result<SortOrder, ApiError> {
    let mut segments = value.split(',');
    let property = segments.next().unwrap_or_default().trim();
    if property.is_empty() {
        return Err(ApiError::bad_request("sort property must not be blank"));
    }
    let direction = match segments.next().unwrap_or("asc").trim().to_ascii_lowercase().as_str() {
        "asc" => SortDirection::Ascending,
        "desc" => SortDirection::Descending,
        _ => return Err(ApiError::bad_request("sort direction must be asc or desc")),
    };
    if segments.next().is_some() {
        return Err(ApiError::bad_request(
            "sort must contain a property and optional direction",
        ));
    }
    Ok(SortOrder {
        property: property.to_owned(),
        direction,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SpringPage<T> {
    content: Vec<T>,
    pageable: PageableDto,
    last: bool,
    total_pages: u64,
    total_elements: u64,
    size: u64,
    number: u64,
    sort: SortDto,
    first: bool,
    number_of_elements: usize,
    empty: bool,
}

impl<T> SpringPage<T> {
    fn from_content(page: ContentPage<T>) -> Self {
        let sorted = !page.sort.is_empty();
        let total_pages = page.total_elements.div_ceil(page.size);
        let number_of_elements = page.content.len();
        Self {
            content: page.content,
            pageable: PageableDto {
                sort: SortDto::new(sorted),
                offset: page.page.saturating_mul(page.size),
                page_number: page.page,
                page_size: page.size,
                unpaged: false,
                paged: true,
            },
            last: total_pages == 0 || page.page + 1 >= total_pages,
            total_pages,
            total_elements: page.total_elements,
            size: page.size,
            number: page.page,
            sort: SortDto::new(sorted),
            first: page.page == 0,
            number_of_elements,
            empty: number_of_elements == 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageableDto {
    sort: SortDto,
    offset: u64,
    page_number: u64,
    page_size: u64,
    unpaged: bool,
    paged: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct SortDto {
    empty: bool,
    sorted: bool,
    unsorted: bool,
}

impl SortDto {
    fn new(sorted: bool) -> Self {
        Self {
            empty: !sorted,
            sorted,
            unsorted: !sorted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repeated_spring_sort_parameters() {
        let request = parse_page_request(Some("page=2&size=10&sort=metadata.title%2Cdesc&sort=name")).unwrap();
        assert_eq!(request.page, 2);
        assert_eq!(request.size, 10);
        assert_eq!(request.sort.len(), 2);
        assert_eq!(request.sort[0].direction, SortDirection::Descending);
        assert_eq!(request.sort[1].direction, SortDirection::Ascending);
    }
}
