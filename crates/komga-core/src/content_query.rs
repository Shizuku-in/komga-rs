//! Framework-independent query contracts for library content.

use serde::{Deserialize, Deserializer, de::Error as _};
use serde_json::Value;

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
    /// Match membership in a collection.
    CollectionId {
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
            "collectionId" => from_json(value).map(|operator| Self::CollectionId { operator }),
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
    /// Match membership in a read list.
    ReadListId {
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
            "readListId" => from_json(value).map(|operator| Self::ReadListId { operator }),
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
        assert!(serde_json::from_str::<SeriesSearch>(r#"{"condition":{"poster":{"operator":"isTrue"}}}"#,).is_err());
        assert!(
            serde_json::from_str::<BookSearch>(
                r#"{"condition":{"seriesId":{"operator":"is","value":"series"},"poster":{"operator":"isTrue"}}}"#,
            )
            .is_err()
        );
    }
}
