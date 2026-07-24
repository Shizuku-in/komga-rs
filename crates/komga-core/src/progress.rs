//! Framework-independent reading-progress policy.
//!
//! Traditional page updates use Komga's one-based page numbers. Readium
//! progression updates preserve the client locator but derive trusted media
//! type, publication progression, completion, and page values from analyzed
//! media before anything is persisted.

use chrono::{DateTime, NaiveDateTime, Utc};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Media, MediaExtension, MediaProfile, R2Locator};

/// Device identity attached to a Readium progression update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct R2Device {
    /// Stable device identifier supplied by the reader.
    pub id: String,
    /// Human-readable device name supplied by the reader.
    pub name: String,
}

/// Readium progression document exchanged by Komga's WebPub endpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct R2Progression {
    /// Client modification timestamp, normalized to UTC while deserializing.
    pub modified: DateTime<Utc>,
    /// Reader device responsible for the update.
    pub device: R2Device,
    /// Precise publication location.
    pub locator: R2Locator,
}

/// Per-user progress persisted for one book.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadProgress {
    /// Owning book identifier.
    pub book_id: String,
    /// Owning user identifier.
    pub user_id: String,
    /// One-based page number, or zero when EPUB positions have no total progression.
    pub page: i32,
    /// Whether the publication is considered complete.
    pub completed: bool,
    /// UTC-compatible timestamp used to resolve progression conflicts.
    pub read_date: NaiveDateTime,
    /// Originating device identifier.
    pub device_id: String,
    /// Originating device name.
    pub device_name: String,
    /// Readium locator, when the update carries or derives one.
    pub locator: Option<R2Locator>,
}

impl ReadProgress {
    /// Converts stored progress to the Readium progression response shape.
    pub fn to_r2_progression(&self) -> R2Progression {
        R2Progression {
            modified: DateTime::from_naive_utc_and_offset(self.read_date, Utc),
            device: R2Device {
                id: self.device_id.clone(),
                name: self.device_name.clone(),
            },
            locator: self.locator.clone().unwrap_or_else(empty_locator),
        }
    }
}

/// Validation failures produced while deriving trusted progress.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReadProgressError {
    /// The request is inconsistent with analyzed media.
    #[error("{0}")]
    Invalid(String),
    /// The supplied progression does not supersede stored progress.
    #[error("Progression is older than existing")]
    Conflict,
}

/// Derives traditional one-based page progress from analyzed media.
pub fn page_progress(
    book_id: &str,
    user_id: &str,
    media: &Media,
    page: i32,
    read_date: NaiveDateTime,
) -> Result<ReadProgress, ReadProgressError> {
    require_page(page, media.page_count)?;
    let locator = if media.profile() == Some(MediaProfile::Epub) {
        if !media.epub_divina_compatible {
            return Err(invalid("epub book is not Divina compatible"));
        }
        let extension = epub_extension(media)?;
        let index = usize::try_from(page - 1).map_err(|_| invalid_page(page, media.page_count))?;
        Some(
            extension
                .positions
                .get(index)
                .cloned()
                .ok_or_else(|| invalid_page(page, media.page_count))?,
        )
    } else {
        None
    };
    Ok(ReadProgress {
        book_id: book_id.to_owned(),
        user_id: user_id.to_owned(),
        page,
        completed: page == media.page_count,
        read_date,
        device_id: String::new(),
        device_name: String::new(),
        locator,
    })
}

/// Marks a book complete at its analyzed page count.
pub fn completed_progress(book_id: &str, user_id: &str, media: &Media, read_date: NaiveDateTime) -> ReadProgress {
    ReadProgress {
        book_id: book_id.to_owned(),
        user_id: user_id.to_owned(),
        page: media.page_count,
        completed: true,
        read_date,
        device_id: String::new(),
        device_name: String::new(),
        locator: None,
    }
}

/// Validates and normalizes a Readium progression against analyzed media.
pub fn progression(
    book_id: &str,
    user_id: &str,
    media: &Media,
    saved: Option<&ReadProgress>,
    update: R2Progression,
) -> Result<ReadProgress, ReadProgressError> {
    let read_date = update.modified.naive_utc();
    if saved.is_some_and(|progress| read_date <= progress.read_date) {
        return Err(ReadProgressError::Conflict);
    }
    match media.profile().ok_or_else(|| invalid("Media has no profile"))? {
        MediaProfile::Divina | MediaProfile::Pdf => positioned_progress(book_id, user_id, media, update, read_date),
        MediaProfile::Epub => epub_progress(book_id, user_id, media, update, read_date),
    }
}

fn positioned_progress(
    book_id: &str,
    user_id: &str,
    media: &Media,
    update: R2Progression,
    read_date: NaiveDateTime,
) -> Result<ReadProgress, ReadProgressError> {
    let page = update
        .locator
        .locations
        .as_ref()
        .and_then(|locations| locations.position);
    let page = page.ok_or_else(|| invalid_page_value(page, media.page_count))?;
    require_page(page, media.page_count)?;
    Ok(ReadProgress {
        book_id: book_id.to_owned(),
        user_id: user_id.to_owned(),
        page,
        completed: page == media.page_count,
        read_date,
        device_id: update.device.id,
        device_name: update.device.name,
        locator: Some(update.locator),
    })
}

