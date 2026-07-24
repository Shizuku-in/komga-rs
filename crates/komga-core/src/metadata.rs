//! Framework-independent metadata entities, patch policy, and aggregation.
//!
//! Komga does not apply uniform JSON merge-patch semantics. Some nullable
//! request fields clear existing values, while null for required scalar fields
//! behaves like an omitted property. [`MetadataUpdate`] makes that distinction
//! explicit before persistence is involved.

use std::collections::{BTreeSet, HashSet};

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// An author and their contribution role.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Author {
    /// Display name, trimmed when metadata is normalized.
    pub name: String,
    /// Lowercase Komga role such as `writer` or `penciller`.
    pub role: String,
}

/// A labelled external metadata link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebLink {
    /// User-visible label.
    pub label: String,
    /// Absolute URL serialized by Komga.
    pub url: String,
}

/// An alternate series title qualified by a label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlternateTitle {
    /// Language or source label.
    pub label: String,
    /// Alternate display title.
    pub title: String,
}

/// Publication state stored in `SERIES_METADATA.STATUS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SeriesStatus {
    /// Publication has ended.
    Ended,
    /// Publication is ongoing.
    Ongoing,
    /// Publication was abandoned.
    Abandoned,
    /// Publication is temporarily paused.
    Hiatus,
}

impl SeriesStatus {
    /// Returns the Kotlin database representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ended => "ENDED",
            Self::Ongoing => "ONGOING",
            Self::Abandoned => "ABANDONED",
            Self::Hiatus => "HIATUS",
        }
    }
}

impl TryFrom<&str> for SeriesStatus {
    type Error = MetadataError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "ENDED" => Ok(Self::Ended),
            "ONGOING" => Ok(Self::Ongoing),
            "ABANDONED" => Ok(Self::Abandoned),
            "HIATUS" => Ok(Self::Hiatus),
            _ => Err(MetadataError::Invalid(format!("unknown series status: {value}"))),
        }
    }
}

/// Reading direction stored in `SERIES_METADATA.READING_DIRECTION`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReadingDirection {
    /// Left-to-right pages.
    LeftToRight,
    /// Right-to-left pages.
    RightToLeft,
    /// Vertically paginated content.
    Vertical,
    /// Continuous vertical scrolling.
    Webtoon,
}

impl ReadingDirection {
    /// Returns the Kotlin database representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LeftToRight => "LEFT_TO_RIGHT",
            Self::RightToLeft => "RIGHT_TO_LEFT",
            Self::Vertical => "VERTICAL",
            Self::Webtoon => "WEBTOON",
        }
    }
}

impl TryFrom<&str> for ReadingDirection {
    type Error = MetadataError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "LEFT_TO_RIGHT" => Ok(Self::LeftToRight),
            "RIGHT_TO_LEFT" => Ok(Self::RightToLeft),
            "VERTICAL" => Ok(Self::Vertical),
            "WEBTOON" => Ok(Self::Webtoon),
            _ => Err(MetadataError::Invalid(format!("unknown reading direction: {value}"))),
        }
    }
}

/// One book's complete editable metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct BookMetadata {
    /// Owning book identifier.
    pub book_id: String,
    /// Display title.
    pub title: String,
    /// Whether title is protected from automatic providers.
    pub title_lock: bool,
    /// Summary text.
    pub summary: String,
    /// Whether summary is locked.
    pub summary_lock: bool,
    /// Display number.
    pub number: String,
    /// Whether display number is locked.
    pub number_lock: bool,
    /// Numeric ordering value.
    pub number_sort: f64,
    /// Whether numeric ordering is locked.
    pub number_sort_lock: bool,
    /// Optional publication date.
    pub release_date: Option<NaiveDate>,
    /// Whether publication date is locked.
    pub release_date_lock: bool,
    /// Ordered contributors.
    pub authors: Vec<Author>,
    /// Whether authors are locked.
    pub authors_lock: bool,
    /// Normalized lowercase tags.
    pub tags: BTreeSet<String>,
    /// Whether tags are locked.
    pub tags_lock: bool,
    /// Digits-only ISBN-13, or an empty string.
    pub isbn: String,
    /// Whether ISBN is locked.
    pub isbn_lock: bool,
    /// Ordered external links.
    pub links: Vec<WebLink>,
    /// Whether links are locked.
    pub links_lock: bool,
    /// Persisted creation timestamp.
    pub created_date: NaiveDateTime,
    /// Persisted modification timestamp.
    pub last_modified_date: NaiveDateTime,
}

