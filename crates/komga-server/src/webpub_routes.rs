//! Kotlin-compatible WebPub publication manifests for analyzed books.
//!
//! The transport types in this module mirror Kotlin's Jackson inclusion rules:
//! publication-level arrays remain present when empty, while empty values inside
//! links and metadata are omitted. The generic route selects a Divina, PDF, or
//! EPUB representation from persisted media. PDF and EPUB routes enforce their
//! requested profile, while Divina reuses any available image-page rendering;
//! every route applies the same REST visibility checks.
//!
//! Manifest links are absolute. Their public origin honors `Forwarded` first,
//! then the corresponding `X-Forwarded-*` headers, before falling back to the
//! request URI and `Host`. Manifest responses deliberately do not implement
//! conditional caching, matching the Kotlin endpoints.

use std::collections::BTreeMap;

use axum::{
    Json,
    extract::{Extension, Path, State},
    http::{HeaderMap, HeaderValue, Uri, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Local, SecondsFormat};
use komga_core::{
    AuthorDto, EPUB_ASSET_SUBTYPE, EPUB_PAGE_SUBTYPE, Media, MediaExtension, MediaProfile, PDF_MEDIA_TYPE,
    RAR_MEDIA_TYPE, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, SeriesMetadataDto, ZIP_MEDIA_TYPE,
};
use komga_media::{can_convert_image_media_type, page_output_dimension, page_output_media_type};
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::{AppState, auth::AuthContext, content_access::load_visible_book_context, error::ApiError};

pub(crate) const DIVINA_MEDIA_TYPE: &str = "application/divina+json";
pub(crate) const WEBPUB_MEDIA_TYPE: &str = "application/webpub+json";
pub(crate) const OPDS_PUBLICATION_MEDIA_TYPE: &str = "application/opds-publication+json";
const DIVINA_PROFILE: &str = "https://readium.org/webpub-manifest/profiles/divina";
const EPUB_PROFILE: &str = "https://readium.org/webpub-manifest/profiles/epub";
const PDF_PROFILE: &str = "https://readium.org/webpub-manifest/profiles/pdf";
const CONTEXT: &str = "https://readium.org/webpub-manifest/context.jsonld";
const ACQUISITION_REL: &str = "http://opds-spec.org/acquisition";

pub(crate) type LinkProperties = BTreeMap<String, BTreeMap<String, Value>>;

/// A WebPub link with Kotlin's nested empty-value omission behavior.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WpLinkDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) href: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub(crate) media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) templated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) width: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) height: Option<i32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) alternate: Vec<WpLinkDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) children: Vec<WpLinkDto>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) properties: LinkProperties,
}

impl WpLinkDto {
    pub(crate) fn resource(href: String, media_type: Option<String>) -> Self {
        Self {
            href: Some(href),
            media_type,
            ..Self::default()
        }
    }
}

/// Complete publication payload; all collection fields serialize even when empty.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WpPublicationDto {
    pub(crate) context: String,
    pub(crate) metadata: WpMetadataDto,
    pub(crate) links: Vec<WpLinkDto>,
    pub(crate) images: Vec<WpLinkDto>,
    pub(crate) reading_order: Vec<WpLinkDto>,
    pub(crate) resources: Vec<WpLinkDto>,
    pub(crate) toc: Vec<WpLinkDto>,
    pub(crate) landmarks: Vec<WpLinkDto>,
    pub(crate) page_list: Vec<WpLinkDto>,
}

/// Metadata shared by the Divina, PDF, and EPUB publication profiles.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WpMetadataDto {
    #[serde(skip_serializing_if = "String::is_empty")]
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    identifier: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    publication_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conforms_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sort_as: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    author: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    translator: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    editor: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    artist: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    illustrator: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    letterer: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    penciler: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    colorist: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    inker: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    contributor: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    publisher: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subject: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reading_progression: Option<WpReadingProgressionDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    number_of_pages: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    belongs_to: Option<WpBelongsToDto>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    rendition: BTreeMap<String, Value>,
}