fn epub_progress(
    book_id: &str,
    user_id: &str,
    media: &Media,
    update: R2Progression,
    read_date: NaiveDateTime,
) -> Result<ReadProgress, ReadProgressError> {
    let href = decode_href(&update.locator.href)?;
    if !media.files.iter().any(|file| file.file_name == href) {
        return Err(invalid(format!("Resource does not exist in book: {href}")));
    }
    let requested_progression = update
        .locator
        .locations
        .as_ref()
        .and_then(|locations| locations.progression)
        .ok_or_else(|| invalid("location.progression is required"))?;
    let extension = epub_extension(media)?;
    let matching = extension
        .positions
        .iter()
        .filter(|position| position.href == href)
        .collect::<Vec<_>>();
    let matched = if extension.is_fixed_layout && matching.len() == 1 {
        matching[0]
    } else if let Some(exact) = matching.iter().find(|position| {
        position.locations.as_ref().and_then(|locations| locations.progression) == Some(requested_progression)
    }) {
        exact
    } else {
        let before = matching
            .iter()
            .filter(|position| {
                position
                    .locations
                    .as_ref()
                    .and_then(|locations| locations.progression)
                    .is_some_and(|value| value < requested_progression)
            })
            .max_by_key(|position| position.locations.as_ref().and_then(|locations| locations.position));
        let after = matching
            .iter()
            .filter(|position| {
                position
                    .locations
                    .as_ref()
                    .and_then(|locations| locations.progression)
                    .is_some_and(|value| value > requested_progression)
            })
            .min_by_key(|position| position.locations.as_ref().and_then(|locations| locations.position));
        match (before, after) {
            (Some(before), Some(after))
                if locator_position(before).is_some()
                    && locator_position(after).is_some()
                    && locator_position(before) <= locator_position(after) =>
            {
                before
            }
            _ => return Err(invalid("Invalid progression")),
        }
    };
    let total_progression = matched
        .locations
        .as_ref()
        .and_then(|locations| locations.total_progression);
    let page = total_progression.map_or(0, |value| (media.page_count as f32 * value).round() as i32);
    let completed = total_progression.is_some_and(|value| value >= 0.99);
    let mut locator = update.locator;
    locator.media_type.clone_from(&matched.media_type);
    if locator.kobo_span.is_none() {
        locator.kobo_span.clone_from(&matched.kobo_span);
    }
    if let Some(locations) = &mut locator.locations {
        locations.total_progression = total_progression;
    }
    Ok(ReadProgress {
        book_id: book_id.to_owned(),
        user_id: user_id.to_owned(),
        page,
        completed,
        read_date,
        device_id: update.device.id,
        device_name: update.device.name,
        locator: Some(locator),
    })
}

fn epub_extension(media: &Media) -> Result<&crate::MediaExtensionEpub, ReadProgressError> {
    match media.extension.as_ref() {
        Some(MediaExtension::Epub(extension)) => Ok(extension),
        None => Err(invalid("Epub extension not found")),
    }
}

fn decode_href(href: &str) -> Result<String, ReadProgressError> {
    let without_fragment = href.split_once('#').map_or(href, |(value, _)| value);
    percent_decode_str(&without_fragment.replace('+', " "))
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| invalid("Invalid resource href"))
}

fn locator_position(locator: &R2Locator) -> Option<i32> {
    locator.locations.as_ref().and_then(|locations| locations.position)
}

fn require_page(page: i32, page_count: i32) -> Result<(), ReadProgressError> {
    if (1..=page_count).contains(&page) {
        Ok(())
    } else {
        Err(invalid_page(page, page_count))
    }
}

fn invalid_page(page: i32, page_count: i32) -> ReadProgressError {
    invalid(format!(
        "Page argument ({page}) must be within 1 and book page count ({page_count})"
    ))
}

fn invalid_page_value(page: Option<i32>, page_count: i32) -> ReadProgressError {
    invalid(format!(
        "Page argument ({}) must be within 1 and book page count ({page_count})",
        page.map_or_else(|| "null".to_owned(), |value| value.to_string())
    ))
}

fn invalid(message: impl Into<String>) -> ReadProgressError {
    ReadProgressError::Invalid(message.into())
}

