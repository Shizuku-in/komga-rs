use std::collections::BTreeSet;

use komga_core::{BookMetadataPatch, MetadataUpdate, SeriesMetadataPatch};

use crate::MediaOperationError;

mod comic_info;
mod epub;
mod mylar;

pub use comic_info::{extract_comic_info_containers, extract_comic_info_metadata};
pub use epub::extract_epub_metadata;
pub use mylar::extract_mylar_metadata;

#[cfg(test)]
pub(crate) use comic_info::parse_comic_info_containers;

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
        .filter(|character| character.is_ascii_digit() || matches!(character, 'X' | 'x'))
        .collect::<String>();
    let digits = if cleaned.len() == 10 {
        let checksum: u32 = cleaned
            .bytes()
            .enumerate()
            .map(|(index, byte)| {
                let digit = if index == 9 && matches!(byte, b'X' | b'x') {
                    10
                } else {
                    u32::from(byte.saturating_sub(b'0'))
                };
                digit * u32::try_from(10 - index).unwrap_or(0)
            })
            .sum();
        if !checksum.is_multiple_of(11) {
            return Err(MediaOperationError::Read("invalid ISBN".into()));
        }
        let base = format!("978{}", &cleaned[..9]);
        let sum: u32 = base
            .bytes()
            .enumerate()
            .map(|(index, byte)| u32::from(byte - b'0') * if index % 2 == 0 { 1 } else { 3 })
            .sum();
        format!("{base}{}", (10 - sum % 10) % 10)
    } else {
        cleaned
    };
    if digits.len() != 13 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MediaOperationError::Read("invalid ISBN".into()));
    }
    let checksum: u32 = digits
        .bytes()
        .enumerate()
        .map(|(index, byte)| u32::from(byte - b'0') * if index % 2 == 0 { 1 } else { 3 })
        .sum();
    if !checksum.is_multiple_of(10) {
        return Err(MediaOperationError::Read("invalid ISBN".into()));
    }
    Ok(digits)
}
