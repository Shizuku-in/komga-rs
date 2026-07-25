//! OPDS 2 JSON catalogs and authentication documents.

use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use chrono::{SecondsFormat, Utc};
use komga_core::{
    BookCondition, BookDto, BookSearch, BooleanOperator, CollectionDto, ContentPage, EqualityOperator, Library,
    MediaStatus, NullableEqualityOperator, PageRequest, ReadListDto, ReadStatus, SeriesCondition, SeriesDto,
    SeriesSearch, SortDirection, SortOrder, StringOperator,
};
use komga_search::EntityKind;
use komga_storage::ReferentialScope;
use serde::Serialize;
use url::Url;

use crate::{
    AppState,
    auth::AuthContext,
    content_access::load_visible_series,
    error::ApiError,
    search_query::search_hits,
    webpub_routes::{ManifestUrls, WpLinkDto, WpPublicationDto, opds_publication},
};

pub(crate) const OPDS_MEDIA_TYPE: &str = "application/opds+json";
pub(crate) const AUTH_MEDIA_TYPE: &str = "application/opds-authentication+json";
const SUBSECTION: &str = "subsection";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Feed {
    metadata: FeedMetadata,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<WpLinkDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    navigation: Vec<WpLinkDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    facets: Vec<Facet>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    groups: Vec<FeedGroup>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    publications: Vec<WpPublicationDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeedGroup {
    metadata: FeedMetadata,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<WpLinkDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    navigation: Vec<WpLinkDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    publications: Vec<WpPublicationDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Facet {
    metadata: FeedMetadata,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<WpLinkDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeedMetadata {
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sub_title: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    feed_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    items_per_page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_page: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    number_of_items: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthenticationDocument {
    authentication: Vec<AuthenticationFlow>,
    title: String,
    id: String,
    description: String,
    links: Vec<WpLinkDto>,
}

#[derive(Debug, Serialize)]
struct AuthenticationFlow {
    #[serde(rename = "type")]
    flow_type: String,
    labels: AuthenticationLabels,
}

#[derive(Debug, Serialize)]
struct AuthenticationLabels {
    login: String,
    password: String,
}

#[derive(Debug, Clone)]
struct CatalogPage {
    request: PageRequest,
    filters: Vec<(String, String)>,
}

struct FeedContext<'a> {
    state: &'a AppState,
    auth: &'a AuthContext,
    library: Option<&'a Library>,
    title: &'a str,
    suffix: &'a str,
    headers: &'a HeaderMap,
    uri: &'a Uri,
}

impl<'a> FeedContext<'a> {
    fn new(
        state: &'a AppState,
        auth: &'a AuthContext,
        library: Option<&'a Library>,
        title: &'a str,
        suffix: &'a str,
        headers: &'a HeaderMap,
        uri: &'a Uri,
    ) -> Self {
        Self {
            state,
            auth,
            library,
            title,
            suffix,
            headers,
            uri,
        }
    }
}

impl Feed {
    fn empty(title: impl Into<String>) -> Self {
        Self {
            metadata: FeedMetadata::new(title),
            links: vec![],
            navigation: vec![],
            facets: vec![],
            groups: vec![],
            publications: vec![],
        }
    }
}

impl FeedGroup {
    fn publications(title: &str, links: Vec<WpLinkDto>, publications: Vec<WpPublicationDto>) -> Self {
        Self {
            metadata: FeedMetadata::new(title),
            links,
            navigation: vec![],
            publications,
        }
    }

    fn navigation(title: &str, links: Vec<WpLinkDto>, navigation: Vec<WpLinkDto>) -> Self {
        Self {
            metadata: FeedMetadata::new(title),
            links,
            navigation,
            publications: vec![],
        }
    }
}

impl FeedMetadata {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            sub_title: None,
            feed_type: None,
            identifier: None,
            modified: None,
            description: None,
            items_per_page: None,
            current_page: None,
            number_of_items: None,
        }
    }

    fn paged<T>(title: impl Into<String>, modified: String, page: &ContentPage<T>) -> Self {
        Self {
            title: title.into(),
            modified: Some(modified),
            items_per_page: Some(page.size),
            current_page: Some(page.page + 1),
            number_of_items: Some(page.total_elements),
            ..Self::new("")
        }
    }
}

pub(crate) async fn auth_document(headers: HeaderMap, uri: Uri) -> Result<Response, ApiError> {
    let document = authentication_document(&headers, &uri)?;
    Ok(json_response(document, AUTH_MEDIA_TYPE, StatusCode::OK))
}

pub(crate) fn unauthorized(headers: &HeaderMap, uri: &Uri) -> Response {
    let document = authentication_document(headers, uri).unwrap_or_else(|_| AuthenticationDocument {
        authentication: vec![basic_flow()],
        title: "Komga".into(),
        id: "/opds/v2/auth".into(),
        description: "Enter your email and password to authenticate.".into(),
        links: vec![],
    });
    let auth_id = document.id.clone();
    let mut response = json_response(document, AUTH_MEDIA_TYPE, StatusCode::UNAUTHORIZED);
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"Realm\""),
    );
    if let Ok(value) = HeaderValue::from_str(&format!(
        "<{auth_id}>; rel=\"http://opds-spec.org/auth/document\"; type=\"{AUTH_MEDIA_TYPE}\""
    )) {
        response.headers_mut().insert(header::LINK, value);
    }
    response
}