fn empty_locator() -> R2Locator {
    R2Locator {
        href: String::new(),
        media_type: String::new(),
        title: None,
        locations: None,
        text: None,
        kobo_span: None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::{EPUB_MEDIA_TYPE, EPUB_PAGE_SUBTYPE, MediaExtensionEpub, MediaFile, MediaStatus, R2Location};

    fn media(media_type: &str, page_count: i32) -> Media {
        Media {
            status: MediaStatus::Ready,
            media_type: Some(media_type.into()),
            pages: Vec::new(),
            page_count,
            files: Vec::new(),
            comment: None,
            extension: None,
            book_id: "book".into(),
            epub_divina_compatible: false,
            epub_is_kepub: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    fn locator(href: &str, progression: f32, position: i32, total: f32) -> R2Locator {
        R2Locator {
            href: href.into(),
            media_type: "application/xhtml+xml".into(),
            title: None,
            locations: Some(R2Location {
                progression: Some(progression),
                position: Some(position),
                total_progression: Some(total),
                ..Default::default()
            }),
            text: None,
            kobo_span: Some(format!("kobo.{position}.1")),
        }
    }

    fn update(locator: R2Locator, second: u32) -> R2Progression {
        R2Progression {
            modified: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, second).unwrap(),
            device: R2Device {
                id: "device".into(),
                name: "Reader".into(),
            },
            locator,
        }
    }

    #[test]
    fn page_progress_validates_bounds_and_completion() {
        let media = media("application/zip", 10);
        let date = Utc::now().naive_utc();
        assert!(!page_progress("book", "user", &media, 5, date).unwrap().completed);
        assert!(page_progress("book", "user", &media, 10, date).unwrap().completed);
        assert!(matches!(
            page_progress("book", "user", &media, 0, date),
            Err(ReadProgressError::Invalid(_))
        ));
        assert_eq!(completed_progress("book", "user", &media, date).page, 10);
    }

    #[test]
    fn epub_page_progress_requires_divina_positions() {
        let mut media = media(EPUB_MEDIA_TYPE, 2);
        let date = Utc::now().naive_utc();
        assert_eq!(
            page_progress("book", "user", &media, 1, date).unwrap_err().to_string(),
            "epub book is not Divina compatible"
        );
        media.epub_divina_compatible = true;
        media.extension = Some(MediaExtension::Epub(MediaExtensionEpub {
            positions: vec![locator("page.xhtml", 0.0, 1, 0.5)],
            ..Default::default()
        }));
        assert_eq!(
            page_progress("book", "user", &media, 1, date)
                .unwrap()
                .locator
                .unwrap()
                .href,
            "page.xhtml"
        );
    }

    #[test]
    fn positioned_progression_checks_pages_and_conflicts() {
        let media = media("application/pdf", 10);
        let request = update(locator("", 0.0, 5, 0.5), 2);
        let saved = ReadProgress {
            read_date: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 2).unwrap().naive_utc(),
            ..completed_progress("book", "user", &media, Utc::now().naive_utc())
        };
        assert_eq!(
            progression("book", "user", &media, Some(&saved), request.clone()),
            Err(ReadProgressError::Conflict)
        );
        let progress = progression("book", "user", &media, None, request).unwrap();
        assert_eq!(progress.page, 5);
        assert!(!progress.completed);
    }

    #[test]
    fn epub_progression_matches_exact_and_skewed_positions() {
        let mut media = media(EPUB_MEDIA_TYPE, 100);
        media.files = vec![MediaFile {
            file_name: "Text/chapter 1.xhtml".into(),
            media_type: Some("application/xhtml+xml".into()),
            sub_type: Some(EPUB_PAGE_SUBTYPE.into()),
            file_size: None,
        }];
        media.extension = Some(MediaExtension::Epub(MediaExtensionEpub {
            positions: vec![
                locator("Text/chapter 1.xhtml", 0.0, 1, 0.1),
                locator("Text/chapter 1.xhtml", 0.5, 2, 0.2),
                locator("Text/chapter 1.xhtml", 0.9, 3, 1.0),
            ],
            ..Default::default()
        }));
        let mut requested = locator("Text/chapter+1.xhtml#fragment", 0.75, 99, 0.95);
        requested.media_type = "wrong/type".into();
        requested.kobo_span = None;
        let progress = progression("book", "user", &media, None, update(requested, 1)).unwrap();
        assert_eq!(progress.page, 20);
        assert!(!progress.completed);
        let stored = progress.locator.unwrap();
        assert_eq!(stored.media_type, "application/xhtml+xml");
        assert_eq!(stored.kobo_span.as_deref(), Some("kobo.2.1"));
        assert_eq!(stored.locations.unwrap().total_progression, Some(0.2));
    }

    #[test]
    fn fixed_layout_epub_uses_its_only_resource_position() {
        let mut media = media(EPUB_MEDIA_TYPE, 4);
        media.files = vec![MediaFile {
            file_name: "page.xhtml".into(),
            media_type: Some("application/xhtml+xml".into()),
            sub_type: Some(EPUB_PAGE_SUBTYPE.into()),
            file_size: None,
        }];
        media.extension = Some(MediaExtension::Epub(MediaExtensionEpub {
            is_fixed_layout: true,
            positions: vec![locator("page.xhtml", 0.0, 1, 1.0)],
            ..Default::default()
        }));
        let progress = progression(
            "book",
            "user",
            &media,
            None,
            update(locator("page.xhtml", 0.75, 99, 0.1), 1),
        )
        .unwrap();
        assert_eq!(progress.page, 4);
        assert!(progress.completed);
    }
}