/// Series and collection membership exposed through WebPub metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WpBelongsToDto {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    series: Vec<WpContributorDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    collection: Vec<WpContributorDto>,
}

/// Named member of a containing series or collection.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WpContributorDto {
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<f32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<WpLinkDto>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct AuthorGroups {
    author: Vec<String>,
    translator: Vec<String>,
    editor: Vec<String>,
    artist: Vec<String>,
    illustrator: Vec<String>,
    letterer: Vec<String>,
    penciler: Vec<String>,
    colorist: Vec<String>,
    inker: Vec<String>,
    contributor: Vec<String>,
}

/// Reading directions supported by Kotlin's WebPub generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum WpReadingProgressionDto {
    Rtl,
    Ltr,
    Ttb,
}

/// Profile selected by the requested manifest route.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RequestedProfile {
    Automatic,
    Divina,
    Pdf,
    Epub,
}

/// Proxy-aware base URL used to build every absolute publication link.
#[derive(Debug, Clone)]
pub(crate) struct ManifestUrls {
    base: Url,
    path_prefix: Vec<String>,
}

impl ManifestUrls {
    /// Resolves the client-visible origin using Spring's forwarded-header precedence.
    pub(crate) fn from_request(headers: &HeaderMap, uri: &Uri) -> Result<Self, ApiError> {
        Self::from_request_with_prefix(headers, uri, &["api", "v1"])
    }

    pub(crate) fn from_request_with_prefix(
        headers: &HeaderMap,
        uri: &Uri,
        path_prefix: &[&str],
    ) -> Result<Self, ApiError> {
        let forwarded_proto = forwarded_parameter(headers, "proto");
        let forwarded_host = forwarded_parameter(headers, "host");
        let scheme = forwarded_proto
            .or_else(|| first_header(headers, "x-forwarded-proto"))
            .or_else(|| uri.scheme_str().map(str::to_owned))
            .unwrap_or_else(|| "http".to_owned())
            .to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(ApiError::bad_request("Invalid forwarded request scheme"));
        }
        let host = forwarded_host
            .or_else(|| first_header(headers, "x-forwarded-host"))
            .or_else(|| uri.authority().map(ToString::to_string))
            .or_else(|| first_header(headers, header::HOST.as_str()))
            .ok_or_else(|| ApiError::bad_request("Host header is required"))?;
        let mut base = Url::parse(&format!("{scheme}://{host}/"))
            .map_err(|_| ApiError::bad_request("Invalid forwarded request host"))?;
        if let Some(prefix) = first_header(headers, "x-forwarded-prefix") {
            let prefix = prefix.trim();
            if !prefix.is_empty() && prefix != "/" {
                let normalized = format!("/{}/", prefix.trim_matches('/'));
                base.set_path(&normalized);
            }
        }
        Ok(Self {
            base,
            path_prefix: path_prefix.iter().map(|value| (*value).to_owned()).collect(),
        })
    }

    fn api_path(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        let mut path = url.path_segments_mut().expect("HTTP URLs support path segments");
        path.pop_if_empty();
        for segment in &self.path_prefix {
            path.push(segment);
        }
        for segment in segments {
            path.push(segment);
        }
        drop(path);
        url
    }

    pub(crate) fn absolute_path(&self, segments: &[&str]) -> String {
        self.api_path(segments).into()
    }

    pub(crate) fn manifest(&self, book_id: &str) -> String {
        self.api_path(&["books", book_id, "manifest"]).into()
    }

    pub(crate) fn profile_manifest(&self, book_id: &str, profile: &str) -> String {
        self.api_path(&["books", book_id, "manifest", profile]).into()
    }

    pub(crate) fn book_file(&self, book_id: &str) -> String {
        self.api_path(&["books", book_id, "file"]).into()
    }

    pub(crate) fn thumbnail(&self, book_id: &str) -> String {
        self.api_path(&["books", book_id, "thumbnail"]).into()
    }

    pub(crate) fn progression(&self, book_id: &str) -> String {
        self.api_path(&["books", book_id, "progression"]).into()
    }

    pub(crate) fn series(&self, series_id: &str) -> String {
        self.api_path(&["series", series_id]).into()
    }

    pub(crate) fn page(&self, book_id: &str, page_number: usize, convert: Option<&str>) -> String {
        let page_number = page_number.to_string();
        let mut url = self.api_path(&["books", book_id, "pages", &page_number]);
        url.query_pairs_mut().append_pair("contentNegotiation", "false");
        if let Some(convert) = convert {
            url.query_pairs_mut().append_pair("convert", convert);
        }
        url.into()
    }

    pub(crate) fn raw_page(&self, book_id: &str, page_number: i32) -> String {
        let page_number = page_number.to_string();
        self.api_path(&["books", book_id, "pages", &page_number, "raw"]).into()
    }

    pub(crate) fn resource(&self, book_id: &str, resource: &str) -> String {
        let mut url = self.api_path(&["books", book_id, "resource"]);
        {
            let mut path = url.path_segments_mut().expect("HTTP URLs support path segments");
            for segment in resource.split('/').filter(|segment| !segment.is_empty()) {
                path.push(segment);
            }
        }
        url.into()
    }

    fn navigation_resource(&self, book_id: &str, href: &str) -> String {
        let (resource, fragment) = href.rsplit_once('#').map_or((href, None), |(path, fragment)| {
            (path, (!fragment.is_empty()).then_some(fragment))
        });
        let mut url = Url::parse(&self.resource(book_id, resource)).expect("generated resource URL is valid");
        url.set_fragment(fragment);
        url.into()
    }
}

