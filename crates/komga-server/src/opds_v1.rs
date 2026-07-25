//! OPDS 1.2 Atom catalogs and acquisition entries.

use std::{collections::BTreeSet, path::Path as FilePath};

use axum::{
    body::Body,
    extract::{Extension, Path, RawQuery, State},
    http::{HeaderMap, HeaderValue, Uri, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, SecondsFormat, Utc};
use komga_core::{
    BookCondition, BookDto, BookSearch, BooleanOperator, CollectionDto, ContentPage, EqualityOperator, Library, Media,
    MediaProfile, MediaStatus, PageRequest, ReadListDto, ReadStatus, SeriesCondition, SeriesDto, SeriesSearch,
    SortDirection, SortOrder, StringOperator,
};
use komga_media::{ImageOutput, convert_image, page_output_media_type};
use komga_storage::ReferentialScope;
use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event},
};
use url::Url;

use crate::{
    AppState,
    auth::AuthContext,
    content_access::load_visible_series,
    error::ApiError,
    thumbnail_routes::{read_thumbnail, thumbnail_response},
    webpub_routes::ManifestUrls,
};

const ATOM_FEED: &str = "application/atom+xml;profile=opds-catalog";
const NAVIGATION_FEED: &str = "application/atom+xml;profile=opds-catalog;kind=navigation";
const ACQUISITION_FEED: &str = "application/atom+xml;profile=opds-catalog;kind=acquisition";
const OPEN_SEARCH: &str = "application/opensearchdescription+xml";
const OPDS_JSON: &str = "application/opds+json";
const PSE_REL: &str = "http://vaemendis.net/opds-pse/stream";

#[derive(Debug, Clone)]
struct AtomLink {
    media_type: String,
    rel: String,
    href: String,
    pse: Option<PseAttributes>,
}

#[derive(Debug, Clone)]
struct PseAttributes {
    count: i32,
    last_read: Option<i32>,
    last_read_date: Option<String>,
}

#[derive(Debug, Clone)]
struct AtomEntry {
    title: String,
    updated: String,
    id: String,
    content: String,
    authors: Vec<String>,
    links: Vec<AtomLink>,
}

struct AtomFeed {
    id: String,
    title: String,
    updated: String,
    links: Vec<AtomLink>,
    entries: Vec<AtomEntry>,
}

#[derive(Debug, Clone)]
struct OpdsPage {
    request: PageRequest,
    filters: Vec<(String, String)>,
}

struct BookFeedContext<'a> {
    state: &'a AppState,
    headers: &'a HeaderMap,
    uri: &'a Uri,
    path: &'a str,
    title: &'a str,
}

pub(crate) async fn catalog(headers: HeaderMap, uri: Uri) -> Result<Response, ApiError> {
    let urls = urls(&headers, &uri)?;
    let entries = [
        (
            "keepReading",
            "Keep Reading",
            "Continue reading your in progress books",
            "keep-reading",
        ),
        ("ondeck", "On Deck", "Browse what to read next", "ondeck"),
        ("allSeries", "All series", "Browse by series", "series"),
        ("latestSeries", "Latest series", "Browse latest series", "series/latest"),
        ("latestBooks", "Latest books", "Browse latest books", "books/latest"),
        ("allLibraries", "All libraries", "Browse by library", "libraries"),
        (
            "allCollections",
            "All collections",
            "Browse by collection",
            "collections",
        ),
        ("allReadLists", "All read lists", "Browse by read lists", "readlists"),
        ("allPublishers", "All publishers", "Browse by publishers", "publishers"),
    ]
    .into_iter()
    .map(|(id, title, content, path)| {
        let segments = path.split('/').collect::<Vec<_>>();
        navigation_entry(id, title, content, urls.absolute_path(&segments))
    })
    .collect();
    Ok(feed_response(AtomFeed {
        id: "root".into(),
        title: "Komga OPDS catalog".into(),
        updated: now(),
        links: vec![
            navigation_link("self", urls.absolute_path(&["catalog"])),
            navigation_link("start", urls.absolute_path(&["catalog"])),
            AtomLink {
                media_type: OPEN_SEARCH.into(),
                rel: "search".into(),
                href: urls.absolute_path(&["search"]),
                pse: None,
            },
            AtomLink {
                media_type: OPDS_JSON.into(),
                rel: "alternate".into(),
                href: alternate_v2(&headers, &uri)?,
                pse: None,
            },
        ],
        entries,
    }))
}

