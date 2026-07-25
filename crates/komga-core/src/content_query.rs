//! Framework-independent query contracts for library content.

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;

use crate::{MediaProfile, MediaStatus, SeriesStatus};

/// Count of series whose sort title begins with one normalized character.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroupCountDto {
    /// Lowercase first character, or an empty string for an empty sort title.
    pub group: String,
    /// Number of matching series in the group.
    pub count: i64,
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

/// Equality operator used by scalar search conditions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum EqualityOperator<T = String> {
    /// Match the supplied value.
    Is {
        /// Value to match.
        value: T,
    },
    /// Exclude the supplied value.
    IsNot {
        /// Value to exclude.
        value: T,
    },
}

/// Equality operator for nullable fields or related-value collections.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum NullableEqualityOperator<T> {
    /// Match the supplied value.
    Is {
        /// Value to match.
        value: T,
    },
    /// Exclude the supplied value.
    IsNot {
        /// Value to exclude.
        value: T,
    },
    /// Match missing values.
    IsNull,
    /// Match present values.
    IsNotNull,
}

/// Numeric comparison operator.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum NumericOperator<T> {
    /// Match the supplied value.
    Is {
        /// Value to match.
        value: T,
    },
    /// Exclude the supplied value.
    IsNot {
        /// Value to exclude.
        value: T,
    },
    /// Match values greater than the supplied value.
    GreaterThan {
        /// Exclusive lower bound.
        value: T,
    },
    /// Match values less than the supplied value.
    LessThan {
        /// Exclusive upper bound.
        value: T,
    },
}

/// Numeric comparison operator for nullable fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
pub enum NullableNumericOperator<T> {
    /// Match the supplied value.
    Is {
        /// Value to match.
        value: T,
    },
    /// Exclude the supplied value, including null fields.
    IsNot {
        /// Value to exclude.
        value: T,
    },
    /// Match values greater than the supplied value.
    GreaterThan {
        /// Exclusive lower bound.
        value: T,
    },
    /// Match values less than the supplied value.
    LessThan {
        /// Exclusive upper bound.
        value: T,
    },
    /// Match null fields.
    IsNull,
    /// Match non-null fields.
    IsNotNull,
}

/// Date comparison operator.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum DateOperator {
    /// Match dates strictly before the supplied instant's UTC date.
    Before { date_time: DateTime<FixedOffset> },
    /// Match dates strictly after the supplied instant's UTC date.
    After { date_time: DateTime<FixedOffset> },
    /// Match dates strictly newer than the current UTC date minus this many days.
    IsInTheLast { days: i64 },
    /// Match dates strictly older than the current UTC date minus this many days.
    IsNotInTheLast { days: i64 },
    /// Match null dates.
    IsNull,
    /// Match non-null dates.
    IsNotNull,
}

impl<'de> Deserialize<'de> for DateOperator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "operator", rename_all = "camelCase", deny_unknown_fields)]
        enum RawDateOperator {
            Before {
                #[serde(rename = "dateTime")]
                date_time: DateTime<FixedOffset>,
            },
            After {
                #[serde(rename = "dateTime")]
                date_time: DateTime<FixedOffset>,
            },
            IsInTheLast {
                duration: String,
            },
            IsNotInTheLast {
                duration: String,
            },
            IsNull,
            IsNotNull,
        }

        match RawDateOperator::deserialize(deserializer)? {
            RawDateOperator::Before { date_time } => Ok(Self::Before { date_time }),
            RawDateOperator::After { date_time } => Ok(Self::After { date_time }),
            RawDateOperator::IsInTheLast { duration } => parse_duration_days(&duration)
                .map(|days| Self::IsInTheLast { days })
                .map_err(D::Error::custom),
            RawDateOperator::IsNotInTheLast { duration } => parse_duration_days(&duration)
                .map(|days| Self::IsNotInTheLast { days })
                .map_err(D::Error::custom),
            RawDateOperator::IsNull => Ok(Self::IsNull),
            RawDateOperator::IsNotNull => Ok(Self::IsNotNull),
        }
    }
}

