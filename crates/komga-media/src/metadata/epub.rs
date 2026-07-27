use chrono::NaiveDate;
use komga_core::{
    Author, Book, BookMetadataPatch, Media, MediaProfile, MediaStatus, MetadataUpdate, ReadingDirection,
    SeriesMetadataPatch,
};

use crate::{MediaOperationError, epub};

use super::{ComicInfoContainers, EmbeddedMetadata, isbn13, set_update, update};

/// Extracts OPF metadata from the EPUB container declaration.
///
/// # Errors
///
/// Returns an operation error when the package document cannot be read or parsed.
pub fn extract_epub_metadata(book: &Book, media: &Media) -> Result<Option<EmbeddedMetadata>, MediaOperationError> {
    if media.status != MediaStatus::Ready || media.profile() != Some(MediaProfile::Epub) {
        return Ok(None);
    }
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".into()))?;
    let xml = epub::package::metadata_xml(&path).map_err(MediaOperationError::Read)?;
    parse_metadata(&xml).map(Some)
}

fn parse_metadata(xml: &str) -> Result<EmbeddedMetadata, MediaOperationError> {
    let doc = epub::package::parse_xml(xml).map_err(MediaOperationError::Read)?;
    let metadata = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "metadata")
        .ok_or_else(|| MediaOperationError::Read("OPF does not contain metadata".into()))?;
    let values = |name: &str| {
        metadata
            .descendants()
            .filter(move |node| node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name))
            .filter_map(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
    };
    let first = |name: &str| values(name).first().copied();
    let mut authors = Vec::new();
    for node in metadata
        .descendants()
        .filter(|node| node.is_element() && matches!(node.tag_name().name(), "creator" | "contributor"))
    {
        let Some(name) = node.text().map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        let role = node
            .attributes()
            .find(|attribute| attribute.name().eq_ignore_ascii_case("role"))
            .map(|attribute| marc_role(attribute.value()))
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
        .find(|value| {
            let length = value
                .chars()
                .filter(|character| character.is_ascii_digit() || matches!(character, 'x' | 'X'))
                .count();
            value.to_ascii_lowercase().contains("isbn") || matches!(length, 10 | 13)
        })
        .map(isbn13)
        .transpose()?;
    let series = metadata
        .descendants()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "meta"
                && (node.attribute("name") == Some("calibre:series")
                    || node.attribute("property") == Some("belongs-to-collection"))
        })
        .and_then(|node| node.attribute("content").or_else(|| node.text()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let series_index = metadata
        .descendants()
        .find(|node| {
            node.is_element()
                && node.tag_name().name() == "meta"
                && (node.attribute("name") == Some("calibre:series_index")
                    || node.attribute("property") == Some("group-position"))
        })
        .and_then(|node| node.attribute("content").or_else(|| node.text()))
        .and_then(|value| value.trim().parse::<f64>().ok());
    let subjects = values("subject").into_iter().map(str::to_owned).collect::<Vec<_>>();
    let description = first("description").map(strip_html);
    let direction = doc
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "spine")
        .and_then(|node| node.attribute("page-progression-direction"))
        .and_then(|value| match value {
            "rtl" => Some(ReadingDirection::RightToLeft),
            "ltr" => Some(ReadingDirection::LeftToRight),
            _ => None,
        });
    Ok(EmbeddedMetadata {
        book: BookMetadataPatch {
            title: first("title").map(str::to_owned),
            summary: update(description.clone()),
            number: series_index.map(|value| value.to_string()),
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