pub(crate) async fn search(headers: HeaderMap, uri: Uri) -> Result<Response, ApiError> {
    let urls = urls(&headers, &uri)?;
    let template = format!("{}?search={{searchTerms}}", urls.absolute_path(&["series"]));
    let mut writer = xml_writer();
    let mut root = BytesStart::new("OpenSearchDescription");
    root.push_attribute(("xmlns", "http://a9.com/-/spec/opensearch/1.1/"));
    writer
        .write_event(Event::Start(root))
        .expect("writing XML to memory cannot fail");
    text_element(&mut writer, "ShortName", "Search");
    text_element(&mut writer, "Description", "Search for series");
    text_element(&mut writer, "InputEncoding", "UTF-8");
    text_element(&mut writer, "OutputEncoding", "UTF-8");
    let mut url = BytesStart::new("Url");
    url.push_attribute(("template", template.as_str()));
    url.push_attribute(("type", ACQUISITION_FEED));
    writer
        .write_event(Event::Empty(url))
        .expect("writing XML to memory cannot fail");
    writer
        .write_event(Event::End(BytesEnd::new("OpenSearchDescription")))
        .expect("writing XML to memory cannot fail");
    Ok(xml_response(writer.into_inner(), OPEN_SEARCH))
}

pub(crate) async fn on_deck(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let page = parse_page(raw.as_deref(), &[])?;
    let books = state
        .storage
        .query_books_on_deck(&[], &page.request, &auth.user)
        .await?;
    book_feed(
        &BookFeedContext {
            state: &state,
            headers: &headers,
            uri: &uri,
            path: "ondeck",
            title: "On Deck",
        },
        books,
        &page,
        true,
    )
    .await
}

pub(crate) async fn keep_reading(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("readProgress.readDate", SortDirection::Descending)];
    let books = state
        .storage
        .query_books(
            &BookSearch {
                condition: Some(BookCondition::AllOf {
                    conditions: vec![
                        BookCondition::ReadStatus {
                            operator: EqualityOperator::Is {
                                value: ReadStatus::InProgress,
                            },
                        },
                        ready_book(),
                        active_book(),
                    ],
                }),
                full_text_search: None,
            },
            &page.request,
            &auth.user,
            None,
        )
        .await?;
    book_feed(
        &BookFeedContext {
            state: &state,
            headers: &headers,
            uri: &uri,
            path: "keep-reading",
            title: "Keep Reading",
        },
        books,
        &page,
        true,
    )
    .await
}

pub(crate) async fn all_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut page = parse_page(raw.as_deref(), &["search", "publisher"])?;
    page.request.sort = vec![sort("metadata.titleSort", SortDirection::Ascending)];
    let search = filter_values(&page, "search").into_iter().next();
    let publishers = filter_values(&page, "publisher");
    let mut conditions = vec![active_series()];
    if let Some(search) = search.as_ref().filter(|value| !value.trim().is_empty()) {
        conditions.push(SeriesCondition::Title {
            operator: StringOperator::Contains { value: search.clone() },
        });
    }
    if !publishers.is_empty() {
        conditions.push(SeriesCondition::AnyOf {
            conditions: publishers
                .into_iter()
                .map(|value| SeriesCondition::Publisher {
                    operator: EqualityOperator::Is { value },
                })
                .collect(),
        });
    }
    let series = state
        .storage
        .query_series(
            &SeriesSearch {
                condition: Some(SeriesCondition::AllOf { conditions }),
                full_text_search: None,
            },
            &page.request,
            &auth.user,
            None,
        )
        .await?;
    let title = search.map_or_else(
        || "All series".to_owned(),
        |value| format!("Series search for: {value}"),
    );
    series_feed(&headers, &uri, "series", &title, series, &page, None)
}