impl BookMetadata {
    /// Applies an API-independent update and returns normalized metadata.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataError::Invalid`] when a supplied title, number,
    /// author, link, or ISBN violates Komga's request contract.
    pub fn apply_patch(&self, patch: BookMetadataPatch) -> Result<Self, MetadataError> {
        let mut value = self.clone();
        if let Some(title) = patch.title {
            require_not_blank(&title, "title")?;
            value.title = title;
        }
        if let Some(lock) = patch.title_lock {
            value.title_lock = lock;
        }
        apply_string_update(&mut value.summary, patch.summary);
        if let Some(lock) = patch.summary_lock {
            value.summary_lock = lock;
        }
        if let Some(number) = patch.number {
            require_not_blank(&number, "number")?;
            value.number = number;
        }
        if let Some(lock) = patch.number_lock {
            value.number_lock = lock;
        }
        if let Some(number_sort) = patch.number_sort {
            if !number_sort.is_finite() {
                return Err(invalid("numberSort must be finite"));
            }
            value.number_sort = number_sort;
        }
        if let Some(lock) = patch.number_sort_lock {
            value.number_sort_lock = lock;
        }
        apply_optional_update(&mut value.release_date, patch.release_date);
        if let Some(lock) = patch.release_date_lock {
            value.release_date_lock = lock;
        }
        match patch.authors {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.authors.clear(),
            MetadataUpdate::Set(authors) => {
                validate_authors(&authors)?;
                value.authors = authors;
            }
        }
        if let Some(lock) = patch.authors_lock {
            value.authors_lock = lock;
        }
        apply_set_update(&mut value.tags, patch.tags);
        if let Some(lock) = patch.tags_lock {
            value.tags_lock = lock;
        }
        match patch.isbn {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.isbn.clear(),
            MetadataUpdate::Set(isbn) => value.isbn = normalize_isbn(&isbn)?,
        }
        if let Some(lock) = patch.isbn_lock {
            value.isbn_lock = lock;
        }
        match patch.links {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.links.clear(),
            MetadataUpdate::Set(links) => {
                validate_links(&links)?;
                value.links = links;
            }
        }
        if let Some(lock) = patch.links_lock {
            value.links_lock = lock;
        }
        value.normalize();
        Ok(value)
    }

    fn normalize(&mut self) {
        self.title = self.title.trim().to_owned();
        self.summary = self.summary.trim().to_owned();
        self.number = self.number.trim().to_owned();
        self.authors.iter_mut().for_each(normalize_author);
        self.tags = normalize_set(std::mem::take(&mut self.tags));
        self.isbn.retain(|character| character.is_ascii_digit());
    }
}

/// One series' complete editable metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesMetadata {
    /// Owning series identifier.
    pub series_id: String,
    /// Publication status.
    pub status: SeriesStatus,
    /// Whether status is locked.
    pub status_lock: bool,
    /// Display title.
    pub title: String,
    /// Whether title is locked.
    pub title_lock: bool,
    /// Sorting title.
    pub title_sort: String,
    /// Whether sorting title is locked.
    pub title_sort_lock: bool,
    /// Summary text.
    pub summary: String,
    /// Whether summary is locked.
    pub summary_lock: bool,
    /// Optional reading direction.
    pub reading_direction: Option<ReadingDirection>,
    /// Whether reading direction is locked.
    pub reading_direction_lock: bool,
    /// Publisher name.
    pub publisher: String,
    /// Whether publisher is locked.
    pub publisher_lock: bool,
    /// Optional audience age rating.
    pub age_rating: Option<i32>,
    /// Whether age rating is locked.
    pub age_rating_lock: bool,
    /// Normalized BCP 47 language tag, or an empty string.
    pub language: String,
    /// Whether language is locked.
    pub language_lock: bool,
    /// Normalized lowercase genres.
    pub genres: BTreeSet<String>,
    /// Whether genres are locked.
    pub genres_lock: bool,
    /// Normalized lowercase tags.
    pub tags: BTreeSet<String>,
    /// Whether tags are locked.
    pub tags_lock: bool,
    /// Optional expected publication count.
    pub total_book_count: Option<i32>,
    /// Whether expected count is locked.
    pub total_book_count_lock: bool,
    /// Labels used by content restrictions.
    pub sharing_labels: BTreeSet<String>,
    /// Whether sharing labels are locked.
    pub sharing_labels_lock: bool,
    /// Ordered external links.
    pub links: Vec<WebLink>,
    /// Whether links are locked.
    pub links_lock: bool,
    /// Ordered alternate titles.
    pub alternate_titles: Vec<AlternateTitle>,
    /// Whether alternate titles are locked.
    pub alternate_titles_lock: bool,
    /// Persisted creation timestamp.
    pub created_date: NaiveDateTime,
    /// Persisted modification timestamp.
    pub last_modified_date: NaiveDateTime,
}

