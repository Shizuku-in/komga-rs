//! Framework-independent projections and query contracts for library content.

use std::collections::BTreeSet;

use serde::Serialize;

/// An author name and role attached to book metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorDto {
    /// Display name.
    pub name: String,
    /// Komga author role, such as `writer` or `penciller`.
    pub role: String,
}

/// A labelled external link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebLinkDto {
    /// User-visible label.
    pub label: String,
    /// Link target.
    pub url: String,
}

/// An alternate title qualified by a label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlternateTitleDto {
    /// Alternate-title label.
    pub label: String,
    /// Alternate title value.
    pub title: String,
}

/// Metadata returned as part of a series representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesMetadataDto {
    /// Publication status.
    pub status: String,
    /// Whether status is locked against automatic updates.
    pub status_lock: bool,
    /// Display title.
    pub title: String,
    /// Whether title is locked.
    pub title_lock: bool,
    /// Sort title.
    pub title_sort: String,
    /// Whether sort title is locked.
    pub title_sort_lock: bool,
    /// Series summary.
    pub summary: String,
    /// Whether summary is locked.
    pub summary_lock: bool,
    /// Reading-direction identifier, or an empty string.
    pub reading_direction: String,
    /// Whether reading direction is locked.
    pub reading_direction_lock: bool,
    /// Publisher name.
    pub publisher: String,
    /// Whether publisher is locked.
    pub publisher_lock: bool,
    /// Optional age rating.
    pub age_rating: Option<i32>,
    /// Whether age rating is locked.
    pub age_rating_lock: bool,
    /// Language identifier.
    pub language: String,
    /// Whether language is locked.
    pub language_lock: bool,
    /// Series genres.
    pub genres: BTreeSet<String>,
    /// Whether genres are locked.
    pub genres_lock: bool,
    /// Series tags.
    pub tags: BTreeSet<String>,
    /// Whether tags are locked.
    pub tags_lock: bool,
    /// Optional expected total number of books.
    pub total_book_count: Option<i32>,
    /// Whether total book count is locked.
    pub total_book_count_lock: bool,
    /// Labels used by content restrictions.
    pub sharing_labels: BTreeSet<String>,
    /// Whether sharing labels are locked.
    pub sharing_labels_lock: bool,
    /// External links.
    pub links: Vec<WebLinkDto>,
    /// Whether links are locked.
    pub links_lock: bool,
    /// Alternate titles.
    pub alternate_titles: Vec<AlternateTitleDto>,
    /// Whether alternate titles are locked.
    pub alternate_titles_lock: bool,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
}

/// Metadata aggregated from all books in a series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMetadataAggregationDto {
    /// Aggregated authors.
    pub authors: Vec<AuthorDto>,
    /// Aggregated tags.
    pub tags: BTreeSet<String>,
    /// Earliest or representative release date.
    pub release_date: Option<String>,
    /// Aggregated summary.
    pub summary: String,
    /// Number associated with the aggregated summary.
    pub summary_number: String,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
}

/// Series representation exposed by Komga's REST API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesDto {
    /// Stable series identifier.
    pub id: String,
    /// Owning library identifier.
    pub library_id: String,
    /// Filesystem-derived name.
    pub name: String,
    /// Filesystem path, redacted for non-administrators.
    pub url: String,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
    /// UTC-compatible filesystem modification timestamp.
    pub file_last_modified: String,
    /// Total number of books.
    pub books_count: i32,
    /// Number of completed books for the requesting user.
    pub books_read_count: i32,
    /// Number of unread books for the requesting user.
    pub books_unread_count: i32,
    /// Number of in-progress books for the requesting user.
    pub books_in_progress_count: i32,
    /// Series metadata.
    pub metadata: SeriesMetadataDto,
    /// Metadata aggregated from books.
    pub books_metadata: BookMetadataAggregationDto,
    /// Whether the series is soft-deleted.
    pub deleted: bool,
    /// Whether this is a one-shot series.
    pub oneshot: bool,
}

/// Analyzed-media state returned with a book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDto {
    /// Analysis status.
    pub status: String,
    /// Detected media type, or an empty string.
    pub media_type: String,
    /// Number of discovered pages.
    pub pages_count: i32,
    /// Analysis comment, or an empty string.
    pub comment: String,
    /// Derived Komga media profile, or an empty string.
    pub media_profile: String,
    /// Whether the EPUB can be rendered through Divina.
    pub epub_divina_compatible: bool,
    /// Whether the EPUB is already in KEPUB form.
    pub epub_is_kepub: bool,
}

/// Metadata returned as part of a book representation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookMetadataDto {
    /// Display title.
    pub title: String,
    /// Whether title is locked.
    pub title_lock: bool,
    /// Book summary.
    pub summary: String,
    /// Whether summary is locked.
    pub summary_lock: bool,
    /// Display number.
    pub number: String,
    /// Whether display number is locked.
    pub number_lock: bool,
    /// Numeric sort value.
    pub number_sort: f64,
    /// Whether the sort value is locked.
    pub number_sort_lock: bool,
    /// Optional release date.
    pub release_date: Option<String>,
    /// Whether release date is locked.
    pub release_date_lock: bool,
    /// Book authors.
    pub authors: Vec<AuthorDto>,
    /// Whether authors are locked.
    pub authors_lock: bool,
    /// Book tags.
    pub tags: BTreeSet<String>,
    /// Whether tags are locked.
    pub tags_lock: bool,
    /// ISBN value.
    pub isbn: String,
    /// Whether ISBN is locked.
    pub isbn_lock: bool,
    /// External links.
    pub links: Vec<WebLinkDto>,
    /// Whether links are locked.
    pub links_lock: bool,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
}

/// Per-user reading progress returned with a book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadProgressDto {
    /// One-based page number.
    pub page: i32,
    /// Whether the book is complete.
    pub completed: bool,
    /// UTC-compatible read timestamp.
    pub read_date: String,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
    /// Originating device identifier.
    pub device_id: String,
    /// Originating device name.
    pub device_name: String,
}

/// Book representation exposed by Komga's REST API.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookDto {
    /// Stable book identifier.
    pub id: String,
    /// Parent series identifier.
    pub series_id: String,
    /// Parent series title.
    pub series_title: String,
    /// Owning library identifier.
    pub library_id: String,
    /// Filesystem-derived name.
    pub name: String,
    /// Filesystem path, reduced to a filename for non-administrators.
    pub url: String,
    /// Stable scanner sequence number.
    pub number: i32,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
    /// UTC-compatible filesystem modification timestamp.
    pub file_last_modified: String,
    /// Source file size in bytes.
    pub size_bytes: i64,
    /// Human-readable binary file size.
    pub size: String,
    /// Media-analysis state.
    pub media: MediaDto,
    /// Book metadata.
    pub metadata: BookMetadataDto,
    /// Progress for the requesting user.
    pub read_progress: Option<ReadProgressDto>,
    /// Whether the book is soft-deleted.
    pub deleted: bool,
    /// Optional persisted file hash.
    pub file_hash: String,
    /// Whether this is a one-shot book.
    pub oneshot: bool,
}
