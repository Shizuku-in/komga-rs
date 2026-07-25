//! Duplicate-page hash rules and matches.

use serde::{Deserialize, Serialize};

/// Administrative action associated with a known duplicate page hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PageHashAction {
    /// Delete matching pages when duplicate-page discovery runs.
    DeleteAuto,
    /// Retain matches until an administrator explicitly requests deletion.
    DeleteManual,
    /// Keep matching pages.
    Ignore,
}

impl PageHashAction {
    /// Returns the Kotlin-compatible persisted and API identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DeleteAuto => "DELETE_AUTO",
            Self::DeleteManual => "DELETE_MANUAL",
            Self::Ignore => "IGNORE",
        }
    }
}

impl TryFrom<&str> for PageHashAction {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "DELETE_AUTO" => Ok(Self::DeleteAuto),
            "DELETE_MANUAL" => Ok(Self::DeleteManual),
            "IGNORE" => Ok(Self::Ignore),
            _ => Err(()),
        }
    }
}

/// One classified page hash returned by the administration API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHashKnown {
    /// Page content hash.
    pub hash: String,
    /// Uncompressed page size, when known.
    pub size: Option<i64>,
    /// Administrative action.
    pub action: PageHashAction,
    /// Number of pages removed under this rule.
    pub delete_count: i32,
    /// Number of current matching pages.
    pub match_count: i32,
    /// UTC-compatible creation timestamp.
    pub created: String,
    /// UTC-compatible modification timestamp.
    pub last_modified: String,
}

/// One unclassified hash shared by at least two pages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHashUnknown {
    /// Page content hash.
    pub hash: String,
    /// Uncompressed page size, when known.
    pub size: Option<i64>,
    /// Number of current matching pages.
    pub match_count: i32,
}

/// One analyzed page matching a content hash.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageHashMatch {
    /// Owning book identifier.
    pub book_id: String,
    /// Local filesystem path exposed by the administrative API.
    pub url: String,
    /// One-based page number.
    pub page_number: i32,
    /// Exact ZIP entry name.
    pub file_name: String,
    /// Uncompressed entry size.
    pub file_size: i64,
    /// Detected page media type.
    pub media_type: String,
}

/// Snapshot-bound page target persisted in a removal task.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HashedPageTarget {
    /// Exact ZIP entry name.
    pub file_name: String,
    /// Detected page media type.
    pub media_type: String,
    /// Page content hash.
    pub file_hash: String,
    /// Uncompressed entry size.
    pub file_size: i64,
    /// One-based page number.
    pub page_number: i32,
}