impl SeriesMetadata {
    /// Applies an API-independent update and returns normalized metadata.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataError::Invalid`] when supplied values violate Komga's
    /// title, language, count, link, or alternate-title contract.
    pub fn apply_patch(&self, patch: SeriesMetadataPatch) -> Result<Self, MetadataError> {
        let mut value = self.clone();
        if let Some(status) = patch.status {
            value.status = status;
        }
        if let Some(lock) = patch.status_lock {
            value.status_lock = lock;
        }
        if let Some(title) = patch.title {
            require_not_blank(&title, "title")?;
            value.title = title;
        }
        if let Some(lock) = patch.title_lock {
            value.title_lock = lock;
        }
        if let Some(title_sort) = patch.title_sort {
            require_not_blank(&title_sort, "titleSort")?;
            value.title_sort = title_sort;
        }
        if let Some(lock) = patch.title_sort_lock {
            value.title_sort_lock = lock;
        }
        if let Some(summary) = patch.summary {
            value.summary = summary;
        }
        if let Some(lock) = patch.summary_lock {
            value.summary_lock = lock;
        }
        apply_optional_update(&mut value.reading_direction, patch.reading_direction);
        if let Some(lock) = patch.reading_direction_lock {
            value.reading_direction_lock = lock;
        }
        if let Some(publisher) = patch.publisher {
            value.publisher = publisher;
        }
        if let Some(lock) = patch.publisher_lock {
            value.publisher_lock = lock;
        }
        match patch.age_rating {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.age_rating = None,
            MetadataUpdate::Set(age) if age >= 0 => value.age_rating = Some(age),
            MetadataUpdate::Set(_) => return Err(invalid("ageRating must be positive or zero")),
        }
        if let Some(lock) = patch.age_rating_lock {
            value.age_rating_lock = lock;
        }
        if let Some(language) = patch.language {
            value.language = normalize_language(&language)?;
        }
        if let Some(lock) = patch.language_lock {
            value.language_lock = lock;
        }
        apply_set_update(&mut value.genres, patch.genres);
        if let Some(lock) = patch.genres_lock {
            value.genres_lock = lock;
        }
        apply_set_update(&mut value.tags, patch.tags);
        if let Some(lock) = patch.tags_lock {
            value.tags_lock = lock;
        }
        match patch.total_book_count {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.total_book_count = None,
            MetadataUpdate::Set(count) if count > 0 => value.total_book_count = Some(count),
            MetadataUpdate::Set(_) => return Err(invalid("totalBookCount must be positive")),
        }
        if let Some(lock) = patch.total_book_count_lock {
            value.total_book_count_lock = lock;
        }
        apply_set_update(&mut value.sharing_labels, patch.sharing_labels);
        if let Some(lock) = patch.sharing_labels_lock {
            value.sharing_labels_lock = lock;
        }
        match patch.links {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.links.clear(),
            MetadataUpdate::Set(links) => {
                validate_links(&links)?;
                value.links = links;
            }
        }
        if let Some(lock) = patch.links_lock {
            value.links_lock = lock;
        }
        match patch.alternate_titles {
            MetadataUpdate::Keep => {}
            MetadataUpdate::Clear => value.alternate_titles.clear(),
            MetadataUpdate::Set(titles) => {
                validate_alternate_titles(&titles)?;
                value.alternate_titles = titles;
            }
        }
        if let Some(lock) = patch.alternate_titles_lock {
            value.alternate_titles_lock = lock;
        }
        value.normalize();
        Ok(value)
    }