pub(crate) async fn catalog(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    recommended(&state, &auth, None, &headers, &uri).await
}

pub(crate) async fn libraries(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    recommended(&state, &auth, None, &headers, &uri).await
}

pub(crate) async fn library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    recommended(&state, &auth, Some(id), &headers, &uri).await
}

async fn recommended(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let urls = urls(headers, uri)?;
    let prefix = library_prefix(library.as_ref());
    let recommendation_page = PageRequest {
        size: 5,
        ..PageRequest::default()
    };
    let keep_reading = query_books(
        state,
        auth,
        library_id.as_deref(),
        vec![BookCondition::ReadStatus {
            operator: EqualityOperator::Is {
                value: ReadStatus::InProgress,
            },
        }],
        true,
        fixed_page(&recommendation_page, "readProgress.readDate", SortDirection::Descending),
        None,
    )
    .await?;
    let on_deck = state
        .storage
        .query_books_on_deck(
            &library_id.iter().cloned().collect::<Vec<_>>(),
            &recommendation_page,
            &auth.user,
        )
        .await?;
    let latest_books = query_books(
        state,
        auth,
        library_id.as_deref(),
        vec![],
        true,
        fixed_page(&recommendation_page, "createdDate", SortDirection::Descending),
        None,
    )
    .await?;
    let latest_series = query_series(
        state,
        auth,
        library_id.as_deref(),
        vec![SeriesCondition::OneShot {
            operator: BooleanOperator::IsFalse,
        }],
        fixed_page(&recommendation_page, "lastModified", SortDirection::Descending),
        None,
    )
    .await?;
    let mut feed = Feed::empty(format!(
        "{} - Recommended",
        library
            .as_ref()
            .map_or("All libraries", |library| library.name.as_str())
    ));
    feed.metadata.modified = Some(library_modified(library.as_ref()));
    feed.links = standard_links::<()>(&urls, &prefix, None, None);
    feed.navigation = library_navigation(state, auth, library_id.as_deref(), &urls).await?;
    if library.is_none() {
        let mut libraries = state.storage.list_libraries().await?;
        libraries.retain(|library| auth.user.can_access_library(&library.id));
        feed.groups.push(FeedGroup::navigation(
            "Libraries",
            vec![link(
                Some("Libraries"),
                Some("self"),
                urls.absolute_path(&["libraries"]),
                OPDS_MEDIA_TYPE,
            )],
            libraries.iter().map(|library| library_link(&urls, library)).collect(),
        ));
    }
    add_publication_group(
        state,
        &mut feed.groups,
        "Keep Reading",
        &format!("{prefix}/keep-reading"),
        keep_reading,
        &urls,
    )
    .await?;
    add_publication_group(
        state,
        &mut feed.groups,
        "On Deck",
        &format!("{prefix}/on-deck"),
        on_deck,
        &urls,
    )
    .await?;
    add_publication_group(
        state,
        &mut feed.groups,
        "Latest Books",
        &format!("{prefix}/books/latest"),
        latest_books,
        &urls,
    )
    .await?;
    if !latest_series.content.is_empty() {
        feed.groups.push(FeedGroup::navigation(
            "Latest Series",
            vec![link(
                Some("Latest Series"),
                Some("self"),
                path_url(&urls, &format!("{prefix}/series/latest")),
                OPDS_MEDIA_TYPE,
            )],
            latest_series
                .content
                .iter()
                .map(|series| series_link(&urls, series))
                .collect(),
        ));
    }
    Ok(opds_response(feed))
}

macro_rules! library_feed_wrappers {
    ($all:ident, $one:ident, $common:ident) => {
        pub(crate) async fn $all(
            State(state): State<AppState>,
            Extension(auth): Extension<AuthContext>,
            RawQuery(raw): RawQuery,
            headers: HeaderMap,
            uri: Uri,
        ) -> Result<Response, ApiError> {
            $common(&state, &auth, None, raw.as_deref(), &headers, &uri).await
        }

        pub(crate) async fn $one(
            State(state): State<AppState>,
            Extension(auth): Extension<AuthContext>,
            Path(id): Path<String>,
            RawQuery(raw): RawQuery,
            headers: HeaderMap,
            uri: Uri,
        ) -> Result<Response, ApiError> {
            $common(&state, &auth, Some(id), raw.as_deref(), &headers, &uri).await
        }
    };
}

