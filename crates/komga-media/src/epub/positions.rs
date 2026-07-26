//! Readium positions and native KEPUB span mapping.
//!
//! Fixed-layout publications receive one locator and Kotlin's `kobo.1.1` anchor
//! per spine resource. Reflowable publications use Readium's 1024-byte position
//! rule. Native KEPUB documents additionally map locators to the nearest existing
//! `.koboSpan`; ordinary reflowable EPUBs do not receive synthesized Kobo spans.

use std::{collections::HashMap, fs::File};

use komga_core::{EPUB_PAGE_SUBTYPE, MediaFile, R2Location, R2Locator};
use roxmltree::Document;
use scraper::{Html, Selector};
use zip::ZipArchive;

use super::package::read_entry_text;

/// Failures surfaced separately so analysis can preserve Kotlin error codes.
pub(crate) enum PositionError {
    /// Server shutdown was requested during resource traversal.
    Cancelled,
    /// A required XHTML resource could not be parsed or read.
    Invalid,
}

/// Detects native KEPUB content by looking for Kobo span elements in the spine.
pub(super) fn is_kepub(
    archive: &mut ZipArchive<File>,
    resources: &[MediaFile],
    cancelled: &impl Fn() -> bool,
) -> Result<bool, PositionError> {
    let Ok(selector) = Selector::parse(".koboSpan") else {
        return Err(PositionError::Invalid);
    };
    for file in resources
        .iter()
        .filter(|file| file.sub_type.as_deref() == Some(EPUB_PAGE_SUBTYPE))
    {
        if cancelled() {
            return Err(PositionError::Cancelled);
        }
        if read_entry_text(archive, &file.file_name)
            .ok()
            .is_some_and(|content| Html::parse_document(&content).select(&selector).next().is_some())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Computes publication locators in persisted spine order.
pub(super) fn compute(
    archive: &mut ZipArchive<File>,
    resources: &[MediaFile],
    fixed_layout: bool,
    is_kepub: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<R2Locator>, PositionError> {
    let reading_order = resources
        .iter()
        .filter(|file| file.sub_type.as_deref() == Some(EPUB_PAGE_SUBTYPE))
        .collect::<Vec<_>>();
    let kobo_positions = if is_kepub {
        reading_order
            .iter()
            .map(|file| {
                if cancelled() {
                    return Err(PositionError::Cancelled);
                }
                let content = read_entry_text(archive, &file.file_name).map_err(|_| PositionError::Invalid)?;
                Ok((
                    file.file_name.as_str(),
                    positions_from_kobo_spans(&content, file.file_size),
                ))
            })
            .collect::<Result<HashMap<_, _>, PositionError>>()?
    } else {
        HashMap::new()
    };

    let mut next_position = 1_i32;
    let mut positions = Vec::new();
    for file in reading_order {
        if cancelled() {
            return Err(PositionError::Cancelled);
        }
        // Fixed layouts advance by resource rather than by byte length.
        if fixed_layout {
            positions.push(locator(file, 0.0, next_position, Some("kobo.1.1".to_owned())));
            next_position = next_position.saturating_add(1);
            continue;
        }
        let size = u64::try_from(file.file_size.unwrap_or_default()).unwrap_or_default();
        // Readium defines one position for each started 1024-byte block.
        let count = usize::try_from(size.saturating_add(1023) / 1024)
            .unwrap_or(usize::MAX)
            .max(1);
        for index in 0..count {
            let progression = index as f32 / count as f32;
            let kobo_span = is_kepub.then(|| {
                if count == 1 || index == 0 {
                    "kobo.1.1".to_owned()
                } else {
                    nearest_kobo_span(kobo_positions.get(file.file_name.as_str()), progression)
                        .unwrap_or_else(|| "kobo.1.1".to_owned())
                }
            });
            positions.push(locator(file, progression, next_position, kobo_span));
            next_position = next_position.saturating_add(1);
        }
    }

    // Kotlin expresses total progression as the one-based position over total.
    let total = positions.len() as f32;
    if total > 0.0 {
        for locator in &mut positions {
            if let Some(location) = &mut locator.locations {
                location.total_progression = location.position.map(|position| position as f32 / total);
            }
        }
    }
    Ok(positions)
}

/// Adds Kobo span identifiers from a converted KEPUB to existing Readium locators.
pub(crate) fn enrich_from_kepub(
    archive: &mut ZipArchive<File>,
    resources: &[MediaFile],
    locators: &mut [R2Locator],
    cancelled: &impl Fn() -> bool,
) -> Result<(), PositionError> {
    let reading_order = resources
        .iter()
        .filter(|file| file.sub_type.as_deref() == Some(EPUB_PAGE_SUBTYPE))
        .collect::<Vec<_>>();
    let kobo_positions = reading_order
        .iter()
        .map(|file| {
            if cancelled() {
                return Err(PositionError::Cancelled);
            }
            let content = read_entry_text(archive, &file.file_name).map_err(|_| PositionError::Invalid)?;
            Ok((
                file.file_name.as_str(),
                positions_from_kobo_spans(&content, file.file_size),
            ))
        })
        .collect::<Result<HashMap<_, _>, PositionError>>()?;
    let counts = locators
        .iter()
        .fold(HashMap::<String, usize>::new(), |mut counts, locator| {
            *counts.entry(locator.href.clone()).or_default() += 1;
            counts
        });

    for locator in locators.iter_mut() {
        if cancelled() {
            return Err(PositionError::Cancelled);
        }
        let progression = locator
            .locations
            .as_ref()
            .and_then(|location| location.progression)
            .unwrap_or_default();
        locator.kobo_span = if counts.get(locator.href.as_str()).copied().unwrap_or_default() == 1 || progression == 0.0
        {
            Some("kobo.1.1".to_owned())
        } else {
            nearest_kobo_span(kobo_positions.get(locator.href.as_str()), progression)
        };
    }
    Ok(())
}

fn locator(file: &MediaFile, progression: f32, position: i32, kobo_span: Option<String>) -> R2Locator {
    R2Locator {
        href: file.file_name.clone(),
        media_type: file
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_owned()),
        title: None,
        locations: Some(R2Location {
            progression: Some(progression),
            position: Some(position),
            ..Default::default()
        }),
        text: None,
        kobo_span,
    }
}

/// Approximates each native Kobo span's resource progression from XML offsets.
fn positions_from_kobo_spans(content: &str, file_size: Option<i64>) -> Vec<(String, f32)> {
    let Ok(document) = Document::parse(content) else {
        return Vec::new();
    };
    let denominator = file_size
        .and_then(|size| u64::try_from(size).ok())
        .filter(|size| *size > 0)
        .unwrap_or(content.len() as u64) as f32;
    document
        .descendants()
        .filter(|node| {
            node.is_element()
                && node
                    .attribute("class")
                    .is_some_and(|classes| classes.split_whitespace().any(|class| class == "koboSpan"))
        })
        .filter_map(|node| {
            let id = node.attribute("id")?.trim();
            (!id.is_empty()).then(|| (id.to_owned(), node.range().end as f32 / denominator))
        })
        .collect()
}

fn nearest_kobo_span(positions: Option<&Vec<(String, f32)>>, progression: f32) -> Option<String> {
    positions?
        .iter()
        .min_by(|(_, left), (_, right)| (progression - *left).abs().total_cmp(&(progression - *right).abs()))
        .map(|(id, _)| id.clone())
}
