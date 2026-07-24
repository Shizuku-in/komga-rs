//! Media analysis, extraction, and image operations for Komga books.

#![warn(missing_docs)]

use chrono::NaiveDate;
use std::{collections::BTreeSet, io};
use thiserror::Error;

use komga_core::{
    Author, Book, BookMetadataPatch, BookPage, Dimension, EPUB_MEDIA_TYPE, JPEG_MEDIA_TYPE, Media, MediaProfile,
    MediaStatus, MetadataUpdate, PDF_MEDIA_TYPE, ReadingDirection, Series, SeriesMetadataPatch, SeriesStatus, WebLink,
    ZIP_MEDIA_TYPE,
};

mod archive;
mod comicrack;
mod epub;
mod image_ops;
mod pdf;

pub use comicrack::{ComicRackListError, parse_read_list};

/// Container relationships declared by a root-level `ComicInfo.xml` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComicInfoContainers {
    /// Collection names from `SeriesGroup`, in declaration order.
    pub collections: Vec<String>,
    /// Read-list names and optional requested positions.
    pub read_lists: Vec<ComicInfoReadList>,
}

/// One read-list relationship declared by ComicInfo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComicInfoReadList {
    /// Read-list name.
    pub name: String,
    /// Requested integer position, or `None` when ComicInfo did not provide one.
    pub number: Option<i32>,
}

/// Book and series patches produced by an embedded metadata provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmbeddedMetadata {
    /// Editable book fields supplied by the provider.
    pub book: BookMetadataPatch,
    /// Editable series fields supplied by the provider.
    pub series: SeriesMetadataPatch,
    /// Incremental container relationships supplied by ComicInfo.
    pub containers: ComicInfoContainers,
}

const COMIC_INFO_MAX_SIZE: i64 = 1024 * 1024;

/// Extracts and parses the analyzed root-level `ComicInfo.xml` resource.
///
/// The entry must have been declared by media analysis. DTD and entity
/// declarations are rejected before structured XML parsing.
///
/// # Errors
///
/// Returns an operation error when the declared entry cannot be extracted or
/// its XML is malformed or unsafe.
pub fn extract_comic_info_containers(
    book: &Book,
    media: &Media,
) -> Result<Option<ComicInfoContainers>, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Ok(None);
    }
    let Some(file) = media.files.iter().find(|file| file.file_name == "ComicInfo.xml") else {
        return Ok(None);
    };
    if file
        .file_size
        .is_some_and(|size| !(0..=COMIC_INFO_MAX_SIZE).contains(&size))
    {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml exceeds the 1 MiB limit".into(),
        ));
    }
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    let bytes = match media.profile() {
        Some(MediaProfile::Divina) if media.media_type.as_deref() == Some(ZIP_MEDIA_TYPE) => {
            archive::extract_zip_entry(&path, "ComicInfo.xml")?
        }
        Some(MediaProfile::Divina) => archive::extract_rar_entry(&path, "ComicInfo.xml")?,
        Some(MediaProfile::Epub) => epub::extract(&path, "ComicInfo.xml").map_err(MediaOperationError::Read)?,
        _ => return Ok(None),
    };
    if bytes.len() > usize::try_from(COMIC_INFO_MAX_SIZE).unwrap_or(usize::MAX) {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml exceeds the 1 MiB limit".into(),
        ));
    }
    parse_comic_info_containers(&bytes).map(Some)
}

/// Extracts all supported ComicInfo metadata from an analyzed book.
pub fn extract_comic_info_metadata(
    book: &Book,
    media: &Media,
    append_volume: bool,
) -> Result<Option<EmbeddedMetadata>, MediaOperationError> {
    let Some(bytes) = extract_comic_info_bytes(book, media)? else {
        return Ok(None);
    };
    parse_comic_info_metadata(&bytes, append_volume).map(Some)
}

/// Extracts OPF metadata from the EPUB container declaration.
pub fn extract_epub_metadata(book: &Book, media: &Media) -> Result<Option<EmbeddedMetadata>, MediaOperationError> {
    if media.status != MediaStatus::Ready || media.profile() != Some(MediaProfile::Epub) {
        return Ok(None);
    }
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".into()))?;
    let xml = epub::package::metadata_xml(&path).map_err(MediaOperationError::Read)?;
    parse_epub_metadata(&xml).map(Some)
}

/// Parses a Mylar `series.json` sidecar from a non-one-shot series root.
pub fn extract_mylar_metadata(series: &Series) -> Result<Option<SeriesMetadataPatch>, MediaOperationError> {
    if series.oneshot {
        return Ok(None);
    }
    let directory = series
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("series URL is not a file URL".into()))?;
    let path = directory.join("series.json");
    if !path.is_file() {
        return Ok(None);
    }
    #[derive(serde::Deserialize)]
    struct Mylar {
        #[serde(default, alias = "series", alias = "title")]
        name: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default, alias = "description_text", alias = "summary")]
        description: Option<String>,
        #[serde(default)]
        publisher: Option<String>,
        #[serde(default, alias = "content_rating")]
        age_rating: Option<serde_json::Value>,
        #[serde(default, alias = "total_issues", alias = "issue_count")]
        total_book_count: Option<i32>,
    }
    let bytes = std::fs::read(path).map_err(|e| MediaOperationError::Read(e.to_string()))?;
    let value: Mylar = serde_json::from_slice(&bytes).map_err(|e| MediaOperationError::Read(e.to_string()))?;
    let status = value
        .status
        .as_deref()
        .and_then(|v| match v.to_ascii_lowercase().as_str() {
            "ended" | "complete" | "completed" => Some(SeriesStatus::Ended),
            "continuing" | "ongoing" => Some(SeriesStatus::Ongoing),
            "hiatus" => Some(SeriesStatus::Hiatus),
            "abandoned" | "cancelled" => Some(SeriesStatus::Abandoned),
            _ => None,
        });
    let age = value.age_rating.and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_i64().and_then(|n| i32::try_from(n).ok()),
        serde_json::Value::String(s) => comic_age_rating(&s),
        _ => None,
    });
    Ok(Some(SeriesMetadataPatch {
        status,
        title: value.name.filter(|v| !v.trim().is_empty()),
        summary: value.description,
        publisher: value.publisher,
        age_rating: age.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        total_book_count: value
            .total_book_count
            .filter(|v| *v > 0)
            .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        ..Default::default()
    }))
}