library_feed_wrappers!(keep_reading, library_keep_reading, keep_reading_impl);
library_feed_wrappers!(on_deck, library_on_deck, on_deck_impl);
library_feed_wrappers!(latest_books, library_latest_books, latest_books_impl);
library_feed_wrappers!(latest_series, library_latest_series, latest_series_impl);
library_feed_wrappers!(browse, library_browse, browse_impl);
library_feed_wrappers!(collections, library_collections, collections_impl);
library_feed_wrappers!(read_lists, library_read_lists, read_lists_impl);

async fn keep_reading_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &[])?;
    let books = query_books(
        state,
        auth,
        library_id.as_deref(),
        vec![BookCondition::ReadStatus {
            operator: EqualityOperator::Is {
                value: ReadStatus::InProgress,
            },
        }],
        true,
        fixed_page(&page.request, "readProgress.readDate", SortDirection::Descending),
        None,
    )
    .await?;
    publication_feed(
        &FeedContext::new(
            state,
            auth,
            library.as_ref(),
            "Keep Reading",
            "keep-reading",
            headers,
            uri,
        ),
        books,
        page,
    )
    .await
}

async fn on_deck_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &[])?;
    let books = state
        .storage
        .query_books_on_deck(
            &library_id.iter().cloned().collect::<Vec<_>>(),
            &page.request,
            &auth.user,
        )
        .await?;
    publication_feed(
        &FeedContext::new(state, auth, library.as_ref(), "On Deck", "on-deck", headers, uri),
        books,
        page,
    )
    .await
}

async fn latest_books_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &[])?;
    let books = query_books(
        state,
        auth,
        library_id.as_deref(),
        vec![],
        true,
        fixed_page(&page.request, "createdDate", SortDirection::Descending),
        None,
    )
    .await?;
    publication_feed(
        &FeedContext::new(
            state,
            auth,
            library.as_ref(),
            "Latest Books",
            "books/latest",
            headers,
            uri,
        ),
        books,
        page,
    )
    .await
}

async fn latest_series_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &[])?;
    let series = query_series(
        state,
        auth,
        library_id.as_deref(),
        vec![SeriesCondition::OneShot {
            operator: BooleanOperator::IsFalse,
        }],
        fixed_page(&page.request, "lastModified", SortDirection::Descending),
        None,
    )
    .await?;
    navigation_feed(
        &FeedContext::new(
            state,
            auth,
            library.as_ref(),
            "Latest Series",
            "series/latest",
            headers,
            uri,
        ),
        series,
        page,
    )
    .await
}

async fn browse_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &["publisher"])?;
    let publishers = filter_values(&page, "publisher");
    let mut extra = Vec::new();
    if !publishers.is_empty() {
        extra.push(SeriesCondition::AllOf {
            conditions: publishers
                .into_iter()
                .map(|value| SeriesCondition::Publisher {
                    operator: EqualityOperator::Is { value },
                })
                .collect(),
        });
    }
    let series = query_series(
        state,
        auth,
        library_id.as_deref(),
        extra,
        fixed_page(&page.request, "metadata.titleSort", SortDirection::Ascending),
        None,
    )
    .await?;
    let urls = urls(headers, uri)?;
    let prefix = library_prefix(library.as_ref());
    let scope = library_id.map_or(ReferentialScope::Global, |id| ReferentialScope::Libraries(vec![id]));
    let publisher_links = state
        .storage
        .referential_publishers(&scope, &auth.user)
        .await?
        .into_iter()
        .map(|publisher| {
            let href = query_url(
                path_url(&urls, &format!("{prefix}/browse")),
                &[("publisher".into(), publisher.clone())],
            );
            link(Some(&publisher), None, href, OPDS_MEDIA_TYPE)
        })
        .collect::<Vec<_>>();
    let mut feed = Feed::empty(library.as_ref().map_or("All libraries", |value| value.name.as_str()));
    feed.metadata = FeedMetadata::paged(feed.metadata.title.clone(), library_modified(library.as_ref()), &series);
    feed.links = standard_links(&urls, &format!("{prefix}/browse"), Some(&series), Some(&page));
    feed.navigation = library_navigation(state, auth, library.as_ref().map(|value| value.id.as_str()), &urls).await?;
    feed.groups.push(FeedGroup::navigation(
        "Series",
        vec![],
        series.content.iter().map(|series| series_link(&urls, series)).collect(),
    ));
    if !publisher_links.is_empty() {
        feed.groups
            .push(FeedGroup::navigation("Publisher", vec![], publisher_links));
    }
    Ok(opds_response(feed))
}

async fn collections_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &[])?;
    let collections = state
        .storage
        .list_collections(
            None,
            library_id.as_deref(),
            None,
            &fixed_page(&page.request, "name", SortDirection::Ascending),
            &auth.user,
        )
        .await?;
    container_navigation_feed(
        &FeedContext::new(
            state,
            auth,
            library.as_ref(),
            "Collections",
            "collections",
            headers,
            uri,
        ),
        collections,
        page,
        false,
    )
    .await
}