fn parse_duration_days(value: &str) -> Result<i64, String> {
    let duration = value
        .parse::<iso8601_duration::Duration>()
        .map_err(|error| format!("invalid ISO 8601 duration: {error:?}"))?;
    if duration.year != 0.0 || duration.month != 0.0 {
        return Err("relative date durations cannot contain years or months".into());
    }
    let seconds = duration
        .num_seconds()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .ok_or_else(|| "relative date duration is out of range".to_owned())?;
    let days = f64::from(seconds) / 86_400.0;
    if days > i64::MAX as f64 {
        return Err("relative date duration is out of range".into());
    }
    Ok(days.trunc() as i64)
}

/// Boolean operator used by deleted, complete, and one-shot conditions.
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
#[allow(missing_docs)]
pub enum StringOperator {
    /// Match the complete string case-insensitively.
    Is { value: String },
    /// Exclude the complete string case-insensitively.
    IsNot { value: String },
    /// Match strings containing the value case-insensitively.
    Contains { value: String },
    /// Exclude strings containing the value case-insensitively.
    DoesNotContain { value: String },
    /// Match strings beginning with the value case-insensitively.
    BeginsWith { value: String },
    /// Exclude strings beginning with the value case-insensitively.
    DoesNotBeginWith { value: String },
    /// Match strings ending with the value case-insensitively.
    EndsWith { value: String },
    /// Exclude strings ending with the value case-insensitively.
    DoesNotEndWith { value: String },
}

/// Author fields matched together against one author relation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorMatch {
    /// Exact author name, or any name when omitted.
    pub name: Option<String>,
    /// Exact author role, or any role when omitted.
    pub role: Option<String>,
}

/// Persisted book poster origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PosterType {
    /// Generated from book content.
    Generated,
    /// Loaded from a sidecar image.
    Sidecar,
    /// Uploaded by a user.
    UserUploaded,
}

impl PosterType {
    /// Returns the value stored in `THUMBNAIL_BOOK.TYPE`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "GENERATED",
            Self::Sidecar => "SIDECAR",
            Self::UserUploaded => "USER_UPLOADED",
        }
    }
}

/// Poster fields matched together against one thumbnail relation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PosterMatch {
    /// Poster origin, or any origin when omitted.
    #[serde(rename = "type")]
    pub poster_type: Option<PosterType>,
    /// Selection state, or either state when omitted.
    pub selected: Option<bool>,
}

/// Supported Series search-condition tree.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum SeriesCondition {
    /// Every nested condition must match.
    AllOf { conditions: Vec<SeriesCondition> },
    /// At least one nested condition must match.
    AnyOf { conditions: Vec<SeriesCondition> },
    /// Match an owning library identifier.
    LibraryId { operator: EqualityOperator },
    /// Match membership in a collection.
    CollectionId { operator: EqualityOperator },
    /// Match soft-deletion state.
    Deleted { operator: BooleanOperator },
    /// Match metadata completeness.
    Complete { operator: BooleanOperator },
    /// Match one-shot state.
    OneShot { operator: BooleanOperator },
    /// Match the display title.
    Title { operator: StringOperator },
    /// Match the sort title.
    TitleSort { operator: StringOperator },
    /// Match the aggregated book release date.
    ReleaseDate { operator: DateOperator },
    /// Match series or aggregated book tags.
    Tag { operator: NullableEqualityOperator<String> },
    /// Match sharing labels.
    SharingLabel { operator: NullableEqualityOperator<String> },
    /// Match the publisher.
    Publisher { operator: EqualityOperator },
    /// Match the language.
    Language { operator: EqualityOperator },
    /// Match genres.
    Genre { operator: NullableEqualityOperator<String> },
    /// Match the nullable age rating.
    AgeRating { operator: NullableNumericOperator<i32> },
    /// Match publication status.
    SeriesStatus { operator: EqualityOperator<SeriesStatus> },
    /// Match the requesting user's read status.
    ReadStatus { operator: EqualityOperator<ReadStatus> },
    /// Match aggregated authors.
    Author { operator: EqualityOperator<AuthorMatch> },
}