fn extract_comic_info_bytes(book: &Book, media: &Media) -> Result<Option<Vec<u8>>, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Ok(None);
    }
    let Some(file) = media.files.iter().find(|file| file.file_name == "ComicInfo.xml") else {
        return Ok(None);
    };
    if file
        .file_size
        .is_some_and(|size| !(0..=COMIC_INFO_MAX_SIZE).contains(&size))
    {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml exceeds the 1 MiB limit".into(),
        ));
    }
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".into()))?;
    let bytes = match media.profile() {
        Some(MediaProfile::Divina) if media.media_type.as_deref() == Some(ZIP_MEDIA_TYPE) => {
            archive::extract_zip_entry(&path, "ComicInfo.xml")?
        }
        Some(MediaProfile::Divina) => archive::extract_rar_entry(&path, "ComicInfo.xml")?,
        Some(MediaProfile::Epub) => epub::extract(&path, "ComicInfo.xml").map_err(MediaOperationError::Read)?,
        _ => return Ok(None),
    };
    if bytes.len() > COMIC_INFO_MAX_SIZE as usize {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml exceeds the 1 MiB limit".into(),
        ));
    }
    Ok(Some(bytes))
}

fn parse_comic_info_containers(bytes: &[u8]) -> Result<ComicInfoContainers, MediaOperationError> {
    let xml = std::str::from_utf8(bytes).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let upper = xml.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml contains a DTD or entity declaration".into(),
        ));
    }
    let document = roxmltree::Document::parse(xml).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let values = |name: &str| {
        document
            .root_element()
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == name)
            .filter_map(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
    };
    let mut collections = Vec::new();
    for value in values("SeriesGroup") {
        for name in value.split(',').map(str::trim).filter(|name| !name.is_empty()) {
            if !collections.iter().any(|existing| existing == name) {
                collections.push(name.to_owned());
            }
        }
    }
    let mut read_lists = Vec::new();
    if let Some(name) = values("AlternateSeries").first() {
        let number = values("AlternateNumber").first().and_then(|value| value.parse().ok());
        push_read_list(&mut read_lists, name, number);
    }
    let arcs = values("StoryArc")
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .map(|value| (!value.is_empty()).then_some(value))
        .collect::<Vec<_>>();
    let numbers = values("StoryArcNumber")
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .collect::<Vec<_>>();
    if numbers.is_empty() {
        for name in arcs.into_iter().flatten() {
            push_read_list(&mut read_lists, name, None);
        }
    } else {
        for (name, number) in arcs
            .into_iter()
            .zip(numbers)
            .filter_map(|(name, number)| name.zip(number.parse::<i32>().ok()))
        {
            push_read_list(&mut read_lists, name, Some(number));
        }
    }
    Ok(ComicInfoContainers {
        collections,
        read_lists,
    })
}