async fn read_lists_impl(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<String>,
    raw: Option<&str>,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let library = checked_library(state, auth, library_id.as_deref()).await?;
    let page = parse_page(raw, &[])?;
    let lists = state
        .storage
        .list_read_lists(
            None,
            library_id.as_deref(),
            None,
            &fixed_page(&page.request, "name", SortDirection::Ascending),
            &auth.user,
        )
        .await?;
    container_navigation_feed(
        &FeedContext::new(state, auth, library.as_ref(), "Read Lists", "readlists", headers, uri),
        lists,
        page,
        true,
    )
    .await
}

pub(crate) async fn collection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let collection = state
        .storage
        .find_collection(&id, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Collection not found"))?;
    let page = parse_page(raw.as_deref(), &[])?;
    let series = query_series(
        &state,
        &auth,
        None,
        vec![SeriesCondition::CollectionId {
            operator: EqualityOperator::Is { value: id.clone() },
        }],
        fixed_page(
            &page.request,
            if collection.ordered {
                "collection.number"
            } else {
                "metadata.titleSort"
            },
            SortDirection::Ascending,
        ),
        None,
    )
    .await?;
    let urls = urls(&headers, &uri)?;
    let mut feed = Feed::empty(&collection.name);
    feed.metadata = FeedMetadata::paged(&collection.name, collection.last_modified_date, &series);
    feed.links = standard_links(&urls, &format!("collections/{id}"), Some(&series), Some(&page));
    feed.navigation = series.content.iter().map(|series| series_link(&urls, series)).collect();
    Ok(opds_response(feed))
}

pub(crate) async fn read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let list = state
        .storage
        .find_read_list(&id, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Read list not found"))?;
    let page = parse_page(raw.as_deref(), &[])?;
    let books = query_books(
        &state,
        &auth,
        None,
        vec![BookCondition::ReadListId {
            operator: EqualityOperator::Is { value: id.clone() },
        }],
        true,
        fixed_page(
            &page.request,
            if list.ordered {
                "readList.number"
            } else {
                "metadata.releaseDate"
            },
            SortDirection::Ascending,
        ),
        None,
    )
    .await?;
    let urls = urls(&headers, &uri)?;
    let mut feed = Feed::empty(&list.name);
    feed.metadata = FeedMetadata::paged(&list.name, list.last_modified_date, &books);
    feed.links = standard_links(&urls, &format!("readlists/{id}"), Some(&books), Some(&page));
    feed.publications = publications(&state, &urls, &books.content).await?;
    Ok(opds_response(feed))
}

pub(crate) async fn series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let series = load_visible_series(&state, &auth.user, &id).await?;
    let page = parse_page(raw.as_deref(), &["tag"])?;
    let mut extra = vec![BookCondition::SeriesId {
        operator: EqualityOperator::Is { value: id.clone() },
    }];
    if let Some(tag) = filter_values(&page, "tag").into_iter().next() {
        extra.push(BookCondition::Tag {
            operator: NullableEqualityOperator::Is { value: tag },
        });
    }
    let books = query_books(
        &state,
        &auth,
        None,
        extra,
        true,
        fixed_page(&page.request, "metadata.numberSort", SortDirection::Ascending),
        None,
    )
    .await?;
    let urls = urls(&headers, &uri)?;
    let mut feed = Feed::empty(&series.metadata.title);
    feed.metadata = FeedMetadata::paged(&series.metadata.title, series.last_modified.clone(), &books);
    feed.metadata.description = Some(if series.metadata.summary.trim().is_empty() {
        series.books_metadata.summary.clone()
    } else {
        series.metadata.summary.clone()
    });
    feed.links = standard_links(&urls, &format!("series/{id}"), Some(&books), Some(&page));
    feed.publications = publications(&state, &urls, &books.content).await?;
    let selected_tag = filter_values(&page, "tag").into_iter().next();
    let tags = state
        .storage
        .referential_book_tags(&ReferentialScope::Series(id.clone()), &auth.user)
        .await?;
    if !tags.is_empty() {
        feed.facets.push(Facet {
            metadata: FeedMetadata::new("Tag"),
            links: tags
                .into_iter()
                .map(|tag| {
                    let href = query_url(urls.absolute_path(&["series", &id]), &[("tag".into(), tag.clone())]);
                    link(
                        Some(&tag),
                        (selected_tag.as_deref() == Some(tag.as_str())).then_some("self"),
                        href,
                        OPDS_MEDIA_TYPE,
                    )
                })
                .collect(),
        });
    }
    Ok(opds_response(feed))
}