    fn normalize(&mut self) {
        self.title = self.title.trim().to_owned();
        self.title_sort = self.title_sort.trim().to_owned();
        self.summary = self.summary.trim().to_owned();
        self.publisher = self.publisher.trim().to_owned();
        self.language = self.language.trim().to_owned();
        self.genres = normalize_set(std::mem::take(&mut self.genres));
        self.tags = normalize_set(std::mem::take(&mut self.tags));
        self.sharing_labels = normalize_set(std::mem::take(&mut self.sharing_labels));
    }
}

/// Metadata derived from all books belonging to one series.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookMetadataAggregation {
    /// Owning series identifier.
    pub series_id: String,
    /// Distinct authors in source order.
    pub authors: Vec<Author>,
    /// Union of book tags.
    pub tags: BTreeSet<String>,
    /// Earliest book publication date.
    pub release_date: Option<NaiveDate>,
    /// First nonblank summary in numeric book order.
    pub summary: String,
    /// Display number belonging to the selected summary.
    pub summary_number: String,
    /// Persisted creation timestamp.
    pub created_date: Option<NaiveDateTime>,
    /// Persisted modification timestamp.
    pub last_modified_date: Option<NaiveDateTime>,
}

/// Three-state update for fields where explicit null clears persisted data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MetadataUpdate<T> {
    /// Preserve the existing value.
    #[default]
    Keep,
    /// Clear the existing scalar or collection.
    Clear,
    /// Replace the existing value.
    Set(T),
}

/// Editable book fields after HTTP-specific null semantics are resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BookMetadataPatch {
    /// New nonblank title; `None` preserves it.
    pub title: Option<String>,
    /// New title lock state.
    pub title_lock: Option<bool>,
    /// Summary update; clear becomes an empty string.
    pub summary: MetadataUpdate<String>,
    /// New summary lock state.
    pub summary_lock: Option<bool>,
    /// New nonblank display number.
    pub number: Option<String>,
    /// New display-number lock state.
    pub number_lock: Option<bool>,
    /// New numeric sort value.
    pub number_sort: Option<f64>,
    /// New numeric-sort lock state.
    pub number_sort_lock: Option<bool>,
    /// Publication-date update.
    pub release_date: MetadataUpdate<NaiveDate>,
    /// New publication-date lock state.
    pub release_date_lock: Option<bool>,
    /// Author update.
    pub authors: MetadataUpdate<Vec<Author>>,
    /// New authors lock state.
    pub authors_lock: Option<bool>,
    /// Tag update.
    pub tags: MetadataUpdate<BTreeSet<String>>,
    /// New tags lock state.
    pub tags_lock: Option<bool>,
    /// ISBN update.
    pub isbn: MetadataUpdate<String>,
    /// New ISBN lock state.
    pub isbn_lock: Option<bool>,
    /// Link update.
    pub links: MetadataUpdate<Vec<WebLink>>,
    /// New links lock state.
    pub links_lock: Option<bool>,
}

