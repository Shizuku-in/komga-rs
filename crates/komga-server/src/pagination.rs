use komga_core::{ContentPage, PageRequest, SortDirection, SortOrder};
use serde::Serialize;

use crate::error::ApiError;

pub(crate) fn parse_page_request(raw_query: Option<&str>) -> Result<PageRequest, ApiError> {
    Ok(parse_content_query(raw_query)?.page)
}

#[derive(Debug)]
pub(crate) struct ContentQueryParams {
    pub(crate) page: PageRequest,
    pub(crate) library_ids: Vec<String>,
    pub(crate) deleted: Option<bool>,
    pub(crate) oneshot: Option<bool>,
}

pub(crate) fn parse_content_query(raw_query: Option<&str>) -> Result<ContentQueryParams, ApiError> {
    let mut request = PageRequest::default();
    let mut library_ids = Vec::new();
    let mut deleted = None;
    let mut oneshot = None;
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
            "unpaged" => request.unpaged = parse_boolean(&value, "unpaged")?,
            "sort" => request.sort.push(parse_sort(&value)?),
            "library_id" => library_ids.push(value.into_owned()),
            "deleted" => deleted = Some(parse_boolean(&value, "deleted")?),
            "oneshot" => oneshot = Some(parse_boolean(&value, "oneshot")?),
            _ => {}
        }
    }
    if !request.unpaged && request.size == 0 {
        return Err(ApiError::bad_request("size must be greater than zero"));
    }
    Ok(ContentQueryParams {
        page: request,
        library_ids,
        deleted,
        oneshot,
    })
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
    pub(crate) fn from_content(page: ContentPage<T>) -> Self {
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

    #[test]
    fn parses_discovery_filters_and_rejects_loose_booleans() {
        let query = parse_content_query(Some(
            "library_id=one&unknown=x&library_id=two&deleted=false&oneshot=true",
        ))
        .unwrap();
        assert_eq!(query.library_ids, ["one", "two"]);
        assert_eq!(query.deleted, Some(false));
        assert_eq!(query.oneshot, Some(true));
        assert!(parse_content_query(Some("deleted=1")).is_err());
    }
}