pub(crate) async fn search(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let query = query_value(raw.as_deref(), "query").unwrap_or_default();
    let terms = query.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
    let page = PageRequest {
        size: 20,
        sort: if query.trim().is_empty() {
            vec![sort("metadata.titleSort", SortDirection::Ascending)]
        } else {
            vec![sort("relevance", SortDirection::Ascending)]
        },
        ..PageRequest::default()
    };
    let series_hits = (!query.trim().is_empty()).then(|| search_hits(&state, EntityKind::Series, &query));
    let book_hits = (!query.trim().is_empty()).then(|| search_hits(&state, EntityKind::Book, &query));
    let collection_hits = (!query.trim().is_empty()).then(|| search_hits(&state, EntityKind::Collection, &query));
    let read_list_hits = (!query.trim().is_empty()).then(|| search_hits(&state, EntityKind::ReadList, &query));
    let (series_hits, book_hits, collection_hits, read_list_hits) = tokio::join!(
        async {
            match series_hits {
                Some(value) => Some(value.await),
                None => None,
            }
        },
        async {
            match book_hits {
                Some(value) => Some(value.await),
                None => None,
            }
        },
        async {
            match collection_hits {
                Some(value) => Some(value.await),
                None => None,
            }
        },
        async {
            match read_list_hits {
                Some(value) => Some(value.await),
                None => None,
            }
        },
    );
    let series = query_series(
        &state,
        &auth,
        None,
        std::iter::once(SeriesCondition::OneShot {
            operator: BooleanOperator::IsFalse,
        })
        .chain(terms.iter().cloned().map(|value| SeriesCondition::Title {
            operator: StringOperator::Contains { value },
        }))
        .collect(),
        page.clone(),
        series_hits.as_deref(),
    )
    .await?;
    let books = query_books(
        &state,
        &auth,
        None,
        terms
            .into_iter()
            .map(|value| BookCondition::Title {
                operator: StringOperator::Contains { value },
            })
            .collect(),
        false,
        PageRequest {
            sort: page.sort.clone(),
            ..page.clone()
        },
        book_hits.as_deref(),
    )
    .await?;
    let collections = state
        .storage
        .list_collections(
            (!query.trim().is_empty()).then_some(query.as_str()),
            None,
            collection_hits.as_deref(),
            &PageRequest {
                sort: if collection_hits.is_some() {
                    page.sort.clone()
                } else {
                    vec![sort("name", SortDirection::Ascending)]
                },
                ..page.clone()
            },
            &auth.user,
        )
        .await?;
    let read_lists = state
        .storage
        .list_read_lists(
            (!query.trim().is_empty()).then_some(query.as_str()),
            None,
            read_list_hits.as_deref(),
            &PageRequest {
                sort: if read_list_hits.is_some() {
                    page.sort.clone()
                } else {
                    vec![sort("name", SortDirection::Ascending)]
                },
                ..page
            },
            &auth.user,
        )
        .await?;
    let urls = urls(&headers, &uri)?;
    let mut feed = Feed::empty("Search results");
    feed.metadata.modified = Some(now());
    feed.links = vec![start_link(&urls), search_link(&urls)];
    if !series.content.is_empty() {
        feed.groups.push(FeedGroup::navigation(
            "Series",
            vec![],
            series.content.iter().map(|series| series_link(&urls, series)).collect(),
        ));
    }
    if !books.content.is_empty() {
        feed.groups.push(FeedGroup::publications(
            "Books",
            vec![],
            publications(&state, &urls, &books.content).await?,
        ));
    }
    if !collections.content.is_empty() {
        feed.groups.push(FeedGroup::navigation(
            "Collections",
            vec![],
            collections
                .content
                .iter()
                .map(|value| container_link(&urls, value, false))
                .collect(),
        ));
    }
    if !read_lists.content.is_empty() {
        feed.groups.push(FeedGroup::navigation(
            "Read Lists",
            vec![],
            read_lists
                .content
                .iter()
                .map(|value| container_link(&urls, value, true))
                .collect(),
        ));
    }
    Ok(opds_response(feed))
}

async fn publication_feed(
    context: &FeedContext<'_>,
    books: ContentPage<BookDto>,
    page: CatalogPage,
) -> Result<Response, ApiError> {
    let urls = urls(context.headers, context.uri)?;
    let path = format!("{}/{}", library_prefix(context.library), context.suffix);
    let full_title = format!(
        "{} - {}",
        context.library.map_or("All libraries", |value| value.name.as_str()),
        context.title
    );
    let mut feed = Feed::empty(&full_title);
    feed.metadata = FeedMetadata::paged(full_title, library_modified(context.library), &books);
    feed.links = standard_links(&urls, &path, Some(&books), Some(&page));
    feed.publications = publications(context.state, &urls, &books.content).await?;
    Ok(opds_response(feed))
}

async fn navigation_feed(
    context: &FeedContext<'_>,
    series: ContentPage<SeriesDto>,
    page: CatalogPage,
) -> Result<Response, ApiError> {
    let urls = urls(context.headers, context.uri)?;
    let path = format!("{}/{}", library_prefix(context.library), context.suffix);
    let full_title = format!(
        "{} - {}",
        context.library.map_or("All libraries", |value| value.name.as_str()),
        context.title
    );
    let mut feed = Feed::empty(&full_title);
    feed.metadata = FeedMetadata::paged(full_title, library_modified(context.library), &series);
    feed.links = standard_links(&urls, &path, Some(&series), Some(&page));
    feed.navigation = series.content.iter().map(|series| series_link(&urls, series)).collect();
    feed.navigation.extend(
        library_navigation(
            context.state,
            context.auth,
            context.library.map(|value| value.id.as_str()),
            &urls,
        )
        .await?,
    );
    Ok(opds_response(feed))
}

