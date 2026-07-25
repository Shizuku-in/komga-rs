use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use komga_core::{HashedPageTarget, PageHashAction, PageHashMatch, PageRequest, SortDirection, SortOrder};
use komga_media::{ImageOutput, convert_image, extract_page};
use komga_storage::TaskRecord;
use serde::Deserialize;

use crate::{
    AppState,
    auth::AuthContext,
    error::ApiError,
    pagination::SpringPage,
    routes::{ApiJson, require_admin},
};

const KNOWN_SORTS: &[&str] = &[
    "hash",
    "matchCount",
    "deleteCount",
    "deleteSize",
    "fileSize",
    "createdDate",
    "lastModifiedDate",
];
const UNKNOWN_SORTS: &[&str] = &["hash", "fileSize", "matchCount", "totalSize"];
const MATCH_SORTS: &[&str] = &["hash", "fileSize", "url", "bookId", "pageNumber"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageHashCreationDto {
    hash: String,
    size: Option<i64>,
    action: PageHashAction,
}

pub(crate) async fn known(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<komga_core::PageHashKnown>>, ApiError> {
    require_admin(&auth)?;
    let (page, actions) = parse_query(raw.as_deref(), KNOWN_SORTS, true)?;
    Ok(Json(SpringPage::from_content(
        state.storage.list_known_page_hashes(&actions, &page).await?,
    )))
}

pub(crate) async fn unknown(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<komga_core::PageHashUnknown>>, ApiError> {
    require_admin(&auth)?;
    let (page, _) = parse_query(raw.as_deref(), UNKNOWN_SORTS, false)?;
    Ok(Json(SpringPage::from_content(
        state.storage.list_unknown_page_hashes(&page).await?,
    )))
}

pub(crate) async fn matches(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(page_hash): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<PageHashMatch>>, ApiError> {
    require_admin(&auth)?;
    let (page, _) = parse_query(raw.as_deref(), MATCH_SORTS, false)?;
    Ok(Json(SpringPage::from_content(
        state.storage.list_page_hash_matches(&page_hash, &page).await?,
    )))
}

pub(crate) async fn known_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(page_hash): Path<String>,
) -> Result<Response, ApiError> {
    require_admin(&auth)?;
    let bytes = state
        .storage
        .page_hash_thumbnail(&page_hash)
        .await?
        .ok_or_else(|| ApiError::not_found("Page hash thumbnail not found"))?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response())
}

pub(crate) async fn unknown_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(page_hash): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ApiError> {
    require_admin(&auth)?;
    let resize = parse_resize(raw.as_deref())?;
    let bytes = preview(&state, &page_hash, resize)
        .await?
        .ok_or_else(|| ApiError::not_found("Page hash match not found"))?;
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], bytes).into_response())
}

