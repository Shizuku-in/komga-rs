//! Framework-independent projections and query contracts for library content.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;

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
    /// Zero-based page index.
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

/// Read states supported by Komga search conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReadStatus {
    /// No progress exists.
    Unread,
    /// Progress is complete.
    Read,
    /// Progress exists but is not complete.
    InProgress,
}

/// Equality operator used by identifier and status conditions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum EqualityOperator {
    /// Match the supplied value.
    Is {
        /// Value to match.
        value: String,
    },
    /// Exclude the supplied value.
    IsNot {
        /// Value to exclude.
        value: String,
    },
}

/// Equality operator for read-status conditions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReadStatusOperator {
    /// Match the supplied status.
    Is {
        /// Status to match.
        value: ReadStatus,
    },
    /// Exclude the supplied status.
    IsNot {
        /// Status to exclude.
        value: ReadStatus,
    },
}

/// Boolean operator used by deleted and one-shot conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum BooleanOperator {
    /// Match true values.
    IsTrue,
    /// Match false values.
    IsFalse,
}

/// String operator supported by title conditions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum StringOperator {
    /// Match the complete string case-insensitively.
    Is {
        /// Value to match.
        value: String,
    },
    /// Exclude the complete string case-insensitively.
    IsNot {
        /// Value to exclude.
        value: String,
    },
    /// Match strings containing the value case-insensitively.
    Contains {
        /// Value that must occur in the field.
        value: String,
    },
    /// Exclude strings containing the value case-insensitively.
    DoesNotContain {
        /// Value that must not occur in the field.
        value: String,
    },
    /// Match strings beginning with the value case-insensitively.
    BeginsWith {
        /// Required field prefix.
        value: String,
    },
    /// Exclude strings beginning with the value case-insensitively.
    DoesNotBeginWith {
        /// Forbidden field prefix.
        value: String,
    },
    /// Match strings ending with the value case-insensitively.
    EndsWith {
        /// Required field suffix.
        value: String,
    },
    /// Exclude strings ending with the value case-insensitively.
    DoesNotEndWith {
        /// Forbidden field suffix.
        value: String,
    },
}

/// Supported Series search-condition tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeriesCondition {
    /// Every nested condition must match.
    AllOf {
        /// Nested conditions.
        conditions: Vec<SeriesCondition>,
    },
    /// At least one nested condition must match.
    AnyOf {
        /// Nested conditions.
        conditions: Vec<SeriesCondition>,
    },
    /// Match an owning library identifier.
    LibraryId {
        /// Equality operation.
        operator: EqualityOperator,
    },
    /// Match soft-deletion state.
    Deleted {
        /// Boolean operation.
        deleted: BooleanOperator,
    },
    /// Match one-shot state.
    OneShot {
        /// Boolean operation.
        operator: BooleanOperator,
    },
    /// Match the display title.
    Title {
        /// String operation.
        title: StringOperator,
    },
    /// Match the sort title.
    TitleSort {
        /// String operation.
        operator: StringOperator,
    },
    /// Match publication status.
    SeriesStatus {
        /// Equality operation.
        operator: EqualityOperator,
    },
    /// Match the requesting user's read status.
    ReadStatus {
        /// Read-status operation.
        operator: ReadStatusOperator,
    },
}

impl<'de> Deserialize<'de> for SeriesCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Value::Object(object) = value else {
            return Err(D::Error::custom("series condition must be an object"));
        };
        if object.len() != 1 {
            return Err(D::Error::custom(
                "series condition must contain exactly one supported field",
            ));
        }
        let (field, value) = object.into_iter().next().expect("object contains one entry");
        let parsed = match field.as_str() {
            "allOf" => from_json(value).map(|conditions| Self::AllOf { conditions }),
            "anyOf" => from_json(value).map(|conditions| Self::AnyOf { conditions }),
            "libraryId" => from_json(value).map(|operator| Self::LibraryId { operator }),
            "deleted" => from_json(value).map(|deleted| Self::Deleted { deleted }),
            "oneShot" => from_json(value).map(|operator| Self::OneShot { operator }),
            "title" => from_json(value).map(|title| Self::Title { title }),
            "titleSort" => from_json(value).map(|operator| Self::TitleSort { operator }),
            "seriesStatus" => from_json(value).map(|operator| Self::SeriesStatus { operator }),
            "readStatus" => from_json(value).map(|operator| Self::ReadStatus { operator }),
            _ => return Err(D::Error::custom(format!("unsupported series condition: {field}"))),
        };
        parsed.map_err(D::Error::custom)
    }
}

/// Request body accepted by the modern Series list endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SeriesSearch {
    /// Optional condition tree.
    pub condition: Option<SeriesCondition>,
    /// Lucene-compatible search text, deferred to the search phase.
    pub full_text_search: Option<String>,
}