impl<'de> Deserialize<'de> for SeriesCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_condition(deserializer, |field, value| match field {
            "allOf" => from_json(value).map(|conditions| Self::AllOf { conditions }),
            "anyOf" => from_json(value).map(|conditions| Self::AnyOf { conditions }),
            "libraryId" => from_json(value).map(|operator| Self::LibraryId { operator }),
            "collectionId" => from_json(value).map(|operator| Self::CollectionId { operator }),
            "deleted" => from_json(value).map(|operator| Self::Deleted { operator }),
            "complete" => from_json(value).map(|operator| Self::Complete { operator }),
            "oneShot" => from_json(value).map(|operator| Self::OneShot { operator }),
            "title" => from_json(value).map(|operator| Self::Title { operator }),
            "titleSort" => from_json(value).map(|operator| Self::TitleSort { operator }),
            "releaseDate" => from_json(value).map(|operator| Self::ReleaseDate { operator }),
            "tag" => from_json(value).map(|operator| Self::Tag { operator }),
            "sharingLabel" => from_json(value).map(|operator| Self::SharingLabel { operator }),
            "publisher" => from_json(value).map(|operator| Self::Publisher { operator }),
            "language" => from_json(value).map(|operator| Self::Language { operator }),
            "genre" => from_json(value).map(|operator| Self::Genre { operator }),
            "ageRating" => from_json(value).map(|operator| Self::AgeRating { operator }),
            "seriesStatus" => from_json(value).map(|operator| Self::SeriesStatus { operator }),
            "readStatus" => from_json(value).map(|operator| Self::ReadStatus { operator }),
            "author" => from_json(value).map(|operator| Self::Author { operator }),
            _ => Err(format!("unsupported series condition: {field}")),
        })
    }
}

/// Request body accepted by the modern Series list endpoint.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SeriesSearch {
    /// Optional condition tree.
    pub condition: Option<SeriesCondition>,
    /// Lucene-compatible search text, deferred to the search phase.
    pub full_text_search: Option<String>,
}

/// Supported Book search-condition tree.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum BookCondition {
    /// Every nested condition must match.
    AllOf { conditions: Vec<BookCondition> },
    /// At least one nested condition must match.
    AnyOf { conditions: Vec<BookCondition> },
    /// Match an owning library identifier.
    LibraryId { operator: EqualityOperator },
    /// Match a parent series identifier.
    SeriesId { operator: EqualityOperator },
    /// Match membership in a read list.
    ReadListId { operator: EqualityOperator },
    /// Match soft-deletion state.
    Deleted { operator: BooleanOperator },
    /// Match one-shot state.
    OneShot { operator: BooleanOperator },
    /// Match the metadata title.
    Title { operator: StringOperator },
    /// Match the metadata release date.
    ReleaseDate { operator: DateOperator },
    /// Match metadata tags.
    Tag { operator: NullableEqualityOperator<String> },
    /// Match metadata number sort.
    NumberSort { operator: NumericOperator<f64> },
    /// Match media-analysis status.
    MediaStatus { operator: EqualityOperator<MediaStatus> },
    /// Match media rendering profile.
    MediaProfile { operator: EqualityOperator<MediaProfile> },
    /// Match the requesting user's read status.
    ReadStatus { operator: EqualityOperator<ReadStatus> },
    /// Match metadata authors.
    Author { operator: EqualityOperator<AuthorMatch> },
    /// Match book posters.
    Poster { operator: EqualityOperator<PosterMatch> },
}