fn parse_comic_info_metadata(bytes: &[u8], append_volume: bool) -> Result<EmbeddedMetadata, MediaOperationError> {
    let xml = std::str::from_utf8(bytes).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let upper = xml.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml contains a DTD or entity declaration".into(),
        ));
    }
    let doc = roxmltree::Document::parse(xml).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let root = doc.root_element();
    let text = |name: &str| {
        root.children()
            .find(|n| n.is_element() && n.tag_name().name() == name)
            .and_then(|n| n.text())
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let list = |name: &str| {
        text(name)
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut authors = Vec::new();
    for (field, role) in [
        ("Writer", "writer"),
        ("Penciller", "penciller"),
        ("Inker", "inker"),
        ("Colorist", "colorist"),
        ("Letterer", "letterer"),
        ("CoverArtist", "cover"),
        ("Editor", "editor"),
        ("Translator", "translator"),
    ] {
        for name in list(field) {
            authors.push(Author {
                name,
                role: role.into(),
            });
        }
    }
    let release_date = match (text("Year"), text("Month"), text("Day")) {
        (Some(y), month, day) => Some(
            NaiveDate::from_ymd_opt(
                y.parse()
                    .map_err(|_| MediaOperationError::Read("invalid ComicInfo date".into()))?,
                month
                    .unwrap_or("1")
                    .parse()
                    .map_err(|_| MediaOperationError::Read("invalid ComicInfo date".into()))?,
                day.unwrap_or("1")
                    .parse()
                    .map_err(|_| MediaOperationError::Read("invalid ComicInfo date".into()))?,
            )
            .ok_or_else(|| MediaOperationError::Read("invalid ComicInfo date".into()))?,
        ),
        _ => None,
    };
    let number_sort = text("Number").and_then(|v| v.parse::<f64>().ok());
    let isbn = text("GTIN").or_else(|| text("ISBN")).map(isbn13).transpose()?;
    let links = text("Web").map(|url| {
        vec![WebLink {
            label: "Web".into(),
            url: url.into(),
        }]
    });
    let title = text("Series").map(str::to_owned).map(|mut title| {
        if append_volume && let Some(volume) = text("Volume") {
            title.push_str(" Vol. ");
            title.push_str(volume);
        }
        title
    });
    let direction = text("Manga").and_then(|value| match value.to_ascii_lowercase().as_str() {
        "yesandrighttoleft" | "righttoleft" => Some(ReadingDirection::RightToLeft),
        "yes" => Some(ReadingDirection::RightToLeft),
        "no" => Some(ReadingDirection::LeftToRight),
        _ => None,
    });
    let age = text("AgeRating").and_then(comic_age_rating);
    let total = text("Count").and_then(|v| v.parse::<i32>().ok()).filter(|v| *v > 0);
    Ok(EmbeddedMetadata {
        book: BookMetadataPatch {
            title: text("Title").map(str::to_owned),
            summary: update(text("Summary").map(str::to_owned)),
            number: text("Number").map(str::to_owned),
            number_sort,
            release_date: release_date.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            authors: (!authors.is_empty())
                .then_some(authors)
                .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            tags: set_update(list("Tags")),
            isbn: isbn.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            links: links.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            ..Default::default()
        },
        series: SeriesMetadataPatch {
            title,
            summary: text("Summary").map(str::to_owned),
            reading_direction: direction.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            publisher: text("Publisher").map(str::to_owned),
            age_rating: age.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            language: text("LanguageISO").map(str::to_owned),
            genres: set_update(list("Genre")),
            total_book_count: total.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            ..Default::default()
        },
        containers: parse_comic_info_containers(bytes)?,
    })
}

fn parse_epub_metadata(xml: &str) -> Result<EmbeddedMetadata, MediaOperationError> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| MediaOperationError::Read(e.to_string()))?;
    let metadata = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "metadata")
        .ok_or_else(|| MediaOperationError::Read("OPF does not contain metadata".into()))?;
    let values = |name: &str| {
        metadata
            .descendants()
            .filter(move |n| n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name))
            .filter_map(|n| n.text())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>()
    };
    let first = |name: &str| values(name).first().copied();
    let mut authors = Vec::new();
    for node in metadata
        .descendants()
        .filter(|n| n.is_element() && matches!(n.tag_name().name(), "creator" | "contributor"))
    {
        let Some(name) = node.text().map(str::trim).filter(|v| !v.is_empty()) else {
            continue;
        };
        let role = node
            .attributes()
            .find(|a| a.name().eq_ignore_ascii_case("role"))
            .map(|a| marc_role(a.value()))
            .unwrap_or(if node.tag_name().name() == "creator" {
                "writer"
            } else {
                "contributor"
            });
        authors.push(Author {
            name: name.into(),
            role: role.into(),
        });
    }
    let date = first("date").map(parse_date).transpose()?;
    let isbn = values("identifier")
        .into_iter()
        .find(|v| {
            let length = v
                .chars()
                .filter(|c| c.is_ascii_digit() || matches!(c, 'x' | 'X'))
                .count();
            v.to_ascii_lowercase().contains("isbn") || matches!(length, 10 | 13)
        })
        .map(isbn13)
        .transpose()?;
    let series = metadata
        .descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "meta"
                && (n.attribute("name") == Some("calibre:series")
                    || n.attribute("property") == Some("belongs-to-collection"))
        })
        .and_then(|n| n.attribute("content").or_else(|| n.text()))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let series_index = metadata
        .descendants()
        .find(|n| {
            n.is_element()
                && n.tag_name().name() == "meta"
                && (n.attribute("name") == Some("calibre:series_index")
                    || n.attribute("property") == Some("group-position"))
        })
        .and_then(|n| n.attribute("content").or_else(|| n.text()))
        .and_then(|v| v.trim().parse::<f64>().ok());
    let subjects = values("subject").into_iter().map(str::to_owned).collect::<Vec<_>>();
    let description = first("description").map(strip_html);
    let direction = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "spine")
        .and_then(|n| n.attribute("page-progression-direction"))
        .and_then(|v| match v {
            "rtl" => Some(ReadingDirection::RightToLeft),
            "ltr" => Some(ReadingDirection::LeftToRight),
            _ => None,
        });
    Ok(EmbeddedMetadata {
        book: BookMetadataPatch {
            title: first("title").map(str::to_owned),
            summary: update(description.clone()),
            number: series_index.map(|v| v.to_string()),
            number_sort: series_index,
            release_date: date.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            authors: (!authors.is_empty())
                .then_some(authors)
                .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            tags: set_update(subjects.clone()),
            isbn: isbn.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            ..Default::default()
        },
        series: SeriesMetadataPatch {
            title: series,
            summary: description,
            publisher: first("publisher").map(str::to_owned),
            language: first("language").map(str::to_owned),
            genres: set_update(subjects),
            reading_direction: direction.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            ..Default::default()
        },
        containers: ComicInfoContainers::default(),
    })
}

fn update<T>(value: Option<T>) -> MetadataUpdate<T> {
    value.map_or(MetadataUpdate::Keep, MetadataUpdate::Set)
}
fn set_update(values: Vec<String>) -> MetadataUpdate<BTreeSet<String>> {
    if values.is_empty() {
        MetadataUpdate::Keep
    } else {
        MetadataUpdate::Set(values.into_iter().collect())
    }
}
fn strip_html(value: &str) -> String {
    scraper::Html::parse_fragment(value)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn parse_date(value: &str) -> Result<NaiveDate, MediaOperationError> {
    if let Ok(date) = NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d") {
        return Ok(date);
    }
    if let Some((year, month)) = value.split_once('-')
        && let (Ok(year), Ok(month)) = (year.parse(), month.get(..2).unwrap_or(month).parse())
        && let Some(date) = NaiveDate::from_ymd_opt(year, month, 1)
    {
        return Ok(date);
    }
    if let Ok(year) = value.parse()
        && let Some(date) = NaiveDate::from_ymd_opt(year, 1, 1)
    {
        return Ok(date);
    }
    Err(MediaOperationError::Read("invalid OPF date".into()))
}
fn marc_role(value: &str) -> &str {
    match value.to_ascii_lowercase().as_str() {
        "aut" => "writer",
        "art" | "ill" => "artist",
        "edt" => "editor",
        "trl" => "translator",
        _ => "contributor",
    }
}
fn comic_age_rating(value: &str) -> Option<i32> {
    match value.to_ascii_lowercase().as_str() {
        "adults only 18+" => Some(18),
        "mature 17+" => Some(17),
        "teen 13+" => Some(13),
        "everyone 10+" => Some(10),
        "everyone" => Some(0),
        _ => value
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .ok(),
    }
}
fn isbn13(value: &str) -> Result<String, MediaOperationError> {
    let raw = value.rsplit(':').next().unwrap_or(value);
    let cleaned = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == 'X' || *c == 'x')
        .collect::<String>();
    let digits = if cleaned.len() == 10 {
        let checksum: u32 = cleaned
            .bytes()
            .enumerate()
            .map(|(i, b)| {
                let digit = if i == 9 && matches!(b, b'X' | b'x') {
                    10
                } else {
                    u32::from(b.saturating_sub(b'0'))
                };
                digit * u32::try_from(10 - i).unwrap_or(0)
            })
            .sum();
        if !checksum.is_multiple_of(11) {
            return Err(MediaOperationError::Read("invalid ISBN".into()));
        }
        let base = format!("978{}", &cleaned[..9]);
        let sum: u32 = base
            .bytes()
            .enumerate()
            .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 })
            .sum();
        format!("{base}{}", (10 - sum % 10) % 10)
    } else {
        cleaned
    };
    if digits.len() != 13 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(MediaOperationError::Read("invalid ISBN".into()));
    }
    let checksum: u32 = digits
        .bytes()
        .enumerate()
        .map(|(i, b)| u32::from(b - b'0') * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    if !checksum.is_multiple_of(10) {
        return Err(MediaOperationError::Read("invalid ISBN".into()));
    }
    Ok(digits)
}