pub(crate) async fn latest_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("lastModified", SortDirection::Descending)];
    let series = state
        .storage
        .query_series(
            &SeriesSearch {
                condition: Some(active_series()),
                full_text_search: None,
            },
            &page.request,
            &auth.user,
            None,
        )
        .await?;
    series_feed(&headers, &uri, "series/latest", "Latest series", series, &page, None)
}

pub(crate) async fn latest_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("createdDate", SortDirection::Descending)];
    let books = query_active_ready_books(&state, &auth, vec![], &page.request).await?;
    book_feed(
        &BookFeedContext {
            state: &state,
            headers: &headers,
            uri: &uri,
            path: "books/latest",
            title: "Latest books",
        },
        books,
        &page,
        true,
    )
    .await
}

pub(crate) async fn libraries(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let urls = urls(&headers, &uri)?;
    let mut libraries = state.storage.list_libraries().await?;
    libraries.retain(|library| auth.user.can_access_library(&library.id));
    libraries.sort_by_cached_key(|library| library.name.to_lowercase());
    let entries = libraries
        .into_iter()
        .map(|library| library_entry(&urls, library))
        .collect();
    Ok(feed_response(AtomFeed {
        id: "allLibraries".into(),
        title: "All libraries".into(),
        updated: now(),
        links: base_links::<()>(&urls, "libraries", None, None),
        entries,
    }))
}

pub(crate) async fn collections(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("name", SortDirection::Ascending)];
    let collections = state
        .storage
        .list_collections(None, None, None, &page.request, &auth.user)
        .await?;
    container_feed(
        &headers,
        &uri,
        "collections",
        "All collections",
        collections,
        &page,
        false,
    )
}

pub(crate) async fn read_lists(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("name", SortDirection::Ascending)];
    let lists = state
        .storage
        .list_read_lists(None, None, None, &page.request, &auth.user)
        .await?;
    container_feed(&headers, &uri, "readlists", "All read lists", lists, &page, true)
}

pub(crate) async fn publishers(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let page = parse_page(raw.as_deref(), &[])?;
    let values = state
        .storage
        .referential_publishers(&ReferentialScope::Global, &auth.user)
        .await?;
    let values = page_values(values, &page.request);
    let urls = urls(&headers, &uri)?;
    let entries = values
        .content
        .iter()
        .map(|publisher| {
            let href = with_query(
                urls.absolute_path(&["series"]),
                &[("publisher".into(), publisher.clone())],
            );
            navigation_entry(&format!("publisher:{publisher}"), publisher, "", href)
        })
        .collect();
    Ok(feed_response(AtomFeed {
        id: "allPublishers".into(),
        title: "All publishers".into(),
        updated: now(),
        links: base_links(&urls, "publishers", Some(&values), Some(&page)),
        entries,
    }))
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
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("metadata.numberSort", SortDirection::Ascending)];
    let books = query_active_ready_books(
        &state,
        &auth,
        vec![BookCondition::SeriesId {
            operator: EqualityOperator::Is { value: id.clone() },
        }],
        &page.request,
    )
    .await?;
    let path = format!("series/{id}");
    book_feed(
        &BookFeedContext {
            state: &state,
            headers: &headers,
            uri: &uri,
            path: &path,
            title: &series.metadata.title,
        },
        books,
        &page,
        false,
    )
    .await
}