/// Returns the manifest selected from the book's persisted media profile.
pub(crate) async fn get_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Automatic, false).await
}

/// Returns the image-oriented Divina representation.
pub(crate) async fn get_divina_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Divina, false).await
}

/// Returns the PDF profile or `400` when the book is not a PDF.
pub(crate) async fn get_pdf_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Pdf, false).await
}

/// Returns the EPUB profile or `400` when the book is not an EPUB.
pub(crate) async fn get_epub_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Epub, false).await
}

pub(crate) async fn get_opds_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Automatic, true).await
}

pub(crate) async fn get_opds_divina_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Divina, true).await
}

pub(crate) async fn get_opds_pdf_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Pdf, true).await
}

pub(crate) async fn get_opds_epub_manifest(
    state: State<AppState>,
    auth: Extension<AuthContext>,
    path: Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    manifest_response(state, auth, path, headers, uri, RequestedProfile::Epub, true).await
}

/// Loads visible content, resolves the requested profile, and serializes its payload.
async fn manifest_response(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
    headers: HeaderMap,
    uri: Uri,
    requested: RequestedProfile,
    opds: bool,
) -> Result<Response, ApiError> {
    let (book, series) = load_visible_book_context(&state, &auth.user, &book_id).await?;
    let media = state
        .storage
        .find_media_by_book_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book analysis failed"))?;
    let profile = match requested {
        RequestedProfile::Automatic => media
            .profile()
            .ok_or_else(|| ApiError::not_found("Book analysis failed"))?,
        RequestedProfile::Divina => MediaProfile::Divina,
        RequestedProfile::Pdf if media.profile() == Some(MediaProfile::Pdf) => MediaProfile::Pdf,
        RequestedProfile::Epub if media.profile() == Some(MediaProfile::Epub) => MediaProfile::Epub,
        RequestedProfile::Pdf | RequestedProfile::Epub => {
            return Err(ApiError::bad_request(format!(
                "Book media type '{}' not compatible with requested profile",
                media.media_type.as_deref().unwrap_or_default()
            )));
        }
    };
    let urls = if opds {
        ManifestUrls::from_request_with_prefix(&headers, &uri, &["opds", "v2"])?
    } else {
        ManifestUrls::from_request(&headers, &uri)?
    };
    let (mut publication, content_type) = match profile {
        MediaProfile::Divina => (
            divina_manifest(&book, &series.metadata, &media, &urls),
            DIVINA_MEDIA_TYPE,
        ),
        MediaProfile::Pdf => (pdf_manifest(&book, &series.metadata, &media, &urls), WEBPUB_MEDIA_TYPE),
        MediaProfile::Epub => (epub_manifest(&book, &series.metadata, &media, &urls), WEBPUB_MEDIA_TYPE),
    };
    if opds {
        decorate_opds_publication(&mut publication, &book, &urls);
    }
    let mut response = Json(publication).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if opds {
            OPDS_PUBLICATION_MEDIA_TYPE
        } else {
            content_type
        }),
    );
    Ok(response)
}

