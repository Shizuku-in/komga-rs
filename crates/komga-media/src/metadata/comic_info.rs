use chrono::NaiveDate;
use komga_core::{
    Author, Book, BookMetadataPatch, Media, MediaProfile, MediaStatus, MetadataUpdate, ReadingDirection,
    SeriesMetadataPatch, WebLink, ZIP_MEDIA_TYPE,
};

use crate::{MediaOperationError, archive, epub};

use super::{ComicInfoContainers, ComicInfoReadList, EmbeddedMetadata, comic_age_rating, isbn13, set_update, update};

const MAX_SIZE: i64 = 1024 * 1024;

/// Extracts and parses the analyzed root-level `ComicInfo.xml` resource.
///
/// # Errors
///
/// Returns an operation error when extraction fails or the XML is malformed or unsafe.
pub fn extract_comic_info_containers(
    book: &Book,
    media: &Media,
) -> Result<Option<ComicInfoContainers>, MediaOperationError> {
    let Some(bytes) = extract_bytes(book, media)? else {
        return Ok(None);
    };
    parse_comic_info_containers(&bytes).map(Some)
}

/// Extracts all supported ComicInfo metadata from an analyzed book.
///
/// # Errors
///
/// Returns an operation error when extraction fails or the XML is malformed or unsafe.
pub fn extract_comic_info_metadata(
    book: &Book,
    media: &Media,
    append_volume: bool,
) -> Result<Option<EmbeddedMetadata>, MediaOperationError> {
    let Some(bytes) = extract_bytes(book, media)? else {
        return Ok(None);
    };
    parse_metadata(&bytes, append_volume).map(Some)
}

fn extract_bytes(book: &Book, media: &Media) -> Result<Option<Vec<u8>>, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Ok(None);
    }
    let Some(file) = media.files.iter().find(|file| file.file_name == "ComicInfo.xml") else {
        return Ok(None);
    };
    if file.file_size.is_some_and(|size| !(0..=MAX_SIZE).contains(&size)) {
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
    if bytes.len() > MAX_SIZE as usize {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml exceeds the 1 MiB limit".into(),
        ));
    }
    Ok(Some(bytes))
}

pub(crate) fn parse_comic_info_containers(bytes: &[u8]) -> Result<ComicInfoContainers, MediaOperationError> {
    let document = document(bytes)?;
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

fn parse_metadata(bytes: &[u8], append_volume: bool) -> Result<EmbeddedMetadata, MediaOperationError> {
    let doc = document(bytes)?;
    let root = doc.root_element();
    let text = |name: &str| {
        root.children()
            .find(|node| node.is_element() && node.tag_name().name() == name)
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let list = |name: &str| {
        text(name)
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
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
        (Some(year), month, day) => Some(
            NaiveDate::from_ymd_opt(
                year.parse().map_err(|_| invalid_date())?,
                month.unwrap_or("1").parse().map_err(|_| invalid_date())?,
                day.unwrap_or("1").parse().map_err(|_| invalid_date())?,
            )
            .ok_or_else(invalid_date)?,
        ),
        _ => None,
    };
    let number_sort = text("Number").and_then(|value| value.parse::<f32>().ok());
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
        "yesandrighttoleft" | "righttoleft" | "yes" => Some(ReadingDirection::RightToLeft),
        "no" => Some(ReadingDirection::LeftToRight),
        _ => None,
    });
    let total = text("Count")
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|value| *value > 0);
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
            age_rating: text("AgeRating")
                .and_then(comic_age_rating)
                .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            language: text("LanguageISO").map(str::to_owned),
            genres: set_update(list("Genre")),
            total_book_count: total.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            ..Default::default()
        },
        containers: parse_comic_info_containers(bytes)?,
    })
}

fn document(bytes: &[u8]) -> Result<roxmltree::Document<'_>, MediaOperationError> {
    let xml = std::str::from_utf8(bytes).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let upper = xml.to_ascii_uppercase();
    if upper.contains("<!DOCTYPE") || upper.contains("<!ENTITY") {
        return Err(MediaOperationError::Read(
            "ComicInfo.xml contains a DTD or entity declaration".into(),
        ));
    }
    roxmltree::Document::parse(xml).map_err(|error| MediaOperationError::Read(error.to_string()))
}

fn invalid_date() -> MediaOperationError {
    MediaOperationError::Read("invalid ComicInfo date".into())
}

fn push_read_list(read_lists: &mut Vec<ComicInfoReadList>, name: &str, number: Option<i32>) {
    if !read_lists.iter().any(|existing| existing.name == name) {
        read_lists.push(ComicInfoReadList {
            name: name.to_owned(),
            number,
        });
    }
}