trait ContainerValue {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
}

impl ContainerValue for CollectionDto {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
}

impl ContainerValue for ReadListDto {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
}

async fn container_navigation_feed<T: ContainerValue>(
    context: &FeedContext<'_>,
    values: ContentPage<T>,
    page: CatalogPage,
    read_list: bool,
) -> Result<Response, ApiError> {
    let urls = urls(context.headers, context.uri)?;
    let path = format!("{}/{}", library_prefix(context.library), context.suffix);
    let full_title = format!(
        "{} - {}",
        context.library.map_or("All libraries", |value| value.name.as_str()),
        context.title
    );
    let mut feed = Feed::empty(&full_title);
    feed.metadata = FeedMetadata::paged(full_title, library_modified(context.library), &values);
    feed.links = standard_links(&urls, &path, Some(&values), Some(&page));
    feed.navigation = library_navigation(
        context.state,
        context.auth,
        context.library.map(|value| value.id.as_str()),
        &urls,
    )
    .await?;
    feed.groups.push(FeedGroup::navigation(
        context.title,
        vec![],
        values
            .content
            .iter()
            .map(|value| {
                link(
                    Some(value.name()),
                    None,
                    urls.absolute_path(&[if read_list { "readlists" } else { "collections" }, value.id()]),
                    OPDS_MEDIA_TYPE,
                )
            })
            .collect(),
    ));
    Ok(opds_response(feed))
}

async fn add_publication_group(
    state: &AppState,
    groups: &mut Vec<FeedGroup>,
    title: &str,
    path: &str,
    books: ContentPage<BookDto>,
    urls: &ManifestUrls,
) -> Result<(), ApiError> {
    if !books.content.is_empty() {
        groups.push(FeedGroup::publications(
            title,
            vec![link(Some(title), Some("self"), path_url(urls, path), OPDS_MEDIA_TYPE)],
            publications(state, urls, &books.content).await?,
        ));
    }
    Ok(())
}

async fn publications(
    state: &AppState,
    urls: &ManifestUrls,
    books: &[BookDto],
) -> Result<Vec<WpPublicationDto>, ApiError> {
    let mut result = Vec::with_capacity(books.len());
    for book in books {
        let series = state
            .storage
            .find_series_dto_by_id(&book.series_id, "opds")
            .await?
            .ok_or_else(|| ApiError::not_found("Series not found"))?;
        let media = state
            .storage
            .find_media_by_book_id(&book.id)
            .await?
            .ok_or_else(|| ApiError::not_found("Book analysis failed"))?;
        result.push(opds_publication(book, &series.metadata, &media, urls));
    }
    Ok(result)
}

async fn query_books(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<&str>,
    mut extra: Vec<BookCondition>,
    ready: bool,
    page: PageRequest,
    ranked: Option<&[String]>,
) -> Result<ContentPage<BookDto>, ApiError> {
    if let Some(library_id) = library_id {
        extra.push(BookCondition::LibraryId {
            operator: EqualityOperator::Is {
                value: library_id.into(),
            },
        });
    }
    extra.push(BookCondition::Deleted {
        operator: BooleanOperator::IsFalse,
    });
    if ready {
        extra.push(BookCondition::MediaStatus {
            operator: EqualityOperator::Is {
                value: MediaStatus::Ready,
            },
        });
    }
    Ok(state
        .storage
        .query_books(
            &BookSearch {
                condition: Some(BookCondition::AllOf { conditions: extra }),
                full_text_search: None,
            },
            &page,
            &auth.user,
            ranked,
        )
        .await?)
}

async fn query_series(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<&str>,
    mut extra: Vec<SeriesCondition>,
    page: PageRequest,
    ranked: Option<&[String]>,
) -> Result<ContentPage<SeriesDto>, ApiError> {
    if let Some(library_id) = library_id {
        extra.push(SeriesCondition::LibraryId {
            operator: EqualityOperator::Is {
                value: library_id.into(),
            },
        });
    }
    extra.push(SeriesCondition::Deleted {
        operator: BooleanOperator::IsFalse,
    });
    Ok(state
        .storage
        .query_series(
            &SeriesSearch {
                condition: Some(SeriesCondition::AllOf { conditions: extra }),
                full_text_search: None,
            },
            &page,
            &auth.user,
            ranked,
        )
        .await?)
}

async fn checked_library(state: &AppState, auth: &AuthContext, id: Option<&str>) -> Result<Option<Library>, ApiError> {
    let Some(id) = id else { return Ok(None) };
    let library = state
        .storage
        .find_library_by_id(id)
        .await?
        .ok_or_else(|| ApiError::not_found("Library not found"))?;
    if !auth.user.can_access_library(id) {
        return Err(ApiError::forbidden());
    }
    Ok(Some(library))
}