pub(crate) fn opds_publication(
    book: &komga_core::BookDto,
    series: &SeriesMetadataDto,
    media: &Media,
    urls: &ManifestUrls,
) -> WpPublicationDto {
    let mut publication = base_publication(book, series, media, urls);
    decorate_opds_publication(&mut publication, book, urls);
    publication
}

fn decorate_opds_publication(publication: &mut WpPublicationDto, book: &komga_core::BookDto, urls: &ManifestUrls) {
    for link in &mut publication.links {
        if link.href.as_deref().is_some_and(|href| href.contains("/manifest")) {
            link.media_type = Some(OPDS_PUBLICATION_MEDIA_TYPE.to_owned());
        }
    }
    let authentication = BTreeMap::from([(
        "authenticate".to_owned(),
        BTreeMap::from([
            ("href".to_owned(), Value::String(urls.absolute_path(&["auth"]))),
            (
                "type".to_owned(),
                Value::String("application/opds-authentication+json".to_owned()),
            ),
        ]),
    )]);
    publication.links.push(WpLinkDto {
        rel: Some("http://readium.org/webpub-manifest/relations/progression".to_owned()),
        href: Some(urls.progression(&book.id)),
        media_type: Some("application/vnd.readium.progression+json".to_owned()),
        properties: authentication,
        ..WpLinkDto::default()
    });
    if let Some(belongs_to) = publication.metadata.belongs_to.as_mut()
        && let Some(series) = belongs_to.series.first_mut()
    {
        series.links.push(WpLinkDto {
            href: Some(urls.series(&book.series_id)),
            media_type: Some("application/opds+json".to_owned()),
            ..WpLinkDto::default()
        });
    }
    publication.images = vec![WpLinkDto::resource(
        urls.thumbnail(&book.id),
        Some("image/jpeg".to_owned()),
    )];
}

/// Builds image reading-order links and JPEG alternates for the Divina profile.
fn divina_manifest(
    book: &komga_core::BookDto,
    series: &SeriesMetadataDto,
    media: &Media,
    urls: &ManifestUrls,
) -> WpPublicationDto {
    let reading_order = media
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let media_type = page_output_media_type(media, page).to_owned();
            let dimension = page_output_dimension(media, page);
            let alternate = (!is_recommended_image(&media_type) && can_convert_image_media_type(&media_type))
                .then(|| WpLinkDto {
                    href: Some(urls.page(&book.id, index + 1, Some("jpeg"))),
                    media_type: Some("image/jpeg".to_owned()),
                    width: dimension.map(|value| value.width),
                    height: dimension.map(|value| value.height),
                    ..WpLinkDto::default()
                })
                .into_iter()
                .collect();
            WpLinkDto {
                href: Some(urls.page(&book.id, index + 1, None)),
                media_type: Some(media_type),
                width: dimension.map(|value| value.width),
                height: dimension.map(|value| value.height),
                alternate,
                ..WpLinkDto::default()
            }
        })
        .collect();
    let mut publication = base_publication(book, series, media, urls);
    publication.metadata.conforms_to = Some(DIVINA_PROFILE.to_owned());
    publication.reading_order = reading_order;
    publication.resources = thumbnail_links(book, urls);
    publication
}

/// Builds one raw single-page PDF link for each analyzed document page.
fn pdf_manifest(
    book: &komga_core::BookDto,
    series: &SeriesMetadataDto,
    media: &Media,
    urls: &ManifestUrls,
) -> WpPublicationDto {
    let mut publication = base_publication(book, series, media, urls);
    publication.metadata.conforms_to = Some(PDF_PROFILE.to_owned());
    publication.reading_order = (1..=media.page_count.max(0))
        .map(|page_number| WpLinkDto::resource(urls.raw_page(&book.id, page_number), Some(PDF_MEDIA_TYPE.to_owned())))
        .collect();
    publication.resources = thumbnail_links(book, urls);
    publication
}