impl<'de> Deserialize<'de> for BookCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_condition(deserializer, |field, value| match field {
            "allOf" => from_json(value).map(|conditions| Self::AllOf { conditions }),
            "anyOf" => from_json(value).map(|conditions| Self::AnyOf { conditions }),
            "libraryId" => from_json(value).map(|operator| Self::LibraryId { operator }),
            "seriesId" => from_json(value).map(|operator| Self::SeriesId { operator }),
            "readListId" => from_json(value).map(|operator| Self::ReadListId { operator }),
            "deleted" => from_json(value).map(|operator| Self::Deleted { operator }),
            "oneShot" => from_json(value).map(|operator| Self::OneShot { operator }),
            "title" => from_json(value).map(|operator| Self::Title { operator }),
            "releaseDate" => from_json(value).map(|operator| Self::ReleaseDate { operator }),
            "tag" => from_json(value).map(|operator| Self::Tag { operator }),
            "numberSort" => from_json(value).map(|operator| Self::NumberSort { operator }),
            "mediaStatus" => from_json(value).map(|operator| Self::MediaStatus { operator }),
            "mediaProfile" => from_json(value).map(|operator| Self::MediaProfile { operator }),
            "readStatus" => from_json(value).map(|operator| Self::ReadStatus { operator }),
            "author" => from_json(value).map(|operator| Self::Author { operator }),
            "poster" => from_json(value).map(|operator| Self::Poster { operator }),
            _ => Err(format!("unsupported book condition: {field}")),
        })
    }
}

fn deserialize_condition<'de, D, T, F>(deserializer: D, parse: F) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    F: FnOnce(&str, Value) -> Result<T, String>,
{
    let value = Value::deserialize(deserializer)?;
    let Value::Object(object) = value else {
        return Err(D::Error::custom("search condition must be an object"));
    };
    if object.len() != 1 {
        return Err(D::Error::custom(
            "search condition must contain exactly one supported field",
        ));
    }
    let (field, value) = object.into_iter().next().expect("object contains one entry");
    parse(&field, value).map_err(D::Error::custom)
}

fn from_json<T>(value: Value) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// Request body accepted by the modern Book list endpoint.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
    fn parses_complete_condition_contract() {
        let series: SeriesSearch = serde_json::from_str(
            r#"{"condition":{"allOf":[{"complete":{"operator":"isFalse"}},{"releaseDate":{"operator":"isInTheLast","duration":"P10DT23H"}},{"author":{"operator":"is","value":{}}},{"seriesStatus":{"operator":"is","value":"ONGOING"}}]}}"#,
        )
        .unwrap();
        assert!(matches!(series.condition, Some(SeriesCondition::AllOf { conditions }) if conditions.len() == 4));

        let book: BookSearch = serde_json::from_str(
            r#"{"condition":{"allOf":[{"numberSort":{"operator":"greaterThan","value":1.5}},{"mediaStatus":{"operator":"is","value":"READY"}},{"mediaProfile":{"operator":"isNot","value":"EPUB"}},{"poster":{"operator":"is","value":{"type":"SIDECAR","selected":true}}}]}}"#,
        )
        .unwrap();
        assert!(matches!(book.condition, Some(BookCondition::AllOf { conditions }) if conditions.len() == 4));
    }

    #[test]
    fn relative_duration_uses_whole_utc_days() {
        let search: BookSearch = serde_json::from_str(
            r#"{"condition":{"releaseDate":{"operator":"isInTheLast","duration":"P1DT23H59M59S"}}}"#,
        )
        .unwrap();
        assert!(matches!(
            search.condition,
            Some(BookCondition::ReleaseDate {
                operator: DateOperator::IsInTheLast { days: 1 }
            })
        ));
    }

    #[test]
    fn rejects_calendar_or_invalid_durations_and_wrong_enum_case() {
        for duration in ["P1Y", "P1M", "nope", "P"] {
            let json =
                format!(r#"{{"condition":{{"releaseDate":{{"operator":"isInTheLast","duration":"{duration}"}}}}}}"#,);
            assert!(serde_json::from_str::<BookSearch>(&json).is_err());
        }
        assert!(
            serde_json::from_str::<BookSearch>(r#"{"condition":{"mediaStatus":{"operator":"is","value":"ready"}}}"#,)
                .is_err()
        );
    }

    #[test]
    fn rejects_unsupported_or_ambiguous_search_conditions() {
        assert!(serde_json::from_str::<SeriesSearch>(r#"{"condition":{"poster":{"operator":"isTrue"}}}"#).is_err());
        assert!(
            serde_json::from_str::<BookSearch>(
                r#"{"condition":{"seriesId":{"operator":"is","value":"series"},"poster":{"operator":"isTrue"}}}"#,
            )
            .is_err()
        );
    }
}