/// Supported Book search-condition tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookCondition {
    /// Every nested condition must match.
    AllOf {
        /// Nested conditions.
        conditions: Vec<BookCondition>,
    },
    /// At least one nested condition must match.
    AnyOf {
        /// Nested conditions.
        conditions: Vec<BookCondition>,
    },
    /// Match an owning library identifier.
    LibraryId {
        /// Equality operation.
        operator: EqualityOperator,
    },
    /// Match a parent series identifier.
    SeriesId {
        /// Equality operation.
        operator: EqualityOperator,
    },
    /// Match soft-deletion state.
    Deleted {
        /// Boolean operation.
        deleted: BooleanOperator,
    },
    /// Match one-shot state.
    OneShot {
        /// Boolean operation.
        operator: BooleanOperator,
    },
    /// Match the metadata title.
    Title {
        /// String operation.
        title: StringOperator,
    },
    /// Match media-analysis status.
    MediaStatus {
        /// Equality operation.
        operator: EqualityOperator,
    },
    /// Match the requesting user's read status.
    ReadStatus {
        /// Read-status operation.
        operator: ReadStatusOperator,
    },
}

impl<'de> Deserialize<'de> for BookCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Value::Object(object) = value else {
            return Err(D::Error::custom("book condition must be an object"));
        };
        if object.len() != 1 {
            return Err(D::Error::custom(
                "book condition must contain exactly one supported field",
            ));
        }
        let (field, value) = object.into_iter().next().expect("object contains one entry");
        let parsed = match field.as_str() {
            "allOf" => from_json(value).map(|conditions| Self::AllOf { conditions }),
            "anyOf" => from_json(value).map(|conditions| Self::AnyOf { conditions }),
            "libraryId" => from_json(value).map(|operator| Self::LibraryId { operator }),
            "seriesId" => from_json(value).map(|operator| Self::SeriesId { operator }),
            "deleted" => from_json(value).map(|deleted| Self::Deleted { deleted }),
            "oneShot" => from_json(value).map(|operator| Self::OneShot { operator }),
            "title" => from_json(value).map(|title| Self::Title { title }),
            "mediaStatus" => from_json(value).map(|operator| Self::MediaStatus { operator }),
            "readStatus" => from_json(value).map(|operator| Self::ReadStatus { operator }),
            _ => return Err(D::Error::custom(format!("unsupported book condition: {field}"))),
        };
        parsed.map_err(D::Error::custom)
    }
}

fn from_json<T>(value: Value) -> std::result::Result<T, serde_json::Error>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value)
}

/// Request body accepted by the modern Book list endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BookSearch {
    /// Optional condition tree.
    pub condition: Option<BookCondition>,
    /// Lucene-compatible search text, deferred to the search phase.
    pub full_text_search: Option<String>,
}

/// Direction of one requested sort field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Ascending order.
    Ascending,
    /// Descending order.
    Descending,
}

/// A validated sort request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortOrder {
    /// API sort property.
    pub property: String,
    /// Sort direction.
    pub direction: SortDirection,
}

/// Pagination and sorting supplied to storage queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Zero-based page number.
    pub page: u64,
    /// Requested page size.
    pub size: u64,
    /// Whether all matching rows should be returned.
    pub unpaged: bool,
    /// Ordered sort fields.
    pub sort: Vec<SortOrder>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            page: 0,
            size: 20,
            unpaged: false,
            sort: Vec::new(),
        }
    }
}

/// One page returned by content persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentPage<T> {
    /// Rows in this page.
    pub content: Vec<T>,
    /// Total matching row count.
    pub total_elements: u64,
    /// Effective zero-based page number.
    pub page: u64,
    /// Effective page size.
    pub size: u64,
    /// Sort fields applied to the query.
    pub sort: Vec<SortOrder>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_supported_search_conditions() {
        let search: BookSearch = serde_json::from_str(
            r#"{"condition":{"allOf":[{"seriesId":{"operator":"is","value":"series"}},{"deleted":{"operator":"isFalse"}}]}}"#,
        )
        .unwrap();
        assert!(matches!(search.condition, Some(BookCondition::AllOf { conditions }) if conditions.len() == 2));
    }

    #[test]
    fn rejects_unsupported_or_ambiguous_search_conditions() {
        assert!(
            serde_json::from_str::<SeriesSearch>(
                r#"{"condition":{"collectionId":{"operator":"is","value":"collection"}}}"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<BookSearch>(
                r#"{"condition":{"seriesId":{"operator":"is","value":"series"},"poster":{"operator":"isTrue"}}}"#,
            )
            .is_err()
        );
    }
}