async fn library_navigation(
    state: &AppState,
    auth: &AuthContext,
    library_id: Option<&str>,
    urls: &ManifestUrls,
) -> Result<Vec<WpLinkDto>, ApiError> {
    let prefix = library_id.map_or_else(|| "libraries".to_owned(), |id| format!("libraries/{id}"));
    let page = PageRequest {
        size: 1,
        sort: vec![sort("name", SortDirection::Ascending)],
        ..PageRequest::default()
    };
    let collections = state
        .storage
        .list_collections(None, library_id, None, &page, &auth.user)
        .await?;
    let read_lists = state
        .storage
        .list_read_lists(None, library_id, None, &page, &auth.user)
        .await?;
    let mut links = vec![
        link(
            Some("Recommended"),
            Some(SUBSECTION),
            path_url(urls, &prefix),
            OPDS_MEDIA_TYPE,
        ),
        link(
            Some("Browse"),
            Some(SUBSECTION),
            path_url(urls, &format!("{prefix}/browse")),
            OPDS_MEDIA_TYPE,
        ),
    ];
    if !collections.content.is_empty() {
        links.push(link(
            Some("Collections"),
            Some(SUBSECTION),
            path_url(urls, &format!("{prefix}/collections")),
            OPDS_MEDIA_TYPE,
        ));
    }
    if !read_lists.content.is_empty() {
        links.push(link(
            Some("Read lists"),
            Some(SUBSECTION),
            path_url(urls, &format!("{prefix}/readlists")),
            OPDS_MEDIA_TYPE,
        ));
    }
    Ok(links)
}

fn standard_links<T>(
    urls: &ManifestUrls,
    path: &str,
    page_data: Option<&ContentPage<T>>,
    page: Option<&CatalogPage>,
) -> Vec<WpLinkDto> {
    let mut links = vec![
        link(
            None,
            Some("self"),
            filtered_url(urls, path, page, None),
            OPDS_MEDIA_TYPE,
        ),
        start_link(urls),
        search_link(urls),
    ];
    if let (Some(data), Some(page)) = (page_data, page) {
        if data.page > 0 {
            links.push(link(
                None,
                Some("previous"),
                filtered_url(urls, path, Some(page), Some(data.page - 1)),
                OPDS_MEDIA_TYPE,
            ));
        }
        if (data.page + 1).saturating_mul(data.size) < data.total_elements {
            links.push(link(
                None,
                Some("next"),
                filtered_url(urls, path, Some(page), Some(data.page + 1)),
                OPDS_MEDIA_TYPE,
            ));
        }
    }
    links
}

fn start_link(urls: &ManifestUrls) -> WpLinkDto {
    link(
        Some("Home"),
        Some("start"),
        urls.absolute_path(&["catalog"]),
        OPDS_MEDIA_TYPE,
    )
}

fn search_link(urls: &ManifestUrls) -> WpLinkDto {
    WpLinkDto {
        title: Some("Search".into()),
        rel: Some("search".into()),
        href: Some(format!("{}{{?query}}", urls.absolute_path(&["search"]))),
        media_type: Some(OPDS_MEDIA_TYPE.into()),
        templated: Some(true),
        ..WpLinkDto::default()
    }
}

fn link(title: Option<&str>, rel: Option<&str>, href: String, media_type: &str) -> WpLinkDto {
    WpLinkDto {
        title: title.map(str::to_owned),
        rel: rel.map(str::to_owned),
        href: Some(href),
        media_type: Some(media_type.into()),
        ..WpLinkDto::default()
    }
}

fn library_link(urls: &ManifestUrls, library: &Library) -> WpLinkDto {
    link(
        Some(&library.name),
        None,
        urls.absolute_path(&["libraries", &library.id]),
        OPDS_MEDIA_TYPE,
    )
}

fn series_link(urls: &ManifestUrls, series: &SeriesDto) -> WpLinkDto {
    link(
        Some(&series.metadata.title),
        None,
        urls.absolute_path(&["series", &series.id]),
        OPDS_MEDIA_TYPE,
    )
}

fn container_link<T: ContainerValue>(urls: &ManifestUrls, value: &T, read_list: bool) -> WpLinkDto {
    link(
        Some(value.name()),
        None,
        urls.absolute_path(&[if read_list { "readlists" } else { "collections" }, value.id()]),
        OPDS_MEDIA_TYPE,
    )
}

fn parse_page(raw: Option<&str>, filters: &[&str]) -> Result<CatalogPage, ApiError> {
    let mut request = PageRequest::default();
    let mut preserved = Vec::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "page" => {
                request.page = value
                    .parse()
                    .map_err(|_| ApiError::bad_request("page must be a non-negative integer"))?;
            }
            "size" => {
                request.size = value
                    .parse::<u64>()
                    .map_err(|_| ApiError::bad_request("size must be a positive integer"))?
                    .min(2000);
                if request.size == 0 {
                    return Err(ApiError::bad_request("size must be greater than zero"));
                }
            }
            key if filters.contains(&key) => preserved.push((key.to_owned(), value.into_owned())),
            _ => {}
        }
    }
    Ok(CatalogPage {
        request,
        filters: preserved,
    })
}