fn push_read_list(read_lists: &mut Vec<ComicInfoReadList>, name: &str, number: Option<i32>) {
    if !read_lists.iter().any(|existing| existing.name == name) {
        read_lists.push(ComicInfoReadList {
            name: name.to_owned(),
            number,
        });
    }
}

/// Binary content paired with the media type used by HTTP responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedBytes {
    /// Encoded content.
    pub bytes: Vec<u8>,
    /// IANA media type for the encoded content.
    pub media_type: String,
}

/// Supported image output encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOutput {
    /// JPEG with a white background for transparent input.
    Jpeg,
    /// PNG preserving transparency.
    Png,
}

impl ImageOutput {
    /// Parses the `convert` query parameter accepted by Komga.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

/// Errors that interrupt analysis without producing a persisted media result.
#[derive(Debug, Error)]
pub enum MediaAnalysisError {
    /// Server shutdown was requested between archive entries.
    #[error("media analysis cancelled")]
    Cancelled,
}

/// Errors produced while reading or converting an already analyzed page.
#[derive(Debug, Error)]
pub enum MediaOperationError {
    /// The media row is not ready for page access.
    #[error("book media is not ready")]
    NotReady,
    /// The requested one-based page number is outside the book.
    #[error("page number does not exist")]
    PageOutOfBounds,
    /// The requested operation is not supported by this media profile.
    #[error("book media profile does not support this operation")]
    UnsupportedProfile,
    /// The requested analyzed resource does not exist.
    #[error("book resource does not exist")]
    ResourceNotFound,
    /// The source file or archive entry could not be read.
    #[error("could not read book content: {0}")]
    Read(String),
    /// The source image cannot be decoded or encoded.
    #[error("could not convert image: {0}")]
    Image(String),
}

/// Detects and analyzes a book using Kotlin-compatible media state transitions.
///
/// ZIP/CBZ, RAR/CBR, PDF, and EPUB books are analyzed without external
/// commands.
///
/// # Errors
///
/// Returns [`MediaAnalysisError::Cancelled`] when the callback requests stop.
pub fn analyze_book(
    book: &Book,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Media, MediaAnalysisError> {
    if cancelled() {
        return Err(MediaAnalysisError::Cancelled);
    }
    let path = match book.url.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            return Ok(Media::result(&book.id, MediaStatus::Error, None, Some("ERR_1005")));
        }
    };
    let mut detected = match infer::get_from_path(&path) {
        Ok(Some(kind)) => archive::normalize_media_type(kind.mime_type()).to_owned(),
        Ok(None) => "application/octet-stream".to_owned(),
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => "ERR_1018",
                std::io::ErrorKind::PermissionDenied => "ERR_1000",
                _ => "ERR_1005",
            };
            return Ok(Media::result(&book.id, MediaStatus::Error, None, Some(code)));
        }
    };

    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("epub"))
        && detected != EPUB_MEDIA_TYPE
    {
        if epub::is_epub(&path) {
            detected = EPUB_MEDIA_TYPE.to_owned();
        } else {
            return Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(detected),
                Some("ERR_1032"),
            ));
        }
    }

    if detected == EPUB_MEDIA_TYPE {
        return match epub::analyze(&path, &book.id, analyze_dimensions, cancelled) {
            Ok(media) => Ok(media),
            Err(epub::AnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(epub::AnalysisError::Invalid) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(EPUB_MEDIA_TYPE.to_owned()),
                Some("ERR_1005"),
            )),
        };
    }

    if detected == PDF_MEDIA_TYPE {
        return match pdf::analyze(&path, &book.id, analyze_dimensions, cancelled) {
            Ok(media) => Ok(media),
            Err(pdf::AnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(pdf::AnalysisError::Io(kind)) => {
                let code = match kind {
                    io::ErrorKind::NotFound => "ERR_1018",
                    io::ErrorKind::PermissionDenied => "ERR_1000",
                    _ => "ERR_1005",
                };
                Ok(Media::result(
                    &book.id,
                    MediaStatus::Error,
                    Some(PDF_MEDIA_TYPE.to_owned()),
                    Some(code),
                ))
            }
            Err(pdf::AnalysisError::Invalid) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(PDF_MEDIA_TYPE.to_owned()),
                Some("ERR_1005"),
            )),
        };
    }

    if detected == ZIP_MEDIA_TYPE {
        return match archive::analyze_zip(&path, analyze_dimensions, cancelled) {
            Ok(analysis) => Ok(archive::media_from_analysis(&book.id, ZIP_MEDIA_TYPE, analysis)),
            Err(archive::ZipAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(archive::ZipAnalysisError::Archive) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(ZIP_MEDIA_TYPE.to_owned()),
                Some("ERR_1008"),
            )),
        };
    }

    if archive::is_rar_media_type(&detected) {
        return match archive::analyze_rar(&path, analyze_dimensions, cancelled) {
            Ok(analysis) => Ok(archive::media_from_analysis(
                &book.id,
                analysis.media_type,
                analysis.contents,
            )),
            Err(archive::RarAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(archive::RarAnalysisError::Encrypted) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(detected),
                Some("ERR_1002"),
            )),
            Err(archive::RarAnalysisError::MultiVolume) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(detected),
                Some("ERR_1004"),
            )),
            Err(archive::RarAnalysisError::Archive) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(detected),
                Some("ERR_1008"),
            )),
        };
    }

    Ok(Media::result(
        &book.id,
        MediaStatus::Unsupported,
        Some(detected),
        Some("ERR_1001"),
    ))
}