/// Builds spine, asset, and navigation links from persisted EPUB analysis.
fn epub_manifest(
    book: &komga_core::BookDto,
    series: &SeriesMetadataDto,
    media: &Media,
    urls: &ManifestUrls,
) -> WpPublicationDto {
    let mut publication = base_publication(book, series, media, urls);
    publication.metadata.conforms_to = Some(EPUB_PROFILE.to_owned());
    publication.reading_order = media
        .files
        .iter()
        .filter(|file| file.sub_type.as_deref() == Some(EPUB_PAGE_SUBTYPE))
        .map(|file| WpLinkDto::resource(urls.resource(&book.id, &file.file_name), file.media_type.clone()))
        .collect();
    publication.resources = thumbnail_links(book, urls);
    publication.resources.extend(
        media
            .files
            .iter()
            .filter(|file| file.sub_type.as_deref() == Some(EPUB_ASSET_SUBTYPE))
            .map(|file| WpLinkDto::resource(urls.resource(&book.id, &file.file_name), file.media_type.clone())),
    );
    if let Some(MediaExtension::Epub(extension)) = media.extension.as_ref() {
        publication.metadata.rendition.insert(
            "layout".to_owned(),
            Value::String(if extension.is_fixed_layout {
                "fixed".to_owned()
            } else {
                "reflowable".to_owned()
            }),
        );
        publication.toc = navigation_links(&extension.toc, &book.id, urls);
        publication.landmarks = navigation_links(&extension.landmarks, &book.id, urls);
        publication.page_list = navigation_links(&extension.page_list, &book.id, urls);
    }
    publication
}

fn base_publication(
    book: &komga_core::BookDto,
    series: &SeriesMetadataDto,
    media: &Media,
    urls: &ManifestUrls,
) -> WpPublicationDto {
    let authors = group_authors(&book.metadata.authors);
    let metadata = WpMetadataDto {
        title: book.metadata.title.clone(),
        identifier: (!book.metadata.isbn.trim().is_empty()).then(|| format!("urn:isbn:{}", book.metadata.isbn)),
        publication_type: None,
        conforms_to: None,
        sort_as: None,
        subtitle: None,
        modified: Some(local_timestamp(&book.last_modified)),
        published: book.metadata.release_date.clone(),
        language: non_empty(&series.language),
        author: authors.author,
        translator: authors.translator,
        editor: authors.editor,
        artist: authors.artist,
        illustrator: authors.illustrator,
        letterer: authors.letterer,
        penciler: authors.penciler,
        colorist: authors.colorist,
        inker: authors.inker,
        contributor: authors.contributor,
        publisher: Vec::new(),
        subject: book.metadata.tags.iter().cloned().collect(),
        reading_progression: reading_progression(&series.reading_direction),
        description: non_empty(&book.metadata.summary),
        number_of_pages: Some(book.media.pages_count),
        belongs_to: Some(WpBelongsToDto {
            series: vec![WpContributorDto {
                name: book.series_title.clone(),
                position: Some(book.metadata.number_sort),
                links: Vec::new(),
            }],
            collection: Vec::new(),
        }),
        rendition: BTreeMap::new(),
    };
    WpPublicationDto {
        context: CONTEXT.to_owned(),
        metadata,
        links: publication_links(book, media, urls),
        images: Vec::new(),
        reading_order: Vec::new(),
        resources: Vec::new(),
        toc: Vec::new(),
        landmarks: Vec::new(),
        page_list: Vec::new(),
    }
}