fn filter_values(page: &CatalogPage, key: &str) -> Vec<String> {
    page.filters
        .iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .collect()
}

fn query_value(raw: Option<&str>, name: &str) -> Option<String> {
    url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .find_map(|(key, value)| (key == name).then(|| value.into_owned()))
}

fn filtered_url(urls: &ManifestUrls, path: &str, page: Option<&CatalogPage>, number: Option<u64>) -> String {
    let mut query = page.map_or_else(Vec::new, |page| page.filters.clone());
    if let Some(number) = number {
        query.push(("page".into(), number.to_string()));
        if page.is_some_and(|page| page.request.size != 20) {
            query.push(("size".into(), page.expect("page exists").request.size.to_string()));
        }
    }
    query_url(path_url(urls, path), &query)
}

fn query_url(base: String, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return base;
    }
    let mut url = Url::parse(&base).expect("absolute OPDS URLs are valid");
    url.query_pairs_mut()
        .extend_pairs(query.iter().map(|(key, value)| (key, value)));
    url.into()
}

fn path_url(urls: &ManifestUrls, path: &str) -> String {
    let segments = path.split('/').filter(|value| !value.is_empty()).collect::<Vec<_>>();
    urls.absolute_path(&segments)
}

fn fixed_page(page: &PageRequest, property: &str, direction: SortDirection) -> PageRequest {
    PageRequest {
        sort: vec![sort(property, direction)],
        ..page.clone()
    }
}

fn sort(property: &str, direction: SortDirection) -> SortOrder {
    SortOrder {
        property: property.into(),
        direction,
    }
}

fn library_prefix(library: Option<&Library>) -> String {
    library.map_or_else(|| "libraries".into(), |library| format!("libraries/{}", library.id))
}

fn library_modified(library: Option<&Library>) -> String {
    library
        .and_then(|library| library.last_modified_date.clone())
        .unwrap_or_else(now)
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn urls(headers: &HeaderMap, uri: &Uri) -> Result<ManifestUrls, ApiError> {
    ManifestUrls::from_request_with_prefix(headers, uri, &["opds", "v2"])
}

fn authentication_document(headers: &HeaderMap, uri: &Uri) -> Result<AuthenticationDocument, ApiError> {
    let urls = urls(headers, uri)?;
    Ok(AuthenticationDocument {
        authentication: vec![basic_flow()],
        title: "Komga".into(),
        id: urls.absolute_path(&["auth"]),
        description: "Enter your email and password to authenticate.".into(),
        links: vec![
            link(Some("Help"), Some("help"), "https://komga.org".into(), "text/html"),
            link(
                Some("Logo"),
                Some("logo"),
                root_asset(headers, uri, "android-chrome-512x512.png")?,
                "image/png",
            ),
        ],
    })
}

fn basic_flow() -> AuthenticationFlow {
    AuthenticationFlow {
        flow_type: "http://opds-spec.org/auth/basic".into(),
        labels: AuthenticationLabels {
            login: "Email".into(),
            password: "Password".into(),
        },
    }
}

fn root_asset(headers: &HeaderMap, uri: &Uri, asset: &str) -> Result<String, ApiError> {
    Ok(ManifestUrls::from_request_with_prefix(headers, uri, &[])?.absolute_path(&[asset]))
}

fn opds_response(feed: Feed) -> Response {
    json_response(feed, OPDS_MEDIA_TYPE, StatusCode::OK)
}

fn json_response<T: Serialize>(value: T, content_type: &'static str, status: StatusCode) -> Response {
    let mut response = (status, Json(value)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_omits_empty_collections_and_uses_one_based_page_metadata() {
        let page = ContentPage::<()> {
            content: vec![],
            total_elements: 42,
            page: 2,
            size: 10,
            sort: vec![],
        };
        let feed = Feed {
            metadata: FeedMetadata::paged("Books", now(), &page),
            links: vec![],
            navigation: vec![],
            facets: vec![],
            groups: vec![],
            publications: vec![],
        };
        let json = serde_json::to_value(feed).unwrap();
        assert_eq!(json["metadata"]["currentPage"], 3);
        assert_eq!(json["metadata"]["numberOfItems"], 42);
        assert!(json.get("groups").is_none());
        assert!(json.get("publications").is_none());
    }

    #[test]
    fn page_parser_caps_size_and_preserves_facets() {
        let page = parse_page(Some("page=1&size=9000&publisher=A%26B&sort=x"), &["publisher"]).unwrap();
        assert_eq!(page.request.page, 1);
        assert_eq!(page.request.size, 2000);
        assert_eq!(page.filters, [("publisher".into(), "A&B".into())]);
    }
}