/// Returns the media type produced when a stored page is delivered.
///
/// PDF pages are persisted without an image media type and render as JPEG.
pub fn page_output_media_type<'a>(media: &Media, page: &'a BookPage) -> &'a str {
    if media.profile() == Some(MediaProfile::Pdf) {
        JPEG_MEDIA_TYPE
    } else {
        &page.media_type
    }
}

/// Returns the dimensions produced when a stored page is delivered.
///
/// PDF crop-box dimensions are scaled with the same policy as the renderer.
pub fn page_output_dimension(media: &Media, page: &BookPage) -> Option<Dimension> {
    if media.profile() == Some(MediaProfile::Pdf) {
        page.dimension.and_then(pdf::output_dimension)
    } else {
        page.dimension
    }
}

/// Extracts one rasterized page from a ready ZIP-, RAR-, PDF-, or fixed-layout
/// EPUB-backed record.
///
/// # Errors
///
/// Returns an error for non-ready media, invalid page numbers, missing files,
/// encrypted entries, unsupported compression methods, or corrupt documents.
pub fn extract_page(book: &Book, media: &Media, page_number: i32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Err(MediaOperationError::NotReady);
    }
    if page_number <= 0 {
        return Err(MediaOperationError::PageOutOfBounds);
    }
    let page = media
        .pages
        .get(usize::try_from(page_number - 1).map_err(|_| MediaOperationError::PageOutOfBounds)?)
        .ok_or(MediaOperationError::PageOutOfBounds)?;
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    match media.profile() {
        Some(MediaProfile::Pdf) => {
            return pdf::render_page(&path, usize::try_from(page_number - 1).unwrap_or(usize::MAX));
        }
        Some(MediaProfile::Epub) if media.epub_divina_compatible => {
            return epub::extract(&path, &page.file_name)
                .map(|bytes| TypedBytes {
                    bytes,
                    media_type: page.media_type.clone(),
                })
                .map_err(MediaOperationError::Read);
        }
        Some(MediaProfile::Epub) => return Err(MediaOperationError::UnsupportedProfile),
        _ => {}
    }
    let bytes = match media.media_type.as_deref() {
        Some(ZIP_MEDIA_TYPE) => archive::extract_zip_entry(&path, &page.file_name)?,
        Some(media_type) if archive::is_rar_media_type(media_type) => {
            archive::extract_rar_entry(&path, &page.file_name)?
        }
        _ => return Err(MediaOperationError::NotReady),
    };
    Ok(TypedBytes {
        bytes,
        media_type: page.media_type.clone(),
    })
}

/// Extracts one declared resource from a ready EPUB.
///
/// Resource names must exactly match an analyzed `MEDIA_FILE` entry. This
/// prevents callers from using the API as an unrestricted ZIP reader.
///
/// # Errors
///
/// Returns an error when the media is not ready, is not EPUB, the resource was
/// not declared during analysis, or the archive entry cannot be read.
pub fn extract_resource(book: &Book, media: &Media, resource: &str) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Err(MediaOperationError::NotReady);
    }
    if media.profile() != Some(MediaProfile::Epub) {
        return Err(MediaOperationError::UnsupportedProfile);
    }
    let file = media
        .files
        .iter()
        .find(|file| file.file_name == resource)
        .ok_or(MediaOperationError::ResourceNotFound)?;
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    let bytes = epub::extract(&path, resource).map_err(MediaOperationError::Read)?;
    Ok(TypedBytes {
        bytes,
        media_type: file
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_owned()),
    })
}

/// Extracts one PDF page as a standalone PDF document.
///
/// # Errors
///
/// Returns an error when media is not a ready PDF, the page is out of bounds,
/// or the source document cannot be read or rewritten.
pub fn extract_raw_page(book: &Book, media: &Media, page_number: i32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready || media.profile() != Some(MediaProfile::Pdf) {
        return Err(MediaOperationError::NotReady);
    }
    if page_number <= 0 {
        return Err(MediaOperationError::PageOutOfBounds);
    }
    let index = usize::try_from(page_number - 1).map_err(|_| MediaOperationError::PageOutOfBounds)?;
    media.pages.get(index).ok_or(MediaOperationError::PageOutOfBounds)?;
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    pdf::extract_page(&path, index)
}

/// Converts and optionally downsizes encoded image bytes.
///
/// `max_edge` preserves aspect ratio and never enlarges the source image.
///
/// # Errors
///
/// Returns [`MediaOperationError::Image`] when decoding or encoding fails.
pub fn convert_image(
    source: &[u8],
    output: ImageOutput,
    max_edge: Option<u32>,
) -> Result<TypedBytes, MediaOperationError> {
    image_ops::convert(source, output, max_edge)
}

/// Reports whether the linked image pipeline can decode a media type for
/// conversion to JPEG or PNG.
pub fn can_convert_image_media_type(media_type: &str) -> bool {
    image_ops::can_decode_media_type(media_type)
}

/// Generates a JPEG poster from an EPUB package cover or the first page.
///
/// # Errors
///
/// Returns page extraction or image conversion errors unchanged.
pub fn generate_thumbnail(book: &Book, media: &Media, max_edge: u32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Err(MediaOperationError::NotReady);
    }
    let cover = if media.profile() == Some(MediaProfile::Epub) {
        let path = book
            .url
            .to_file_path()
            .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
        epub::cover(&path)
            .map_err(MediaOperationError::Read)?
            .map_or_else(|| extract_page(book, media, 1), Ok)?
    } else {
        extract_page(book, media, 1)?
    };
    convert_image(&cover.bytes, ImageOutput::Jpeg, Some(max_edge))
}

/// Reads the first image frame's dimensions without decoding all pixels.
pub fn image_dimension(bytes: &[u8]) -> Option<Dimension> {
    image_ops::dimension(bytes)
}

/// Builds a two-by-two JPEG poster, cycling one to four source images.
pub fn collage_thumbnail(sources: &[Vec<u8>], size: u32) -> Result<Vec<u8>, MediaOperationError> {
    image_ops::collage(sources, size)
}