pub(crate) async fn library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let library = state
        .storage
        .find_library_by_id(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("Library not found"))?;
    if !auth.user.can_access_library(&id) {
        return Err(ApiError::forbidden());
    }
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort("metadata.titleSort", SortDirection::Ascending)];
    let series = state
        .storage
        .query_series(
            &SeriesSearch {
                condition: Some(SeriesCondition::AllOf {
                    conditions: vec![
                        SeriesCondition::LibraryId {
                            operator: EqualityOperator::Is { value: id.clone() },
                        },
                        active_series(),
                    ],
                }),
                full_text_search: None,
            },
            &page.request,
            &auth.user,
            None,
        )
        .await?;
    series_feed(
        &headers,
        &uri,
        &format!("libraries/{id}"),
        &library.name,
        series,
        &page,
        None,
    )
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
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort(
        if collection.ordered {
            "collection.number"
        } else {
            "metadata.titleSort"
        },
        SortDirection::Ascending,
    )];
    let series = state
        .storage
        .query_series(
            &SeriesSearch {
                condition: Some(SeriesCondition::AllOf {
                    conditions: vec![
                        SeriesCondition::CollectionId {
                            operator: EqualityOperator::Is { value: id.clone() },
                        },
                        active_series(),
                    ],
                }),
                full_text_search: None,
            },
            &page.request,
            &auth.user,
            None,
        )
        .await?;
    series_feed(
        &headers,
        &uri,
        &format!("collections/{id}"),
        &collection.name,
        series,
        &page,
        collection.ordered.then_some(1),
    )
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
    let mut page = parse_page(raw.as_deref(), &[])?;
    page.request.sort = vec![sort(
        if list.ordered {
            "readList.number"
        } else {
            "metadata.releaseDate"
        },
        SortDirection::Ascending,
    )];
    let books = query_active_ready_books(
        &state,
        &auth,
        vec![BookCondition::ReadListId {
            operator: EqualityOperator::Is { value: id.clone() },
        }],
        &page.request,
    )
    .await?;
    let path = format!("readlists/{id}");
    book_feed(
        &BookFeedContext {
            state: &state,
            headers: &headers,
            uri: &uri,
            path: &path,
            title: &list.name,
        },
        books,
        &page,
        false,
    )
    .await
}

