//! Administrator historical-event query API.

use axum::{
    Json,
    extract::{Extension, RawQuery, State},
};
use komga_core::{HistoricalEvent, PageRequest, SortDirection, SortOrder};

use crate::{AppState, auth::AuthContext, error::ApiError, pagination::SpringPage, routes::require_admin};

const ALLOWED_SORTS: &[&str] = &["type", "bookId", "seriesId", "timestamp"];

pub(crate) async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<HistoricalEvent>>, ApiError> {
    require_admin(&auth)?;
    let page = parse_query(raw.as_deref())?;
    Ok(Json(SpringPage::from_content(
        state.storage.list_historical_events(&page).await?,
    )))
}

fn parse_query(raw: Option<&str>) -> Result<PageRequest, ApiError> {
    let mut page = 0;
    let mut size = 20;
    let mut unpaged = false;
    let mut sort = Vec::new();
    let mut had_sort = false;
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
                had_sort = true;
                if let Some(order) = parse_sort(&value)
                    && ALLOWED_SORTS.contains(&order.property.as_str())
                {
                    sort.push(order);
                }
            }
            _ => {}
        }
    }
    if !had_sort {
        sort.push(SortOrder {
            property: "timestamp".into(),
            direction: SortDirection::Descending,
        });
    }
    Ok(PageRequest {
        page,
        size,
        unpaged,
        sort,
    })
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
    fn history_query_defaults_filters_and_clamps() {
        let default = parse_query(None).unwrap();
        assert_eq!(default.sort[0].property, "timestamp");
        assert_eq!(default.sort[0].direction, SortDirection::Descending);

        let parsed = parse_query(Some("page=bad&size=9999&sort=unknown&sort=type,asc&ignored=x")).unwrap();
        assert_eq!(parsed.page, 0);
        assert_eq!(parsed.size, 2_000);
        assert_eq!(parsed.sort.len(), 1);
        assert_eq!(parsed.sort[0].property, "type");

        assert!(parse_query(Some("unpaged=maybe")).is_err());
    }
}