/// Editable series fields after HTTP-specific null semantics are resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesMetadataPatch {
    /// New publication status.
    pub status: Option<SeriesStatus>,
    /// New status lock state.
    pub status_lock: Option<bool>,
    /// New nonblank title.
    pub title: Option<String>,
    /// New title lock state.
    pub title_lock: Option<bool>,
    /// New nonblank sort title.
    pub title_sort: Option<String>,
    /// New sort-title lock state.
    pub title_sort_lock: Option<bool>,
    /// New summary; `None` preserves it.
    pub summary: Option<String>,
    /// New summary lock state.
    pub summary_lock: Option<bool>,
    /// Reading-direction update.
    pub reading_direction: MetadataUpdate<ReadingDirection>,
    /// New reading-direction lock state.
    pub reading_direction_lock: Option<bool>,
    /// New publisher; `None` preserves it.
    pub publisher: Option<String>,
    /// New publisher lock state.
    pub publisher_lock: Option<bool>,
    /// Age-rating update.
    pub age_rating: MetadataUpdate<i32>,
    /// New age-rating lock state.
    pub age_rating_lock: Option<bool>,
    /// New language; `None` preserves it.
    pub language: Option<String>,
    /// New language lock state.
    pub language_lock: Option<bool>,
    /// Genre update.
    pub genres: MetadataUpdate<BTreeSet<String>>,
    /// New genres lock state.
    pub genres_lock: Option<bool>,
    /// Tag update.
    pub tags: MetadataUpdate<BTreeSet<String>>,
    /// New tags lock state.
    pub tags_lock: Option<bool>,
    /// Expected-count update.
    pub total_book_count: MetadataUpdate<i32>,
    /// New expected-count lock state.
    pub total_book_count_lock: Option<bool>,
    /// Sharing-label update.
    pub sharing_labels: MetadataUpdate<BTreeSet<String>>,
    /// New sharing-label lock state.
    pub sharing_labels_lock: Option<bool>,
    /// Link update.
    pub links: MetadataUpdate<Vec<WebLink>>,
    /// New links lock state.
    pub links_lock: Option<bool>,
    /// Alternate-title update.
    pub alternate_titles: MetadataUpdate<Vec<AlternateTitle>>,
    /// New alternate-title lock state.
    pub alternate_titles_lock: Option<bool>,
}

/// Validation error produced by metadata updates or persisted enum parsing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MetadataError {
    /// A supplied or persisted value violates Komga's metadata contract.
    #[error("{0}")]
    Invalid(String),
}

/// Reproduces Kotlin's series aggregation over book metadata.
pub fn aggregate_book_metadata(series_id: &str, metadatas: &[BookMetadata]) -> BookMetadataAggregation {
    let mut authors = Vec::new();
    let mut seen_authors = HashSet::new();
    let mut tags = BTreeSet::new();
    let mut release_date = None;
    for metadata in metadatas {
        for author in &metadata.authors {
            if seen_authors.insert((author.role.clone(), author.name.clone())) {
                authors.push(author.clone());
            }
        }
        tags.extend(metadata.tags.iter().cloned());
        if let Some(date) = metadata.release_date {
            release_date = Some(release_date.map_or(date, |current: NaiveDate| current.min(date)));
        }
    }
    let summary_source = metadatas
        .iter()
        .filter(|metadata| !metadata.summary.trim().is_empty())
        .min_by(|left, right| left.number_sort.total_cmp(&right.number_sort));
    BookMetadataAggregation {
        series_id: series_id.to_owned(),
        authors,
        tags,
        release_date,
        summary: summary_source.map_or_else(String::new, |metadata| metadata.summary.clone()),
        summary_number: summary_source.map_or_else(String::new, |metadata| metadata.number.clone()),
        created_date: None,
        last_modified_date: None,
    }
}

fn apply_string_update(target: &mut String, update: MetadataUpdate<String>) {
    match update {
        MetadataUpdate::Keep => {}
        MetadataUpdate::Clear => target.clear(),
        MetadataUpdate::Set(value) => *target = value,
    }
}

fn apply_optional_update<T>(target: &mut Option<T>, update: MetadataUpdate<T>) {
    match update {
        MetadataUpdate::Keep => {}
        MetadataUpdate::Clear => *target = None,
        MetadataUpdate::Set(value) => *target = Some(value),
    }
}

fn apply_set_update(target: &mut BTreeSet<String>, update: MetadataUpdate<BTreeSet<String>>) {
    match update {
        MetadataUpdate::Keep => {}
        MetadataUpdate::Clear => target.clear(),
        MetadataUpdate::Set(values) => *target = normalize_set(values),
    }
}

fn normalize_set(values: BTreeSet<String>) -> BTreeSet<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn normalize_author(author: &mut Author) {
    author.name = author.name.trim().to_owned();
    author.role = author.role.trim().to_lowercase();
}

fn validate_authors(authors: &[Author]) -> Result<(), MetadataError> {
    for author in authors {
        require_not_blank(&author.name, "author name")?;
        require_not_blank(&author.role, "author role")?;
    }
    Ok(())
}

