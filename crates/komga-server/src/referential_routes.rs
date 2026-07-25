use axum::{
    Json,
    extract::{Extension, RawQuery, State},
};
use komga_core::AuthorDto;
use komga_storage::{ReferentialScope, author_page_request};

use crate::{AppState, auth::AuthContext, error::ApiError, pagination::SpringPage, pagination::parse_page_request};

#[derive(Debug, Default)]
struct ReferentialQuery {
    search: Option<String>,
    role: Option<String>,
    library_ids: Vec<String>,
    collection_id: Option<String>,
    series_id: Option<String>,
    read_list_id: Option<String>,
}

fn parse_query(raw: Option<&str>) -> ReferentialQuery {
    let mut query = ReferentialQuery::default();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "search" => query.search = Some(value.into_owned()),
            "role" => query.role = Some(value.into_owned()),
            "library_id" => query.library_ids.push(value.into_owned()),
            "collection_id" => query.collection_id = Some(value.into_owned()),
            "series_id" => query.series_id = Some(value.into_owned()),
            "readlist_id" => query.read_list_id = Some(value.into_owned()),
            _ => {}
        }
    }
    query
}

fn author_scope(query: &ReferentialQuery) -> ReferentialScope {
    if !query.library_ids.is_empty() {
        ReferentialScope::Libraries(query.library_ids.clone())
    } else if let Some(id) = &query.collection_id {
        ReferentialScope::Collection(id.clone())
    } else if let Some(id) = &query.series_id {
        ReferentialScope::Series(id.clone())
    } else if let Some(id) = &query.read_list_id {
        ReferentialScope::ReadList(id.clone())
    } else {
        ReferentialScope::Global
    }
}

fn series_scope(query: &ReferentialQuery) -> ReferentialScope {
    if !query.library_ids.is_empty() {
        ReferentialScope::Libraries(query.library_ids.clone())
    } else if let Some(id) = &query.collection_id {
        ReferentialScope::Collection(id.clone())
    } else {
        ReferentialScope::Global
    }
}

fn book_tag_scope(query: &ReferentialQuery) -> ReferentialScope {
    if let Some(id) = &query.series_id {
        ReferentialScope::Series(id.clone())
    } else if let Some(id) = &query.read_list_id {
        ReferentialScope::ReadList(id.clone())
    } else if !query.library_ids.is_empty() {
        ReferentialScope::Libraries(query.library_ids.clone())
    } else {
        ReferentialScope::Global
    }
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
    let page = author_page_request(parse_page_request(raw.as_deref())?);
    let authors = state
        .storage
        .referential_v2_authors(
            &author_scope(&query),
            query.search.as_deref(),
            query.role.as_deref(),
            &page,
            &auth.user,
        )
        .await?;
    Ok(Json(SpringPage::from_content(authors)))
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
