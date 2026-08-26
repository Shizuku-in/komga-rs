use axum::{
    Json,
    extract::{Extension, RawQuery, State},
};
use komga_core::{AuthorDto, ContentPage};
use komga_storage::{ReferentialFilter, ReferentialScope, ReferentialTagSource};

use crate::{AppState, auth::AuthContext, error::ApiError, pagination::SpringPage, pagination::parse_page_request};

#[derive(Debug, Default)]
struct ReferentialQuery {
    search: Option<String>,
    role: Option<String>,
    library_ids: Vec<String>,
    collection_ids: Vec<String>,
    series_ids: Vec<String>,
    read_list_ids: Vec<String>,
    tag_source: Option<ReferentialTagSource>,
    invalid_tag_source: bool,
}

fn parse_query(raw: Option<&str>) -> ReferentialQuery {
    let mut query = ReferentialQuery::default();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "search" => query.search = Some(value.into_owned()),
            "role" => query.role = Some(value.into_owned()),
            "library_id" => query.library_ids.push(value.into_owned()),
            "collection_id" => query.collection_ids.push(value.into_owned()),
            "series_id" => query.series_ids.push(value.into_owned()),
            "readlist_id" => query.read_list_ids.push(value.into_owned()),
            "include" => {
                query.tag_source = match value.as_ref() {
                    "SERIES" => Some(ReferentialTagSource::Series),
                    "BOOK" => Some(ReferentialTagSource::Book),
                    "BOTH" => Some(ReferentialTagSource::Both),
                    _ => {
                        query.invalid_tag_source = true;
                        None
                    }
                }
            }
            _ => {}
        }
    }
    query
}

fn author_scope(query: &ReferentialQuery) -> ReferentialScope {
    if !query.library_ids.is_empty() {
        ReferentialScope::Libraries(query.library_ids.clone())
    } else if let Some(id) = query.collection_ids.first() {
        ReferentialScope::Collection(id.clone())
    } else if let Some(id) = query.series_ids.first() {
        ReferentialScope::Series(id.clone())
    } else if let Some(id) = query.read_list_ids.first() {
        ReferentialScope::ReadList(id.clone())
    } else {
        ReferentialScope::Global
    }
}

fn series_scope(query: &ReferentialQuery) -> ReferentialScope {
    if !query.library_ids.is_empty() {
        ReferentialScope::Libraries(query.library_ids.clone())
    } else if let Some(id) = query.collection_ids.first() {
        ReferentialScope::Collection(id.clone())
    } else {
        ReferentialScope::Global
    }
}

fn book_tag_scope(query: &ReferentialQuery) -> ReferentialScope {
    if let Some(id) = query.series_ids.first() {
        ReferentialScope::Series(id.clone())
    } else if let Some(id) = query.read_list_ids.first() {
        ReferentialScope::ReadList(id.clone())
    } else if !query.library_ids.is_empty() {
        ReferentialScope::Libraries(query.library_ids.clone())
    } else {
        ReferentialScope::Global
    }
}

fn v2_filter(query: &ReferentialQuery) -> ReferentialFilter {
    if !query.library_ids.is_empty() {
        ReferentialFilter::Libraries(query.library_ids.clone())
    } else if !query.collection_ids.is_empty() {
        ReferentialFilter::Collections(query.collection_ids.clone())
    } else if !query.series_ids.is_empty() {
        ReferentialFilter::Series(query.series_ids.clone())
    } else if !query.read_list_ids.is_empty() {
        ReferentialFilter::ReadLists(query.read_list_ids.clone())
    } else {
        ReferentialFilter::Global
    }
}

fn v2_series_filter(query: &ReferentialQuery) -> ReferentialFilter {
    if !query.library_ids.is_empty() {
        ReferentialFilter::Libraries(query.library_ids.clone())
    } else if !query.collection_ids.is_empty() {
        ReferentialFilter::Collections(query.collection_ids.clone())
    } else {
        ReferentialFilter::Global
    }
}

fn spring_page<T>(page: ContentPage<T>) -> Json<SpringPage<T>> {
    Json(SpringPage::from_content(page))
}

pub(crate) async fn authors_v1(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<AuthorDto>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_v1_authors(
                &author_scope(&query),
                query.search.as_deref().unwrap_or_default(),
                &auth.user,
            )
            .await?,
    ))
}

pub(crate) async fn authors_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<AuthorDto>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = parse_page_request(raw.as_deref())?;
    let authors = state
        .storage
        .referential_v2_authors(
            &v2_filter(&query),
            query.search.as_deref(),
            query.role.as_deref(),
            &page,
            &auth.user,
        )
        .await?;
    Ok(spring_page(authors))
}

pub(crate) async fn author_names_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_author_names(
            &v2_filter(&query),
            query.search.as_deref(),
            query.role.as_deref(),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn author_roles_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_author_roles(&v2_filter(&query), &parse_page_request(raw.as_deref())?, &auth.user)
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn genres_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_genres(
            &v2_series_filter(&query),
            query.search.as_deref(),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn sharing_labels_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_sharing_labels(
            &v2_series_filter(&query),
            query.search.as_deref(),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn languages_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_languages(
            &v2_series_filter(&query),
            query.search.as_deref(),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn publishers_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_publishers(
            &v2_series_filter(&query),
            query.search.as_deref(),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn tags_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    if query.invalid_tag_source {
        return Err(ApiError::bad_request("include must be SERIES, BOOK, or BOTH"));
    }
    let page = state
        .storage
        .referential_v2_tags(
            &v2_filter(&query),
            query.search.as_deref(),
            query.tag_source.unwrap_or(ReferentialTagSource::Both),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn series_release_years_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_release_years(
            &v2_series_filter(&query),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn age_ratings_v2(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<i32>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let page = state
        .storage
        .referential_v2_age_ratings(
            &v2_series_filter(&query),
            &parse_page_request(raw.as_deref())?,
            &auth.user,
        )
        .await?;
    Ok(spring_page(page))
}

pub(crate) async fn author_names(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_author_names(query.search.as_deref().unwrap_or_default(), &auth.user)
            .await?,
    ))
}

pub(crate) async fn author_roles(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Vec<String>>, ApiError> {
    Ok(Json(state.storage.referential_author_roles(&auth.user).await?))
}

pub(crate) async fn genres(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_genres(&series_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn sharing_labels(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_sharing_labels(&series_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn tags(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_tags(&series_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn book_tags(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_book_tags(&book_tag_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn series_tags(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_series_tags(&series_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn languages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_languages(&series_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn publishers(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_publishers(&series_scope(&query), &auth.user)
            .await?,
    ))
}

pub(crate) async fn age_ratings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    let values = state
        .storage
        .referential_age_ratings(&series_scope(&query), &auth.user)
        .await?
        .into_iter()
        .map(|value| value.map_or_else(|| "None".to_owned(), |value| value.to_string()))
        .collect();
    Ok(Json(values))
}

pub(crate) async fn series_release_dates(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Vec<String>>, ApiError> {
    let query = parse_query(raw.as_deref());
    Ok(Json(
        state
            .storage
            .referential_release_years(&series_scope(&query), &auth.user)
            .await?
            .into_iter()
            .map(|year| year.to_string())
            .collect(),
    ))
}