pub(crate) async fn create_or_update(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(payload): ApiJson<PageHashCreationDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if payload.hash.trim().is_empty() {
        return Err(ApiError::bad_request("hash must not be blank"));
    }
    let size = payload.size.filter(|value| *value >= 0);
    let thumbnail = if state.storage.find_known_page_hash(&payload.hash).await?.is_none() {
        preview(&state, &payload.hash, Some(500)).await?
    } else {
        None
    };
    state
        .storage
        .create_or_update_page_hash(&payload.hash, size, payload.action, thumbnail.as_deref())
        .await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn delete_all(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(page_hash): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let matches = state
        .storage
        .list_page_hash_matches(
            &page_hash,
            &PageRequest {
                unpaged: true,
                ..PageRequest::default()
            },
        )
        .await?;
    let mut grouped = BTreeMap::<String, Vec<HashedPageTarget>>::new();
    for matched in matches.content {
        grouped
            .entry(matched.book_id.clone())
            .or_default()
            .push(target(&page_hash, matched));
    }
    for (book_id, pages) in grouped {
        state
            .enqueue_task(&TaskRecord::remove_hashed_pages(&book_id, &pages, 4))
            .await?;
    }
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn delete_match(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(page_hash): Path<String>,
    ApiJson(matched): ApiJson<PageHashMatch>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let book_id = matched.book_id.clone();
    state
        .enqueue_task(&TaskRecord::remove_hashed_pages(
            &book_id,
            &[target(&page_hash, matched)],
            4,
        ))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

fn target(page_hash: &str, matched: PageHashMatch) -> HashedPageTarget {
    HashedPageTarget {
        file_name: matched.file_name,
        media_type: matched.media_type,
        file_hash: page_hash.to_owned(),
        file_size: matched.file_size,
        page_number: matched.page_number,
    }
}

async fn preview(state: &AppState, page_hash: &str, max_edge: Option<u32>) -> Result<Option<Vec<u8>>, ApiError> {
    let matched = state
        .storage
        .list_page_hash_matches(
            page_hash,
            &PageRequest {
                size: 1,
                ..PageRequest::default()
            },
        )
        .await?
        .content
        .into_iter()
        .next();
    let Some(matched) = matched else {
        return Ok(None);
    };
    let book = state
        .storage
        .find_book_by_id(&matched.book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Page hash match not found"))?;
    let media = state
        .storage
        .find_media_by_book_id(&matched.book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Page hash match not found"))?;
    let bytes = tokio::task::spawn_blocking(move || {
        let page = extract_page(&book, &media, matched.page_number)?;
        convert_image(&page.bytes, ImageOutput::Jpeg, max_edge).map(|value| value.bytes)
    })
    .await
    .map_err(|error| ApiError::internal(format!("Page hash preview task failed: {error}")))?
    .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok(Some(bytes))
}

fn parse_resize(raw: Option<&str>) -> Result<Option<u32>, ApiError> {
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if key == "resize" {
            let resize = value
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| ApiError::bad_request("resize must be a positive integer"))?;
            return Ok(Some(resize));
        }
    }
    Ok(None)
}

fn parse_query(
    raw: Option<&str>,
    allowed_sorts: &[&str],
    parse_actions: bool,
) -> Result<(PageRequest, Vec<PageHashAction>), ApiError> {
    let mut page = 0;
    let mut size = 20;
    let mut unpaged = false;
    let mut sort = Vec::new();
    let mut actions = Vec::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "page" => page = value.parse().unwrap_or(0),
            "size" => {
                size = value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .unwrap_or(20)
                    .min(2_000)
            }
            "unpaged" => {
                unpaged = match value.to_ascii_lowercase().as_str() {
                    "true" | "on" | "yes" | "1" => true,
                    "false" | "off" | "no" | "0" => false,
                    _ => return Err(ApiError::bad_request("invalid boolean value for unpaged")),
                }
            }
            "sort" => {
                if let Some(order) = parse_sort(&value)
                    && allowed_sorts.contains(&order.property.as_str())
                {
                    sort.push(order);
                }
            }
            "action" if parse_actions => {
                for action in value.split(',') {
                    actions.push(
                        PageHashAction::try_from(action.trim())
                            .map_err(|()| ApiError::bad_request(format!("invalid page hash action: {action}")))?,
                    );
                }
            }
            _ => {}
        }
    }
    Ok((
        PageRequest {
            page,
            size,
            unpaged,
            sort,
        },
        actions,
    ))
}

fn parse_sort(value: &str) -> Option<SortOrder> {
    let mut segments = value.split(',');
    let property = segments.next()?.trim();
    if property.is_empty() {
        return None;
    }
    let direction = match segments.next().unwrap_or("asc").trim().to_ascii_lowercase().as_str() {
        "asc" => SortDirection::Ascending,
        "desc" => SortDirection::Descending,
        _ => return None,
    };
    if segments.next().is_some() {
        return None;
    }
    Some(SortOrder {
        property: property.to_owned(),
        direction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parser_caps_pages_and_accepts_repeated_or_comma_actions() {
        let (page, actions) = parse_query(
            Some("page=bad&size=9999&action=DELETE_AUTO,IGNORE&action=DELETE_MANUAL&sort=unknown&sort=matchCount,desc"),
            KNOWN_SORTS,
            true,
        )
        .unwrap();
        assert_eq!(page.page, 0);
        assert_eq!(page.size, 2_000);
        assert_eq!(page.sort.len(), 1);
        assert_eq!(actions.len(), 3);
    }
}