pub(crate) async fn small_thumbnail(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    crate::content_access::load_visible_book(&state, &auth.user, &book_id).await?;
    let thumbnail = state
        .storage
        .find_selected_book_thumbnail(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book thumbnail not found"))?;
    let generated = thumbnail.thumbnail_type == "GENERATED";
    let bytes = read_thumbnail(thumbnail.thumbnail, thumbnail.url).await?;
    let bytes = if generated {
        bytes
    } else {
        let max_edge = state.storage.thumbnail_max_edge().await?;
        tokio::task::spawn_blocking(move || convert_image(&bytes, ImageOutput::Jpeg, Some(max_edge)))
            .await
            .map_err(|error| {
                tracing::error!(%error, %book_id, "OPDS thumbnail converter panicked");
                ApiError::internal("Could not convert book thumbnail")
            })?
            .map_err(|_| ApiError::not_found("Book thumbnail not found"))?
            .bytes
    };
    thumbnail_response(bytes, &headers)
}

async fn query_active_ready_books(
    state: &AppState,
    auth: &AuthContext,
    mut conditions: Vec<BookCondition>,
    page: &PageRequest,
) -> Result<ContentPage<BookDto>, ApiError> {
    conditions.push(ready_book());
    conditions.push(active_book());
    Ok(state
        .storage
        .query_books(
            &BookSearch {
                condition: Some(BookCondition::AllOf { conditions }),
                full_text_search: None,
            },
            page,
            &auth.user,
            None,
        )
        .await?)
}

async fn book_feed(
    context: &BookFeedContext<'_>,
    books: ContentPage<BookDto>,
    page: &OpdsPage,
    prepend_series: bool,
) -> Result<Response, ApiError> {
    let urls = urls(context.headers, context.uri)?;
    let mut entries = Vec::with_capacity(books.content.len());
    for book in &books.content {
        let media = context
            .state
            .storage
            .find_media_by_book_id(&book.id)
            .await?
            .ok_or_else(|| ApiError::not_found("Book analysis failed"))?;
        entries.push(book_entry(&urls, book, &media, prepend_series));
    }
    Ok(feed_response(AtomFeed {
        id: context.path.rsplit('/').next().unwrap_or(context.path).into(),
        title: context.title.into(),
        updated: now(),
        links: base_links(&urls, context.path, Some(&books), Some(page)),
        entries,
    }))
}

fn series_feed(
    headers: &HeaderMap,
    uri: &Uri,
    path: &str,
    title: &str,
    series: ContentPage<SeriesDto>,
    page: &OpdsPage,
    prepend: Option<usize>,
) -> Result<Response, ApiError> {
    let urls = urls(headers, uri)?;
    let entries = series
        .content
        .iter()
        .enumerate()
        .map(|(index, series)| {
            let title = prepend.map_or_else(
                || series.metadata.title.clone(),
                |base| format!("{} - {}", base + index, series.metadata.title),
            );
            navigation_entry(&series.id, &title, "", urls.absolute_path(&["series", &series.id]))
        })
        .collect();
    Ok(feed_response(AtomFeed {
        id: path.rsplit('/').next().unwrap_or(path).into(),
        title: title.into(),
        updated: now(),
        links: base_links(&urls, path, Some(&series), Some(page)),
        entries,
    }))
}

trait ContainerEntry {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn modified(&self) -> &str;
}

impl ContainerEntry for CollectionDto {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn modified(&self) -> &str {
        &self.last_modified_date
    }
}

impl ContainerEntry for ReadListDto {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn modified(&self) -> &str {
        &self.last_modified_date
    }
}

fn container_feed<T: ContainerEntry>(
    headers: &HeaderMap,
    uri: &Uri,
    path: &str,
    title: &str,
    page_data: ContentPage<T>,
    page: &OpdsPage,
    read_list: bool,
) -> Result<Response, ApiError> {
    let urls = urls(headers, uri)?;
    let target = if read_list { "readlists" } else { "collections" };
    let entries = page_data
        .content
        .iter()
        .map(|value| AtomEntry {
            title: value.name().into(),
            updated: value.modified().into(),
            id: value.id().into(),
            content: String::new(),
            authors: vec![],
            links: vec![navigation_link("subsection", urls.absolute_path(&[target, value.id()]))],
        })
        .collect();
    Ok(feed_response(AtomFeed {
        id: path.into(),
        title: title.into(),
        updated: now(),
        links: base_links(&urls, path, Some(&page_data), Some(page)),
        entries,
    }))
}

fn book_entry(urls: &ManifestUrls, book: &BookDto, media: &Media, prepend_series: bool) -> AtomEntry {
    let mut page_types = match media.profile() {
        Some(MediaProfile::Divina) => media
            .pages
            .iter()
            .map(|page| page_output_media_type(media, page).to_owned())
            .collect::<BTreeSet<_>>(),
        Some(MediaProfile::Pdf) => BTreeSet::from(["image/jpeg".to_owned()]),
        Some(MediaProfile::Epub) if media.epub_divina_compatible => media
            .pages
            .iter()
            .map(|page| page_output_media_type(media, page).to_owned())
            .collect(),
        _ => BTreeSet::new(),
    };
    let page_link = if page_types.is_empty() {
        None
    } else {
        let direct = page_types.len() == 1
            && matches!(
                page_types.first().map(String::as_str),
                Some("image/jpeg" | "image/png" | "image/gif")
            );
        let media_type = if direct {
            page_types.pop_first().unwrap_or_else(|| "image/jpeg".into())
        } else {
            "image/jpeg".into()
        };
        let suffix = if direct {
            "{pageNumber}"
        } else {
            "{pageNumber}?convert=jpeg"
        };
        Some(AtomLink {
            media_type,
            rel: PSE_REL.into(),
            href: format!("{}{suffix}", urls.absolute_path(&["books", &book.id, "pages", ""])),
            pse: Some(PseAttributes {
                count: media.page_count,
                last_read: book.read_progress.as_ref().map(|progress| progress.page),
                last_read_date: book.read_progress.as_ref().map(|progress| progress.read_date.clone()),
            }),
        })
    };
    let file_name = FilePath::new(book.url.rsplit('/').next().unwrap_or(&book.name))
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(&book.name)
        .replace(';', "");
    let extension = file_name.rsplit_once('.').map_or("", |(_, extension)| extension);
    let mut links = vec![
        AtomLink {
            media_type: "image/jpeg".into(),
            rel: "http://opds-spec.org/image/thumbnail".into(),
            href: urls.absolute_path(&["books", &book.id, "thumbnail", "small"]),
            pse: None,
        },
        AtomLink {
            media_type: "image/jpeg".into(),
            rel: "http://opds-spec.org/image".into(),
            href: urls.absolute_path(&["books", &book.id, "thumbnail"]),
            pse: None,
        },
        AtomLink {
            media_type: media
                .media_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
            rel: "http://opds-spec.org/acquisition".into(),
            href: urls.absolute_path(&["books", &book.id, "file", &file_name]),
            pse: None,
        },
    ];
    links.extend(page_link);
    AtomEntry {
        title: if prepend_series {
            format!(
                "{} {}: {}",
                book.series_title, book.metadata.number, book.metadata.title
            )
        } else {
            book.metadata.title.clone()
        },
        updated: book.last_modified.clone(),
        id: book.id.clone(),
        content: if book.metadata.summary.trim().is_empty() {
            format!("{extension} - {}", book.size)
        } else {
            format!("{extension} - {}\n\n{}", book.size, book.metadata.summary)
        },
        authors: book.metadata.authors.iter().map(|author| author.name.clone()).collect(),
        links,
    }
}

fn library_entry(urls: &ManifestUrls, library: Library) -> AtomEntry {
    AtomEntry {
        title: library.name,
        updated: library.last_modified_date.unwrap_or_else(now),
        id: library.id.clone(),
        content: String::new(),
        authors: vec![],
        links: vec![navigation_link(
            "subsection",
            urls.absolute_path(&["libraries", &library.id]),
        )],
    }
}

fn navigation_entry(id: &str, title: &str, content: &str, href: String) -> AtomEntry {
    AtomEntry {
        title: title.into(),
        updated: now(),
        id: id.into(),
        content: content.into(),
        authors: vec![],
        links: vec![navigation_link("subsection", href)],
    }
}

fn navigation_link(rel: &str, href: String) -> AtomLink {
    AtomLink {
        media_type: NAVIGATION_FEED.into(),
        rel: rel.into(),
        href,
        pse: None,
    }
}

fn base_links<T>(
    urls: &ManifestUrls,
    path: &str,
    page_data: Option<&ContentPage<T>>,
    page: Option<&OpdsPage>,
) -> Vec<AtomLink> {
    let mut links = vec![
        navigation_link("self", filtered_url(urls, path, page, None)),
        navigation_link("start", urls.absolute_path(&["catalog"])),
    ];
    if let (Some(page_data), Some(page)) = (page_data, page) {
        if page_data.page > 0 {
            links.push(navigation_link(
                "previous",
                filtered_url(urls, path, Some(page), Some(page_data.page - 1)),
            ));
        }
        if (page_data.page + 1).saturating_mul(page_data.size) < page_data.total_elements {
            links.push(navigation_link(
                "next",
                filtered_url(urls, path, Some(page), Some(page_data.page + 1)),
            ));
        }
    }
    links
}

fn filtered_url(urls: &ManifestUrls, path: &str, page: Option<&OpdsPage>, page_number: Option<u64>) -> String {
    let segments = path.split('/').filter(|value| !value.is_empty()).collect::<Vec<_>>();
    let mut query = page.map_or_else(Vec::new, |page| page.filters.clone());
    if let Some(page_number) = page_number {
        query.push(("page".into(), page_number.to_string()));
        if page.is_some_and(|page| page.request.size != 20) {
            query.push(("size".into(), page.expect("page exists").request.size.to_string()));
        }
    }
    with_query(urls.absolute_path(&segments), &query)
}

fn with_query(base: String, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return base;
    }
    let mut url = Url::parse(&base).expect("absolute OPDS URLs are valid");
    url.query_pairs_mut()
        .extend_pairs(query.iter().map(|(key, value)| (key, value)));
    url.into()
}