fn validate_links(links: &[WebLink]) -> Result<(), MetadataError> {
    for link in links {
        require_not_blank(&link.label, "link label")?;
        let url = Url::parse(&link.url).map_err(|_| invalid(format!("invalid link URL: {}", link.url)))?;
        if !url.has_host() {
            return Err(invalid(format!("invalid link URL: {}", link.url)));
        }
    }
    Ok(())
}

fn validate_alternate_titles(titles: &[AlternateTitle]) -> Result<(), MetadataError> {
    for title in titles {
        require_not_blank(&title.label, "alternate title label")?;
        require_not_blank(&title.title, "alternate title")?;
    }
    Ok(())
}

fn normalize_isbn(value: &str) -> Result<String, MetadataError> {
    let digits = value.chars().filter(char::is_ascii_digit).collect::<String>();
    if digits.is_empty() {
        return Ok(digits);
    }
    if digits.len() != 13 {
        return Err(invalid("isbn must be blank or a valid ISBN-13"));
    }
    let sum = digits
        .bytes()
        .enumerate()
        .map(|(index, byte)| u32::from(byte - b'0') * if index % 2 == 0 { 1 } else { 3 })
        .sum::<u32>();
    if sum % 10 != 0 {
        return Err(invalid("isbn must be blank or a valid ISBN-13"));
    }
    Ok(digits)
}

fn normalize_language(value: &str) -> Result<String, MetadataError> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let mut parts = value.split('-');
    let primary = parts.next().unwrap_or_default().to_lowercase();
    let primary = match primary.as_str() {
        "eng" => "en",
        "fra" | "fre" => "fr",
        "deu" | "ger" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "jpn" => "ja",
        "zho" | "chi" => "zh",
        "kor" => "ko",
        "rus" => "ru",
        "ara" => "ar",
        "nld" | "dut" => "nl",
        value => value,
    };
    if primary.len() != 2 || !primary.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(invalid("language must be blank or a valid BCP 47 tag"));
    }
    let mut normalized = primary.to_owned();
    for part in parts {
        if part.is_empty() || part.len() > 8 || !part.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(invalid("language must be blank or a valid BCP 47 tag"));
        }
        normalized.push('-');
        if part.len() == 2 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            normalized.push_str(&part.to_uppercase());
        } else if part.len() == 4 && part.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            let mut characters = part.chars();
            normalized.extend(characters.next().into_iter().flat_map(char::to_uppercase));
            normalized.push_str(&characters.as_str().to_lowercase());
        } else {
            normalized.push_str(&part.to_lowercase());
        }
    }
    Ok(normalized)
}

fn require_not_blank(value: &str, field: &str) -> Result<(), MetadataError> {
    if value.trim().is_empty() {
        Err(invalid(format!("{field} must not be blank")))
    } else {
        Ok(())
    }
}