fn publication_links(book: &komga_core::BookDto, media: &Media, urls: &ManifestUrls) -> Vec<WpLinkDto> {
    let profile = media.profile();
    let mut links = vec![WpLinkDto {
        rel: Some("self".to_owned()),
        href: Some(urls.manifest(&book.id)),
        media_type: Some(if profile == Some(MediaProfile::Divina) {
            DIVINA_MEDIA_TYPE.to_owned()
        } else {
            WEBPUB_MEDIA_TYPE.to_owned()
        }),
        ..WpLinkDto::default()
    }];
    if profile == Some(MediaProfile::Pdf) || (profile == Some(MediaProfile::Epub) && media.epub_divina_compatible) {
        links.push(WpLinkDto::resource(
            urls.profile_manifest(&book.id, "divina"),
            Some(DIVINA_MEDIA_TYPE.to_owned()),
        ));
    }
    links.push(WpLinkDto {
        rel: Some(ACQUISITION_REL.to_owned()),
        href: Some(urls.book_file(&book.id)),
        media_type: Some(export_media_type(media.media_type.as_deref().unwrap_or_default()).to_owned()),
        ..WpLinkDto::default()
    });
    links
}

fn thumbnail_links(book: &komga_core::BookDto, urls: &ManifestUrls) -> Vec<WpLinkDto> {
    vec![WpLinkDto::resource(
        urls.thumbnail(&book.id),
        Some("image/jpeg".to_owned()),
    )]
}

fn navigation_links(entries: &[komga_core::EpubTocEntry], book_id: &str, urls: &ManifestUrls) -> Vec<WpLinkDto> {
    entries
        .iter()
        .map(|entry| WpLinkDto {
            title: non_empty(&entry.title),
            href: entry
                .href
                .as_deref()
                .map(|href| urls.navigation_resource(book_id, href)),
            children: navigation_links(&entry.children, book_id, urls),
            ..WpLinkDto::default()
        })
        .collect()
}

fn reading_progression(value: &str) -> Option<WpReadingProgressionDto> {
    match value {
        "LEFT_TO_RIGHT" => Some(WpReadingProgressionDto::Ltr),
        "RIGHT_TO_LEFT" => Some(WpReadingProgressionDto::Rtl),
        "VERTICAL" | "WEBTOON" => Some(WpReadingProgressionDto::Ttb),
        _ => None,
    }
}

fn group_authors(authors: &[AuthorDto]) -> AuthorGroups {
    let role = |expected: &str| {
        authors
            .iter()
            .filter(|author| author.role == expected)
            .map(|author| author.name.clone())
            .collect::<Vec<_>>()
    };
    let mut penciler = role("penciler");
    penciler.extend(role("penciller"));
    let known_roles = [
        "author",
        "translator",
        "editor",
        "artist",
        "illustrator",
        "letterer",
        "penciler",
        "penciller",
        "colorist",
        "inker",
    ];
    AuthorGroups {
        author: role("author"),
        translator: role("translator"),
        editor: role("editor"),
        artist: role("artist"),
        illustrator: role("illustrator"),
        letterer: role("letterer"),
        penciler,
        colorist: role("colorist"),
        inker: role("inker"),
        contributor: authors
            .iter()
            .filter(|author| !known_roles.contains(&author.role.as_str()))
            .map(|author| author.name.clone())
            .collect(),
    }
}

fn export_media_type(media_type: &str) -> &str {
    match media_type {
        ZIP_MEDIA_TYPE => "application/vnd.comicbook+zip",
        RAR_MEDIA_TYPE | RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE => "application/vnd.comicbook-rar",
        value => value,
    }
}