fn parse_page(raw: Option<&str>, filters: &[&str]) -> Result<OpdsPage, ApiError> {
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
    Ok(OpdsPage {
        request,
        filters: preserved,
    })
}

fn filter_values(page: &OpdsPage, key: &str) -> Vec<String> {
    page.filters
        .iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
        .collect()
}

fn page_values<T>(values: Vec<T>, page: &PageRequest) -> ContentPage<T> {
    let total = values.len() as u64;
    let content = values
        .into_iter()
        .skip((page.page.saturating_mul(page.size)) as usize)
        .take(page.size as usize)
        .collect();
    ContentPage {
        content,
        total_elements: total,
        page: page.page,
        size: page.size,
        sort: vec![],
    }
}

fn sort(property: &str, direction: SortDirection) -> SortOrder {
    SortOrder {
        property: property.into(),
        direction,
    }
}

fn active_series() -> SeriesCondition {
    SeriesCondition::Deleted {
        operator: BooleanOperator::IsFalse,
    }
}

fn active_book() -> BookCondition {
    BookCondition::Deleted {
        operator: BooleanOperator::IsFalse,
    }
}

fn ready_book() -> BookCondition {
    BookCondition::MediaStatus {
        operator: EqualityOperator::Is {
            value: MediaStatus::Ready,
        },
    }
}

