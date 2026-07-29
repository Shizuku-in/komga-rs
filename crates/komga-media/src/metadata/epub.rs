use std::collections::HashMap;

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
    let refined_roles = metadata
        .descendants()
        .filter(|node| {
            node.is_element()
                && node.tag_name().name() == "meta"
                && node.attribute("property") == Some("role")
                && node.attribute("scheme") == Some("marc:relators")
        })
        .filter_map(|node| Some((node.attribute("refines")?.strip_prefix('#')?, node.text()?.trim())))
        .filter(|(_, role)| !role.is_empty())
        .collect::<HashMap<_, _>>();
    let mut authors = Vec::new();
    for node in metadata
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "creator")
    {
        let Some(name) = node.text().map(str::trim).filter(|value| !value.is_empty()) else {
            continue;
        };
        let role = node
            .attributes()
            .find(|attribute| attribute.name().eq_ignore_ascii_case("role"))
            .map(|attribute| attribute.value())
            .or_else(|| node.attribute("id").and_then(|id| refined_roles.get(id).copied()))
            .map(marc_role)
            .unwrap_or("writer");
        authors.push(Author {
            name: name.into(),
            role: role.into(),
        });
    }
    let date = first("date").and_then(parse_date);
    let isbn = values("identifier").into_iter().find_map(|value| isbn13(value).ok());
    let collection = metadata.descendants().find(|node| {
        node.is_element()
            && node.tag_name().name() == "meta"
            && node.attribute("property") == Some("belongs-to-collection")
    });
    let calibre_series = metadata.descendants().find(|node| {
        node.is_element() && node.tag_name().name() == "meta" && node.attribute("name") == Some("calibre:series")
    });
    let series = collection
        .or(calibre_series)
        .and_then(|node| node.attribute("content").or_else(|| node.text()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let collection_id = collection.and_then(|node| node.attribute("id"));
    let series_index = metadata.descendants().find(|node| {
        node.is_element()
            && node.tag_name().name() == "meta"
            && node.attribute("refines").and_then(|value| value.strip_prefix('#')) == collection_id
            && node.attribute("property") == Some("group-position")
    });
    let calibre_series_index = metadata.descendants().find(|node| {
        node.is_element() && node.tag_name().name() == "meta" && node.attribute("name") == Some("calibre:series_index")
    });
    let series_index = series_index
        .or(calibre_series_index)
        .and_then(|node| node.attribute("content").or_else(|| node.text()))
        .map(str::trim)
        .filter(|value| !value.is_empty());
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
            summary: update(description),
            number: series_index.map(str::to_owned),
            number_sort: series_index.and_then(|value| value.parse::<f32>().ok()),
            release_date: date.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            authors: (!authors.is_empty())
                .then_some(authors)
                .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            isbn: isbn.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
            ..Default::default()
        },
        series: SeriesMetadataPatch {
            title: series.clone(),
            title_sort: series,
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
        .join("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    if let Ok(date) = NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d") {
        return Some(date);
    }
    if let Some((year, month)) = value.split_once('-')
        && let (Ok(year), Ok(month)) = (year.parse(), month.get(..2).unwrap_or(month).parse())
        && let Some(date) = NaiveDate::from_ymd_opt(year, month, 1)
    {
        return Some(date);
    }
    if let Ok(year) = value.parse()
        && let Some(date) = NaiveDate::from_ymd_opt(year, 1, 1)
    {
        return Some(date);
    }
    None
}

fn marc_role(value: &str) -> &str {
    match value.to_ascii_lowercase().as_str() {
        "aut" => "writer",
        "clr" => "colorist",
        "cov" => "cover",
        "art" | "ill" => "penciller",
        "edt" => "editor",
        "trl" => "translator",
        _ => "writer",
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, path::PathBuf};

    use super::*;

    fn fixture(name: &str) -> EmbeddedMetadata {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/epub")
            .join(name);
        parse_metadata(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn assert_panik_metadata(metadata: &EmbeddedMetadata) {
        assert_eq!(metadata.book.title.as_deref(), Some("Panik im Paradies"));
        assert!(matches!(
            &metadata.book.summary,
            MetadataUpdate::Set(summary)
                if summary.starts_with("Bereits im ersten Band \"Panik im Paradies\"")
                    && summary.ends_with("ein düsteres Geheimnis hütet...")
        ));
        assert_eq!(
            metadata.book.release_date,
            MetadataUpdate::Set(NaiveDate::from_ymd_opt(1999, 7, 31).unwrap())
        );
        assert_eq!(
            metadata.book.authors,
            MetadataUpdate::Set(vec![
                Author {
                    name: "Ulf Blanck".into(),
                    role: "writer".into(),
                },
                Author {
                    name: "The Editor".into(),
                    role: "editor".into(),
                },
            ])
        );
        assert_eq!(metadata.book.isbn, MetadataUpdate::Set("9783440077894".into()));
        assert_eq!(metadata.book.number.as_deref(), Some("1.5"));
        assert_eq!(metadata.book.number_sort, Some(1.5));
        assert_eq!(metadata.book.tags, MetadataUpdate::Keep);

        assert_eq!(metadata.series.title.as_deref(), Some("Die drei ??? Kids"));
        assert_eq!(metadata.series.title_sort.as_deref(), Some("Die drei ??? Kids"));
        assert_eq!(
            metadata.series.reading_direction,
            MetadataUpdate::Set(ReadingDirection::RightToLeft)
        );
        assert_eq!(metadata.series.publisher.as_deref(), Some("Kosmos"));
        assert_eq!(metadata.series.language.as_deref(), Some("de"));
        assert_eq!(
            metadata.series.genres,
            MetadataUpdate::Set(BTreeSet::from(["Kinder- und Jugendbücher".into()]))
        );
        assert_eq!(metadata.series.summary, None);
    }

    #[test]
    fn kotlin_epub_3_namespace_variants_match_metadata_contract() {
        assert_panik_metadata(&fixture("Panik im Paradies.opf"));
        assert_panik_metadata(&fixture("Panik im Paradies - namespace.opf"));
    }

    #[test]
    fn kotlin_epub_3_multiple_authors_and_subjects_match_metadata_contract() {
        let metadata = fixture("Die Drei 3.opf");
        assert_eq!(
            metadata.book.title.as_deref(),
            Some("Die Drei Fragezeichen-Kids, Bd.3, Invasion Der Fliegen")
        );
        assert!(matches!(
            &metadata.book.summary,
            MetadataUpdate::Set(summary)
                if summary.starts_with("Hunderte von Fliegen tummeln sich")
                    && summary.ends_with("einen düsteren Kanalschacht...")
        ));
        assert_eq!(
            metadata.book.release_date,
            MetadataUpdate::Set(NaiveDate::from_ymd_opt(1999, 7, 31).unwrap())
        );
        assert_eq!(
            metadata.book.authors,
            MetadataUpdate::Set(vec![
                Author {
                    name: "Ulf Blanck".into(),
                    role: "writer".into(),
                },
                Author {
                    name: "Stefanie Wegner".into(),
                    role: "writer".into(),
                },
            ])
        );
        assert_eq!(metadata.book.isbn, MetadataUpdate::Set("9783440077931".into()));
        assert_eq!(metadata.book.number.as_deref(), Some("3"));
        assert_eq!(metadata.book.number_sort, Some(3.0));
        assert_eq!(metadata.book.tags, MetadataUpdate::Keep);

        assert_eq!(metadata.series.title.as_deref(), Some("Die drei ??? Kids"));
        assert_eq!(metadata.series.title_sort.as_deref(), Some("Die drei ??? Kids"));
        assert_eq!(metadata.series.reading_direction, MetadataUpdate::Keep);
        assert_eq!(metadata.series.publisher.as_deref(), Some("Franckh-Kosmos Verlag"));
        assert_eq!(metadata.series.language.as_deref(), Some("de"));
        assert_eq!(
            metadata.series.genres,
            MetadataUpdate::Set(BTreeSet::from([
                "Juvenile Fiction".into(),
                "Kinder- und Jugendbücher".into(),
                "Mysteries & Detective Stories".into(),
            ]))
        );
        assert_eq!(metadata.series.summary, None);
    }

    #[test]
    fn kotlin_epub_2_roles_and_early_date_match_metadata_contract() {
        let metadata = fixture("1979.opf");
        assert_eq!(metadata.book.title.as_deref(), Some("1979"));
        assert!(matches!(
            &metadata.book.summary,
            MetadataUpdate::Set(summary)
                if summary.contains("Ich schlief.Später hielten wir an")
                    && summary.ends_with("FOTO: ECKHART NICKEL")
        ));
        assert_eq!(
            metadata.book.release_date,
            MetadataUpdate::Set(NaiveDate::from_ymd_opt(101, 1, 1).unwrap())
        );
        assert_eq!(
            metadata.book.authors,
            MetadataUpdate::Set(vec![
                Author {
                    name: "Kracht, Christian".into(),
                    role: "writer".into(),
                },
                Author {
                    name: "The Editor".into(),
                    role: "editor".into(),
                },
            ])
        );
        assert_eq!(metadata.book.isbn, MetadataUpdate::Keep);
        assert_eq!(metadata.book.number, None);
        assert_eq!(metadata.book.number_sort, None);
        assert_eq!(metadata.series.title, None);
        assert_eq!(metadata.series.title_sort, None);
        assert_eq!(metadata.series.reading_direction, MetadataUpdate::Keep);
        assert_eq!(metadata.series.publisher, None);
        assert_eq!(metadata.series.language.as_deref(), Some("de"));
        assert_eq!(metadata.series.genres, MetadataUpdate::Keep);
    }
}
