//! Collection and read-list domain contracts.

use serde::{Deserialize, Serialize};

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