fn urls(headers: &HeaderMap, uri: &Uri) -> Result<ManifestUrls, ApiError> {
    ManifestUrls::from_request_with_prefix(headers, uri, &["opds", "v1.2"])
}

fn alternate_v2(headers: &HeaderMap, uri: &Uri) -> Result<String, ApiError> {
    Ok(ManifestUrls::from_request_with_prefix(headers, uri, &["opds", "v2"])?.absolute_path(&["catalog"]))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn normalize_date(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_else(|_| value.to_owned())
}

fn feed_response(feed: AtomFeed) -> Response {
    xml_response(render_feed(&feed), ATOM_FEED)
}

fn xml_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    let mut response = Body::from(bytes).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn xml_writer() -> Writer<Vec<u8>> {
    let mut writer = Writer::new(Vec::new());
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .expect("writing XML to memory cannot fail");
    writer
}

fn render_feed(feed: &AtomFeed) -> Vec<u8> {
    let mut writer = xml_writer();
    let mut root = BytesStart::new("feed");
    root.push_attribute(("xmlns", "http://www.w3.org/2005/Atom"));
    root.push_attribute(("xmlns:pse", "http://vaemendis.net/opds-pse/ns"));
    writer
        .write_event(Event::Start(root))
        .expect("writing XML to memory cannot fail");
    text_element(&mut writer, "id", &feed.id);
    text_element(&mut writer, "title", &feed.title);
    text_element(&mut writer, "updated", &normalize_date(&feed.updated));
    writer
        .write_event(Event::Start(BytesStart::new("author")))
        .expect("writing XML to memory cannot fail");
    text_element(&mut writer, "name", "Komga");
    text_element(&mut writer, "uri", "https://github.com/gotson/komga");
    writer
        .write_event(Event::End(BytesEnd::new("author")))
        .expect("writing XML to memory cannot fail");
    for link in &feed.links {
        write_link(&mut writer, link);
    }
    for entry in &feed.entries {
        writer
            .write_event(Event::Start(BytesStart::new("entry")))
            .expect("writing XML to memory cannot fail");
        text_element(&mut writer, "title", &entry.title);
        text_element(&mut writer, "updated", &normalize_date(&entry.updated));
        text_element(&mut writer, "id", &entry.id);
        text_element(&mut writer, "content", &entry.content.replace('\n', "<br/>"));
        for author in &entry.authors {
            writer
                .write_event(Event::Start(BytesStart::new("author")))
                .expect("writing XML to memory cannot fail");
            text_element(&mut writer, "name", author);
            writer
                .write_event(Event::End(BytesEnd::new("author")))
                .expect("writing XML to memory cannot fail");
        }
        for link in &entry.links {
            write_link(&mut writer, link);
        }
        writer
            .write_event(Event::End(BytesEnd::new("entry")))
            .expect("writing XML to memory cannot fail");
    }
    writer
        .write_event(Event::End(BytesEnd::new("feed")))
        .expect("writing XML to memory cannot fail");
    writer.into_inner()
}

fn write_link(writer: &mut Writer<Vec<u8>>, link: &AtomLink) {
    let mut element = BytesStart::new("link");
    element.push_attribute(("type", link.media_type.as_str()));
    element.push_attribute(("rel", link.rel.as_str()));
    element.push_attribute(("href", link.href.as_str()));
    let count;
    let last_read;
    let last_read_date;
    if let Some(pse) = &link.pse {
        count = pse.count.to_string();
        element.push_attribute(("pse:count", count.as_str()));
        last_read = pse.last_read.map(|value| value.to_string());
        if let Some(value) = &last_read {
            element.push_attribute(("pse:lastRead", value.as_str()));
        }
        last_read_date = pse.last_read_date.as_deref().map(normalize_date);
        if let Some(value) = &last_read_date {
            element.push_attribute(("pse:lastReadDate", value.as_str()));
        }
    }
    writer
        .write_event(Event::Empty(element))
        .expect("writing XML to memory cannot fail");
}

fn text_element(writer: &mut Writer<Vec<u8>>, name: &str, text: &str) {
    writer
        .write_event(Event::Start(BytesStart::new(name)))
        .expect("writing XML to memory cannot fail");
    writer
        .write_event(Event::Text(BytesText::new(text)))
        .expect("writing XML to memory cannot fail");
    writer
        .write_event(Event::End(BytesEnd::new(name)))
        .expect("writing XML to memory cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom_writer_escapes_text_and_writes_pse_attributes() {
        let xml = String::from_utf8(render_feed(&AtomFeed {
            id: "root".into(),
            title: "A & B".into(),
            updated: "2026-07-25T00:00:00Z".into(),
            links: vec![],
            entries: vec![AtomEntry {
                title: "<Book>".into(),
                updated: "2026-07-25T00:00:00Z".into(),
                id: "book".into(),
                content: String::new(),
                authors: vec![],
                links: vec![AtomLink {
                    media_type: "image/jpeg".into(),
                    rel: PSE_REL.into(),
                    href: "http://localhost/page/{pageNumber}".into(),
                    pse: Some(PseAttributes {
                        count: 10,
                        last_read: Some(2),
                        last_read_date: None,
                    }),
                }],
            }],
        }))
        .unwrap();
        assert!(xml.contains("A &amp; B"));
        assert!(xml.contains("&lt;Book&gt;"));
        let document = roxmltree::Document::parse(&xml).unwrap();
        let link = document.descendants().find(|node| node.has_tag_name("link")).unwrap();
        assert_eq!(
            link.attribute(("http://vaemendis.net/opds-pse/ns", "count")),
            Some("10")
        );
    }

    #[test]
    fn opds_pagination_caps_size_and_preserves_filters() {
        let page = parse_page(Some("page=2&size=9000&publisher=A%26B&unknown=x"), &["publisher"]).unwrap();
        assert_eq!(page.request.page, 2);
        assert_eq!(page.request.size, 2000);
        assert_eq!(page.filters, [("publisher".into(), "A&B".into())]);
    }
}