fn is_recommended_image(media_type: &str) -> bool {
    matches!(media_type, "image/jpeg" | "image/png" | "image/gif")
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn local_timestamp(value: &str) -> String {
    DateTime::parse_from_rfc3339(value).map_or_else(
        |_| value.to_owned(),
        |timestamp| {
            timestamp
                .with_timezone(&Local)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        },
    )
}

fn first_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn forwarded_parameter(headers: &HeaderMap, name: &str) -> Option<String> {
    let forwarded = first_header(headers, "forwarded")?;
    forwarded.split(';').find_map(|parameter| {
        let (key, value) = parameter.trim().split_once('=')?;
        key.eq_ignore_ascii_case(name)
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_origin_and_prefix_build_encoded_urls() {
        let headers = HeaderMap::from_iter([
            (
                "forwarded".parse().unwrap(),
                "for=192.0.2.1;proto=HTTPS;host=reader.example:8443".parse().unwrap(),
            ),
            ("x-forwarded-prefix".parse().unwrap(), "/komga".parse().unwrap()),
        ]);
        let urls = ManifestUrls::from_request(&headers, &"/api/v1/books/book/manifest".parse().unwrap()).unwrap();
        assert_eq!(
            urls.resource("book id", "Text/Chapter 1.xhtml"),
            "https://reader.example:8443/komga/api/v1/books/book%20id/resource/Text/Chapter%201.xhtml"
        );
        assert_eq!(
            urls.navigation_resource("book id", "Text/Chapter 1.xhtml#part two"),
            "https://reader.example:8443/komga/api/v1/books/book%20id/resource/Text/Chapter%201.xhtml#part%20two"
        );
    }

    #[test]
    fn nested_empty_values_are_omitted_but_publication_arrays_remain() {
        let publication = WpPublicationDto {
            context: CONTEXT.into(),
            metadata: WpMetadataDto {
                title: "Book".into(),
                identifier: None,
                publication_type: None,
                conforms_to: Some(EPUB_PROFILE.into()),
                sort_as: None,
                subtitle: None,
                modified: None,
                published: None,
                language: None,
                author: Vec::new(),
                translator: Vec::new(),
                editor: Vec::new(),
                artist: Vec::new(),
                illustrator: Vec::new(),
                letterer: Vec::new(),
                penciler: Vec::new(),
                colorist: Vec::new(),
                inker: Vec::new(),
                contributor: Vec::new(),
                publisher: Vec::new(),
                subject: Vec::new(),
                reading_progression: None,
                description: None,
                number_of_pages: Some(0),
                belongs_to: None,
                rendition: BTreeMap::new(),
            },
            links: vec![WpLinkDto::default()],
            images: Vec::new(),
            reading_order: Vec::new(),
            resources: Vec::new(),
            toc: Vec::new(),
            landmarks: Vec::new(),
            page_list: Vec::new(),
        };
        let json = serde_json::to_value(publication).unwrap();
        assert_eq!(json["context"], CONTEXT);
        assert_eq!(json["metadata"]["numberOfPages"], 0);
        assert!(json["metadata"].get("author").is_none());
        assert_eq!(json["links"][0], serde_json::json!({}));
        for field in ["images", "readingOrder", "resources", "toc", "landmarks", "pageList"] {
            assert_eq!(json[field], serde_json::json!([]));
        }
    }

    #[test]
    fn media_type_helpers_match_kotlin_exports_and_conversion_support() {
        assert_eq!(export_media_type(ZIP_MEDIA_TYPE), "application/vnd.comicbook+zip");
        assert_eq!(export_media_type(RAR5_MEDIA_TYPE), "application/vnd.comicbook-rar");
        assert_eq!(export_media_type(PDF_MEDIA_TYPE), PDF_MEDIA_TYPE);
        assert!(can_convert_image_media_type("image/webp"));
        assert!(!can_convert_image_media_type("image/svg+xml"));
        assert!(DateTime::parse_from_rfc3339(&local_timestamp("2026-07-24T12:34:56Z")).is_ok());
    }

    #[test]
    fn author_roles_match_kotlin_webpub_groups() {
        let authors = [
            AuthorDto {
                name: "Penciller".into(),
                role: "penciller".into(),
            },
            AuthorDto {
                name: "Author".into(),
                role: "author".into(),
            },
            AuthorDto {
                name: "Writer".into(),
                role: "writer".into(),
            },
            AuthorDto {
                name: "Penciler".into(),
                role: "penciler".into(),
            },
            AuthorDto {
                name: "Cover".into(),
                role: "cover".into(),
            },
        ];
        let groups = group_authors(&authors);
        assert_eq!(groups.author, ["Author"]);
        assert_eq!(groups.penciler, ["Penciler", "Penciller"]);
        assert_eq!(groups.contributor, ["Writer", "Cover"]);
    }
}