fn invalid(message: impl Into<String>) -> MetadataError {
    MetadataError::Invalid(message.into())
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    fn timestamp() -> NaiveDateTime {
        DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .naive_utc()
    }

    fn book(number: &str, number_sort: f64, summary: &str) -> BookMetadata {
        BookMetadata {
            book_id: number.into(),
            title: format!("Book {number}"),
            title_lock: false,
            summary: summary.into(),
            summary_lock: false,
            number: number.into(),
            number_lock: false,
            number_sort,
            number_sort_lock: false,
            release_date: None,
            release_date_lock: false,
            authors: Vec::new(),
            authors_lock: false,
            tags: BTreeSet::new(),
            tags_lock: false,
            isbn: String::new(),
            isbn_lock: false,
            links: Vec::new(),
            links_lock: false,
            created_date: timestamp(),
            last_modified_date: timestamp(),
        }
    }

    #[test]
    fn book_patch_preserves_required_nulls_and_clears_nullable_fields() {
        let mut existing = book("1", 1.0, "Summary");
        existing.authors = vec![Author {
            name: " Author ".into(),
            role: "Writer".into(),
        }];
        existing.tags.insert("Existing".into());
        existing.isbn = "9781617290459".into();
        let patched = existing
            .apply_patch(BookMetadataPatch {
                title: None,
                summary: MetadataUpdate::Clear,
                authors: MetadataUpdate::Clear,
                tags: MetadataUpdate::Set([" New ".into(), "".into()].into()),
                isbn: MetadataUpdate::Clear,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(patched.title, existing.title);
        assert!(patched.summary.is_empty());
        assert!(patched.authors.is_empty());
        assert_eq!(patched.tags, ["new".into()].into());
        assert!(patched.isbn.is_empty());
    }

    #[test]
    fn book_patch_normalizes_authors_and_validates_isbn_and_links() {
        let existing = book("1", 1.0, "");
        let patched = existing
            .apply_patch(BookMetadataPatch {
                authors: MetadataUpdate::Set(vec![Author {
                    name: " Jane Doe ".into(),
                    role: " Writer ".into(),
                }]),
                isbn: MetadataUpdate::Set("978-161-729-045-9abc".into()),
                links: MetadataUpdate::Set(vec![WebLink {
                    label: "Source".into(),
                    url: "https://example.org/book".into(),
                }]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(patched.authors[0].name, "Jane Doe");
        assert_eq!(patched.authors[0].role, "writer");
        assert_eq!(patched.isbn, "9781617290459");
        assert!(
            existing
                .apply_patch(BookMetadataPatch {
                    isbn: MetadataUpdate::Set("1617290459".into()),
                    ..Default::default()
                })
                .is_err()
        );
    }

    #[test]
    fn series_patch_normalizes_collections_language_and_clearable_values() {
        let existing = SeriesMetadata {
            series_id: "series".into(),
            status: SeriesStatus::Ongoing,
            status_lock: false,
            title: "Series".into(),
            title_lock: false,
            title_sort: "Series".into(),
            title_sort_lock: false,
            summary: String::new(),
            summary_lock: false,
            reading_direction: Some(ReadingDirection::RightToLeft),
            reading_direction_lock: false,
            publisher: String::new(),
            publisher_lock: false,
            age_rating: Some(12),
            age_rating_lock: false,
            language: String::new(),
            language_lock: false,
            genres: BTreeSet::new(),
            genres_lock: false,
            tags: BTreeSet::new(),
            tags_lock: false,
            total_book_count: Some(5),
            total_book_count_lock: false,
            sharing_labels: BTreeSet::new(),
            sharing_labels_lock: false,
            links: Vec::new(),
            links_lock: false,
            alternate_titles: Vec::new(),
            alternate_titles_lock: false,
            created_date: timestamp(),
            last_modified_date: timestamp(),
        };
        let patched = existing
            .apply_patch(SeriesMetadataPatch {
                language: Some("fra".into()),
                reading_direction: MetadataUpdate::Clear,
                age_rating: MetadataUpdate::Clear,
                genres: MetadataUpdate::Set([" Action ".into(), "".into()].into()),
                total_book_count: MetadataUpdate::Clear,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(patched.language, "fr");
        assert_eq!(patched.reading_direction, None);
        assert_eq!(patched.age_rating, None);
        assert_eq!(patched.genres, ["action".into()].into());
        assert_eq!(patched.total_book_count, None);
    }

    #[test]
    fn aggregation_matches_kotlin_order_union_and_summary_rules() {
        let mut second = book("2", 2.0, "Second summary");
        second.release_date = NaiveDate::from_ymd_opt(2022, 1, 1);
        second.authors = vec![Author {
            name: "Writer".into(),
            role: "writer".into(),
        }];
        second.tags.insert("later".into());
        let mut first = book("1", 1.0, "First summary");
        first.release_date = NaiveDate::from_ymd_opt(2020, 1, 1);
        first.authors = vec![
            Author {
                name: "Writer".into(),
                role: "writer".into(),
            },
            Author {
                name: "Artist".into(),
                role: "penciller".into(),
            },
        ];
        first.tags.insert("earlier".into());
        let aggregate = aggregate_book_metadata("series", &[second, first]);
        assert_eq!(aggregate.authors.len(), 2);
        assert_eq!(aggregate.tags, ["earlier".into(), "later".into()].into());
        assert_eq!(aggregate.release_date, NaiveDate::from_ymd_opt(2020, 1, 1));
        assert_eq!(aggregate.summary, "First summary");
        assert_eq!(aggregate.summary_number, "1");
    }
}
