//! Collection and read-list domain contracts.

use serde::{Deserialize, Serialize};

/// A parsed ComicRack reading-list request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadListRequest {
    /// Requested reading-list name.
    pub name: String,
    /// Books in source order.
    pub books: Vec<ReadListRequestBook>,
}

/// A ComicRack book reference to match against persisted metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListRequestBook {
    /// Candidate series titles, in preference order.
    pub series: Vec<String>,
    /// ComicRack issue number.
    pub number: String,
}

/// Complete preview of a ComicRack reading-list match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListRequestMatchDto {
    /// Match result for the requested reading-list name.
    pub read_list_match: ReadListMatchDto,
    /// Per-book candidates in source order.
    pub requests: Vec<ReadListRequestBookMatchesDto>,
    /// Request-level error code; empty for a successful preview.
    pub error_code: String,
}

/// Match result for the requested reading-list name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListMatchDto {
    /// Requested name.
    pub name: String,
    /// `ERR_1009` when a normalized duplicate exists, otherwise empty.
    pub error_code: String,
}

/// Candidate matches for one source book.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListRequestBookMatchesDto {
    /// Original source request.
    pub request: ReadListRequestBook,
    /// Candidate books grouped by matching series.
    pub matches: Vec<ReadListRequestBookMatchDto>,
}

/// Candidate books within one matching series.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListRequestBookMatchDto {
    /// Candidate series.
    pub series: ReadListRequestBookMatchSeriesDto,
    /// All books in the series with the requested number.
    pub books: Vec<ReadListRequestBookMatchBookDto>,
}

/// A series matching a ComicRack series title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListRequestBookMatchSeriesDto {
    /// Stable series identifier.
    pub series_id: String,
    /// Persisted series title.
    pub title: String,
    /// Aggregated publication date in `yyyy-MM-dd` form.
    pub release_date: Option<String>,
}

/// A book matching a ComicRack issue number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListRequestBookMatchBookDto {
    /// Stable book identifier.
    pub book_id: String,
    /// Persisted issue number.
    pub number: String,
    /// Persisted book title.
    pub title: String,
}

/// A collection projected for the requesting user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDto {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Whether members use their explicit order.
    pub ordered: bool,
    /// Visible member identifiers in stored order.
    pub series_ids: Vec<String>,
    /// UTC-compatible creation timestamp.
    pub created_date: String,
    /// UTC-compatible modification timestamp.
    pub last_modified_date: String,
    /// Whether one or more stored members were hidden.
    pub filtered: bool,
}

/// A read list projected for the requesting user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListDto {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional descriptive text.
    pub summary: String,
    /// Whether members use their explicit order.
    pub ordered: bool,
    /// Visible member identifiers in stored order.
    pub book_ids: Vec<String>,
    /// UTC-compatible creation timestamp.
    pub created_date: String,
    /// UTC-compatible modification timestamp.
    pub last_modified_date: String,
    /// Whether one or more stored members were hidden.
    pub filtered: bool,
}

/// Collection creation payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionCreation {
    /// Display name.
    pub name: String,
    /// Whether members use their explicit order.
    pub ordered: bool,
    /// Ordered series identifiers.
    pub series_ids: Vec<String>,
}

/// Read-list creation payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadListCreation {
    /// Display name.
    pub name: String,
    /// Optional descriptive text.
    #[serde(default)]
    pub summary: String,
    /// Whether members use their explicit order.
    #[serde(default = "default_true")]
    pub ordered: bool,
    /// Ordered book identifiers.
    pub book_ids: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Read-list progress summary used by Mihon/Tachiyomi clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadListReadProgressDto {
    /// Number of visible books.
    pub books_count: i32,
    /// Number of completed books.
    pub books_read_count: i32,
    /// Number of books without progress.
    pub books_unread_count: i32,
    /// Number of partially read books.
    pub books_in_progress_count: i32,
    /// One-based index of the last continuously completed member.
    pub last_read_continuous_index: i32,
}

/// Mihon bulk completion payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadListProgressUpdate {
    /// Complete the first this-many visible books.
    pub last_book_read: u32,
}