/// Detects an image media type from encoded bytes.
pub fn detect_image_media_type(bytes: &[u8]) -> Option<String> {
    infer::get(bytes)
        .filter(|kind| kind.mime_type().starts_with("image/"))
        .map(|kind| kind.mime_type().to_owned())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        fs::File,
        io::{Cursor, Write},
        path::{Path, PathBuf},
    };

    use hayro::hayro_syntax::Pdf;
    use image::{DynamicImage, GenericImageView, ImageFormat};
    use pdf_writer::Ref;
    use rars::{
        ArchiveVersion, FeatureSet,
        rar15_40::{StoredEntry, WriterOptions, write_stored_volumes},
    };
    use url::Url;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn png() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(48, 32, image::Rgba([10, 20, 30, 128])));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn book(path: &Path) -> Book {
        Book {
            id: "book".into(),
            name: "Volume".into(),
            url: Url::from_file_path(path).unwrap(),
            file_last_modified: chrono::Local::now().naive_local(),
            file_size: 0,
            file_hash: String::new(),
            file_hash_koreader: String::new(),
            number: 1,
            series_id: "series".into(),
            library_id: "library".into(),
            deleted_date: None,
            oneshot: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    fn kotlin_archive(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/archives")
            .join(name)
    }

    fn kotlin_pdf() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/pdf/komga.pdf")
    }

    fn kotlin_epub(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/epub")
            .join(name)
    }

    fn blank_pdf(page_sizes: &[(f32, f32)]) -> Vec<u8> {
        let mut pdf = pdf_writer::Pdf::new();
        let catalog_ref = Ref::new(1);
        let pages_ref = Ref::new(2);
        let page_refs = (0..page_sizes.len())
            .map(|index| Ref::new(i32::try_from(index + 3).unwrap()))
            .collect::<Vec<_>>();
        pdf.catalog(catalog_ref).pages(pages_ref);
        pdf.pages(pages_ref)
            .kids(page_refs.iter().copied())
            .count(i32::try_from(page_refs.len()).unwrap());
        for (page_ref, (width, height)) in page_refs.into_iter().zip(page_sizes.iter().copied()) {
            let mut page = pdf.page(page_ref);
            page.parent(pages_ref);
            page.media_box(pdf_writer::Rect::new(0.0, 0.0, width, height));
            page.resources();
        }
        pdf.finish()
    }

    fn write_epub(path: &Path, opf: &str, entries: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        writer.start_file("mimetype", options).unwrap();
        writer.write_all(EPUB_MEDIA_TYPE.as_bytes()).unwrap();
        writer.start_file("META-INF/container.xml", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            )
            .unwrap();
        writer.start_file("content.opf", options).unwrap();
        writer.write_all(opf.as_bytes()).unwrap();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn analyzes_naturally_sorted_zip_pages_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default();
        writer.start_file("10.png", options).unwrap();
        writer.write_all(&png()).unwrap();
        writer.start_file("notes.txt", options).unwrap();
        writer.write_all(b"notes").unwrap();
        writer.start_file("2.png", options).unwrap();
        writer.write_all(&png()).unwrap();
        writer.finish().unwrap();

        let media = analyze_book(&book(&path), true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.page_count, 2);
        assert_eq!(media.pages[0].file_name, "2.png");
        assert_eq!(media.pages[0].dimension, Some(Dimension { width: 48, height: 32 }));
        assert_eq!(media.pages[1].file_name, "10.png");
        assert_eq!(media.files[0].file_name, "notes.txt");
    }

    #[test]
    fn reports_empty_and_missing_books_with_kotlin_codes() {
        let temporary = tempfile::tempdir().unwrap();
        let empty_path = temporary.path().join("empty.cbz");
        let mut writer = ZipWriter::new(File::create(&empty_path).unwrap());
        writer.start_file("notes.txt", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"notes").unwrap();
        writer.finish().unwrap();
        let empty = analyze_book(&book(&empty_path), true, || false).unwrap();
        assert_eq!(empty.status, MediaStatus::Error);
        assert_eq!(empty.comment.as_deref(), Some("ERR_1006"));

        let missing = temporary.path().join("missing.cbz");
        let missing = analyze_book(&book(&missing), true, || false).unwrap();
        assert_eq!(missing.status, MediaStatus::Error);
        assert_eq!(missing.comment.as_deref(), Some("ERR_1018"));
    }

    #[test]
    fn converts_transparent_pages_without_upscaling() {
        let converted = convert_image(&png(), ImageOutput::Jpeg, Some(300)).unwrap();
        let decoded = image::load_from_memory(&converted.bytes).unwrap();
        assert_eq!(converted.media_type, "image/jpeg");
        assert_eq!(decoded.dimensions(), (48, 32));
        assert!(!decoded.color().has_alpha());
    }

    #[test]
    fn cancellation_stops_before_opening_the_archive() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        fs::write(&path, []).unwrap();
        assert!(matches!(
            analyze_book(&book(&path), true, || true),
            Err(MediaAnalysisError::Cancelled)
        ));
    }

    #[test]
    fn reads_kotlin_zip_compression_fixtures() {
        for name in [
            "zip.zip",
            "zip-bzip2.zip",
            "zip-copy.zip",
            "zip-deflate64.zip",
            "zip-lzma.zip",
            "zip-ppmd.zip",
        ] {
            let media = analyze_book(&book(&kotlin_archive(name)), false, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Ready, "{name}");
            assert_eq!(media.page_count, 1, "{name}");
            assert_eq!(media.pages[0].dimension, None, "{name}");
        }

        let encrypted = analyze_book(&book(&kotlin_archive("zip-encrypted.zip")), false, || false).unwrap();
        assert_eq!(encrypted.status, MediaStatus::Error);
    }

    #[test]
    fn reads_kotlin_rar_and_solid_fixtures() {
        for (name, media_type, page_count) in [
            ("rar4.rar", "application/x-rar-compressed; version=4", 3),
            ("rar5.rar", "application/x-rar-compressed; version=5", 1),
            ("rar4-solid.rar", "application/x-rar-compressed; version=4", 3),
            ("rar5-solid.rar", "application/x-rar-compressed; version=5", 3),
        ] {
            let source = book(&kotlin_archive(name));
            let media = analyze_book(&source, true, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Ready, "{name}");
            assert_eq!(media.media_type.as_deref(), Some(media_type), "{name}");
            assert_eq!(media.page_count, page_count, "{name}");
            assert!(
                media
                    .pages
                    .iter()
                    .all(|page| page.dimension == Some(Dimension { width: 48, height: 48 })),
                "{name}"
            );
            let extracted = extract_page(&source, &media, page_count).unwrap();
            assert_eq!(extracted.media_type, "image/png", "{name}");
            assert!(extracted.bytes.starts_with(b"\x89PNG"), "{name}");
        }

        let source = book(&kotlin_archive("rar4.rar"));
        let media = analyze_book(&source, false, || false).unwrap();
        assert!(media.pages.iter().all(|page| page.dimension.is_none()));
        assert_eq!(media.pages[0].file_name, "komga-1.png");
        assert_eq!(media.pages[0].file_size, Some(3108));
    }

    #[test]
    fn analyzes_and_renders_kotlin_pdf_fixture() {
        let source = book(&kotlin_pdf());
        let media = analyze_book(&source, true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(PDF_MEDIA_TYPE));
        assert_eq!(media.page_count, 1);
        assert_eq!(media.pages[0].file_name, "1");
        assert_eq!(media.pages[0].media_type, "");
        assert_eq!(
            media.pages[0].dimension,
            Some(Dimension {
                width: 512,
                height: 512
            })
        );
        assert_eq!(media.pages[0].file_size, None);

        assert_eq!(page_output_media_type(&media, &media.pages[0]), JPEG_MEDIA_TYPE);
        assert_eq!(
            page_output_dimension(&media, &media.pages[0]),
            Some(Dimension {
                width: 3200,
                height: 3200
            })
        );

        let rendered = extract_page(&source, &media, 1).unwrap();
        assert_eq!(rendered.media_type, JPEG_MEDIA_TYPE);
        assert!(rendered.bytes.starts_with(&[0xff, 0xd8]));
        assert_eq!(
            image::load_from_memory(&rendered.bytes).unwrap().dimensions(),
            (3200, 3200)
        );

        let raw = extract_raw_page(&source, &media, 1).unwrap();
        assert_eq!(raw.media_type, PDF_MEDIA_TYPE);
        assert!(raw.bytes.starts_with(b"%PDF-"));
        assert_eq!(Pdf::new(raw.bytes).unwrap().pages().len(), 1);
    }

    #[test]
    fn analyzes_fixed_layout_epub_and_extracts_pages_resources_and_cover() {
        let source = book(&kotlin_archive("epub3.epub"));
        let media = analyze_book(&source, true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(EPUB_MEDIA_TYPE));
        assert_eq!(media.page_count, 2);
        assert!(media.epub_divina_compatible);
        assert!(!media.epub_is_kepub);
        assert_eq!(media.pages[0].file_name, "cover.jpeg");
        assert_eq!(media.pages[1].file_name, "0_0.png");
        assert_eq!(media.pages[1].dimension, Some(Dimension { width: 38, height: 38 }));
        assert_eq!(media.comment, None);

        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_ref() else {
            panic!("expected EPUB extension");
        };
        assert!(extension.is_fixed_layout);
        assert_eq!(extension.positions.len(), 2);
        assert_eq!(extension.positions[0].href, "titlepage.xhtml");
        assert_eq!(extension.positions[1].href, "page_1.xhtml");

        let first_page = extract_page(&source, &media, 1).unwrap();
        assert_eq!(first_page.media_type, "image/jpeg");
        assert!(first_page.bytes.starts_with(&[0xff, 0xd8]));
        let resource = extract_resource(&source, &media, "nav.xhtml").unwrap();
        assert_eq!(resource.media_type, "application/xhtml+xml");
        assert!(resource.bytes.windows(10).any(|bytes| bytes == b"Navigation"));
        assert!(matches!(
            extract_resource(&source, &media, "META-INF/container.xml"),
            Err(MediaOperationError::ResourceNotFound)
        ));

        let thumbnail = generate_thumbnail(&source, &media, 300).unwrap();
        assert_eq!(thumbnail.media_type, JPEG_MEDIA_TYPE);
        assert_eq!(
            image::load_from_memory(&thumbnail.bytes).unwrap().dimensions(),
            (48, 48)
        );
    }

    #[test]
    fn analyzes_reflowable_epub_navigation_positions_and_invalid_extension() {
        let source = book(&kotlin_epub("The Incomplete Theft - Ralph Burke.epub"));
        let media = analyze_book(&source, false, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(EPUB_MEDIA_TYPE));
        assert!(!media.epub_divina_compatible);
        assert!(media.pages.is_empty());
        assert_eq!(
            media
                .files
                .iter()
                .filter(|file| file.sub_type.as_deref() == Some("EPUB_PAGE"))
                .count(),
            4
        );
        assert_eq!(media.comment, None);
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_ref() else {
            panic!("expected EPUB extension");
        };
        assert!(!extension.is_fixed_layout);
        assert_eq!(extension.toc[0].title, "The Incomplete Theft");
        assert_eq!(
            extension.toc[0].href.as_deref(),
            Some("OEBPS/@public@vhost@g@gutenberg@html@files@65659@65659-h@65659-h-0.htm_split_001.html")
        );
        assert!(!extension.positions.is_empty());
        assert!(extension.positions.iter().all(|position| position.kobo_span.is_none()));
        assert!(matches!(
            extract_page(&source, &media, 1),
            Err(MediaOperationError::PageOutOfBounds)
        ));

        let invalid = analyze_book(&book(&kotlin_archive("zip-as-epub.epub")), false, || false).unwrap();
        assert_eq!(invalid.status, MediaStatus::Error);
        assert_eq!(invalid.media_type.as_deref(), Some(ZIP_MEDIA_TYPE));
        assert_eq!(invalid.comment.as_deref(), Some("ERR_1032"));
    }

    #[test]
    fn detects_native_kepub_positions_and_reports_missing_manifest_resources() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("native-kepub.epub");
        let opf = r#"<?xml version="1.0"?>
            <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
              <metadata/>
              <manifest>
                <item id="page" href="page.xhtml" media-type="application/xhtml+xml"/>
                <item id="missing" href="missing.css" media-type="text/css"/>
              </manifest>
              <spine><itemref idref="page"/></spine>
            </package>"#;
        let page = format!(
            "<html><body><span class=\"koboSpan\" id=\"kobo.1.1\">start</span>{}<span class=\"koboSpan\" id=\"kobo.1.2\">end</span></body></html>",
            "x".repeat(1200)
        );
        write_epub(&path, opf, &[("page.xhtml", page.as_bytes())]);

        let media = analyze_book(&book(&path), false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert!(media.epub_is_kepub);
        assert!(!media.epub_divina_compatible);
        assert_eq!(media.comment.as_deref(), Some("ERR_1033 [missing.css]"));
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension else {
            panic!("expected EPUB extension");
        };
        assert_eq!(extension.positions.len(), 2);
        assert_eq!(extension.positions[0].kobo_span.as_deref(), Some("kobo.1.1"));
        assert!(extension.positions[1].kobo_span.is_some());
    }

    #[test]
    fn pdf_dimensions_can_be_skipped_and_invalid_documents_report_kotlin_error() {
        let source = book(&kotlin_pdf());
        let media = analyze_book(&source, false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.pages[0].dimension, None);
        assert_eq!(page_output_dimension(&media, &media.pages[0]), None);

        let temporary = tempfile::tempdir().unwrap();
        let invalid_path = temporary.path().join("invalid.pdf");
        fs::write(&invalid_path, b"%PDF-1.7\ninvalid").unwrap();
        let invalid = analyze_book(&book(&invalid_path), true, || false).unwrap();
        assert_eq!(invalid.status, MediaStatus::Error);
        assert_eq!(invalid.media_type.as_deref(), Some(PDF_MEDIA_TYPE));
        assert_eq!(invalid.comment.as_deref(), Some("ERR_1005"));
    }

    #[test]
    fn extracts_the_requested_page_as_a_standalone_pdf() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("pages.pdf");
        fs::write(&path, blank_pdf(&[(100.0, 200.0), (300.0, 400.0)])).unwrap();
        let source = book(&path);
        let media = analyze_book(&source, true, || false).unwrap();
        assert_eq!(media.page_count, 2);

        let raw = extract_raw_page(&source, &media, 2).unwrap();
        let extracted = Pdf::new(raw.bytes).unwrap();
        assert_eq!(extracted.pages().len(), 1);
        let crop_box = extracted.pages()[0].crop_box();
        assert_eq!((crop_box.width(), crop_box.height()), (300.0, 400.0));
        assert!(matches!(
            extract_raw_page(&source, &media, 3),
            Err(MediaOperationError::PageOutOfBounds)
        ));
    }

    #[test]
    fn rejects_encrypted_and_multivolume_rar_with_kotlin_codes() {
        for name in ["rar4-encrypted.rar", "rar5-encrypted.rar"] {
            let media = analyze_book(&book(&kotlin_archive(name)), false, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Unsupported, "{name}");
            assert_eq!(media.comment.as_deref(), Some("ERR_1002"), "{name}");
            assert!(
                media
                    .media_type
                    .as_deref()
                    .is_some_and(|value| value.starts_with("application/x-rar-compressed")),
                "{name}"
            );
        }

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("multi-volume.cbr");
        let image = png();
        let volumes = write_stored_volumes(
            StoredEntry {
                name: b"page.png",
                data: &image,
                file_time: 0,
                file_attr: 0x20,
                host_os: 3,
                password: None,
                file_comment: None,
            },
            WriterOptions::new(ArchiveVersion::Rar40, FeatureSet::store_only()),
            64,
        )
        .unwrap();
        assert!(volumes.len() > 1);
        fs::write(&path, &volumes[0]).unwrap();

        let media = analyze_book(&book(&path), false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Unsupported);
        assert_eq!(media.comment.as_deref(), Some("ERR_1004"));
    }

    #[test]
    fn image_conversion_capabilities_match_enabled_decoders() {
        for media_type in [
            "image/jpeg",
            "image/png",
            "image/gif",
            "image/webp",
            "image/bmp",
            "image/tiff",
            "image/x-icon",
            "image/vnd.microsoft.icon; charset=binary",
        ] {
            assert!(can_convert_image_media_type(media_type), "{media_type}");
        }
        for media_type in ["image/svg+xml", "application/pdf", "application/octet-stream"] {
            assert!(!can_convert_image_media_type(media_type), "{media_type}");
        }
    }

    #[test]
    fn parses_only_comic_info_container_fields_with_kotlin_pairing() {
        let parsed = parse_comic_info_containers(
            br#"<ComicInfo><SeriesGroup> Alpha, ,Beta,Alpha </SeriesGroup><AlternateSeries>Event</AlternateSeries><AlternateNumber>7</AlternateNumber><StoryArc>Arc A, Arc B, Arc C</StoryArc><StoryArcNumber>1,nope,3</StoryArcNumber><Unknown>ignored</Unknown></ComicInfo>"#,
        )
        .unwrap();
        assert_eq!(parsed.collections, ["Alpha", "Beta"]);
        assert_eq!(
            parsed.read_lists,
            [
                ComicInfoReadList {
                    name: "Event".into(),
                    number: Some(7),
                },
                ComicInfoReadList {
                    name: "Arc A".into(),
                    number: Some(1),
                },
                ComicInfoReadList {
                    name: "Arc C".into(),
                    number: Some(3),
                },
            ]
        );
    }

    #[test]
    fn comic_info_without_arc_numbers_keeps_names_and_rejects_dtd() {
        let parsed = parse_comic_info_containers(
            b"<ComicInfo><StoryArc> First, Second </StoryArc><AlternateNumber>invalid</AlternateNumber></ComicInfo>",
        )
        .unwrap();
        assert_eq!(
            parsed.read_lists,
            [
                ComicInfoReadList {
                    name: "First".into(),
                    number: None,
                },
                ComicInfoReadList {
                    name: "Second".into(),
                    number: None,
                },
            ]
        );
        assert!(parse_comic_info_containers(b"<!DOCTYPE x><ComicInfo />").is_err());

        let paired = parse_comic_info_containers(
            b"<ComicInfo><StoryArc>one, , three</StoryArc><StoryArcNumber>6,7,8</StoryArcNumber></ComicInfo>",
        )
        .unwrap();
        assert_eq!(
            paired.read_lists,
            [
                ComicInfoReadList {
                    name: "one".into(),
                    number: Some(6),
                },
                ComicInfoReadList {
                    name: "three".into(),
                    number: Some(8),
                },
            ]
        );
    }
}
