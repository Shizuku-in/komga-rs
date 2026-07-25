use std::collections::HashMap;

use chrono::{NaiveDate, TimeZone, Utc};
use komga_core::{
    AuthorMatch, BookCondition, BookSearch, BooleanOperator, DateOperator, EqualityOperator, MediaStatus,
    NullableEqualityOperator, NullableNumericOperator, PageRequest, ReadStatus, SeriesCondition, SeriesSearch,
    SeriesStatus, SortDirection, SortOrder,
};
use komga_storage::{LegacySeriesRegex, LegacySeriesRegexField};

use crate::error::ApiError;

const MAX_PAGE_SIZE: u64 = 2_000;

const SERIES_SORTS: &[&str] = &[
    "metadata.titleSort",
    "created",
    "createdDate",
    "lastModified",
    "lastModifiedDate",
    "booksMetadata.releaseDate",
    "readDate",
    "name",
    "booksCount",
    "collection.number",
    "random",
];

const BOOK_SORTS: &[&str] = &[
    "name",
    "series",
    "created",
    "createdDate",
    "lastModified",
    "lastModifiedDate",
    "fileSize",
    "size",
    "fileHash",
    "url",
    "media.status",
    "media.comment",
    "media.mediaType",
    "media.pagesCount",
    "metadata.title",
    "metadata.numberSort",
    "metadata.releaseDate",
    "readProgress.lastModified",
    "readProgress.readDate",
    "readList.number",
];

#[derive(Debug, PartialEq)]
pub(crate) struct LegacySeriesQuery {
    pub(crate) search: SeriesSearch,
    pub(crate) page: PageRequest,
    pub(crate) regex: Option<LegacySeriesRegex>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct LegacyBookQuery {
    pub(crate) search: BookSearch,
    pub(crate) page: PageRequest,
}

pub(crate) fn parse_legacy_series_query(raw_query: Option<&str>) -> Result<LegacySeriesQuery, ApiError> {
    let values = RawValues::parse(raw_query);
    let search_term = values.first("search").map(str::to_owned);
    let page = values.page_request(SERIES_SORTS, search_term.as_deref())?;
    let regex = values.first("search_regex").and_then(parse_regex);
    let mut conditions = Vec::new();

    push_any_series(&mut conditions, values.list("library_id"), |value| {
        SeriesCondition::LibraryId { operator: is(value) }
    });
    push_any_series(&mut conditions, values.list("collection_id"), |value| {
        SeriesCondition::CollectionId { operator: is(value) }
    });
    push_any_series(
        &mut conditions,
        parse_list(&values, "status", parse_series_status)?,
        |value| SeriesCondition::SeriesStatus {
            operator: EqualityOperator::Is { value },
        },
    );
    push_any_series(
        &mut conditions,
        parse_list(&values, "read_status", parse_read_status)?,
        |value| SeriesCondition::ReadStatus {
            operator: EqualityOperator::Is { value },
        },
    );
    push_any_series(&mut conditions, values.list("publisher"), |value| {
        SeriesCondition::Publisher { operator: is(value) }
    });
    push_any_series(&mut conditions, values.list("language"), |value| {
        SeriesCondition::Language { operator: is(value) }
    });
    push_any_series(&mut conditions, values.list("genre"), |value| SeriesCondition::Genre {
        operator: NullableEqualityOperator::Is { value },
    });
    push_any_series(&mut conditions, values.list("tag"), |value| SeriesCondition::Tag {
        operator: NullableEqualityOperator::Is { value },
    });
    push_any_series(&mut conditions, values.list("sharing_label"), |value| {
        SeriesCondition::SharingLabel {
            operator: NullableEqualityOperator::Is { value },
        }
    });

    let age_ratings = values
        .list("age_rating")
        .into_iter()
        .map(|value| match value.parse::<i32>() {
            Ok(value) => SeriesCondition::AgeRating {
                operator: NullableNumericOperator::Is { value },
            },
            Err(_) => SeriesCondition::AgeRating {
                operator: NullableNumericOperator::IsNull,
            },
        })
        .collect::<Vec<_>>();
    push_series_conditions(&mut conditions, age_ratings);

    let release_years = values
        .list("release_year")
        .into_iter()
        .filter_map(|value| value.parse::<i32>().ok())
        .filter_map(release_year_condition)
        .collect::<Vec<_>>();
    push_series_conditions(&mut conditions, release_years);

    let authors = values
        .all("author")
        .iter()
        .filter_map(|value| value.rsplit_once(','))
        .map(|(name, role)| SeriesCondition::Author {
            operator: EqualityOperator::Is {
                value: AuthorMatch {
                    name: Some(name.to_owned()),
                    role: Some(role.to_owned()),
                },
            },
        })
        .collect::<Vec<_>>();
    push_series_conditions(&mut conditions, authors);

    if let Some(value) = values.optional_boolean("oneshot")? {
        conditions.push(SeriesCondition::OneShot {
            operator: boolean_operator(value),
        });
    }
    if let Some(value) = values.optional_boolean("complete")? {
        conditions.push(SeriesCondition::Complete {
            operator: boolean_operator(value),
        });
    }
    if let Some(value) = values.optional_boolean("deleted")? {
        conditions.push(SeriesCondition::Deleted {
            operator: boolean_operator(value),
        });
    }

    Ok(LegacySeriesQuery {
        search: SeriesSearch {
            condition: Some(SeriesCondition::AllOf { conditions }),
            full_text_search: search_term,
        },
        page,
        regex,
    })
}

pub(crate) fn parse_legacy_book_query(raw_query: Option<&str>) -> Result<LegacyBookQuery, ApiError> {
    let values = RawValues::parse(raw_query);
    let search_term = values.first("search").map(str::to_owned);
    let page = values.page_request(BOOK_SORTS, search_term.as_deref())?;
    let mut conditions = Vec::new();

    push_any_book(&mut conditions, values.list("library_id"), |value| {
        BookCondition::LibraryId { operator: is(value) }
    });
    push_any_book(
        &mut conditions,
        parse_list(&values, "media_status", parse_media_status)?,
        |value| BookCondition::MediaStatus {
            operator: EqualityOperator::Is { value },
        },
    );
    push_any_book(
        &mut conditions,
        parse_list(&values, "read_status", parse_read_status)?,
        |value| BookCondition::ReadStatus {
            operator: EqualityOperator::Is { value },
        },
    );
    push_any_book(&mut conditions, values.list("tag"), |value| BookCondition::Tag {
        operator: NullableEqualityOperator::Is { value },
    });

    if let Some(value) = values.first("released_after") {
        let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| ApiError::bad_request("released_after must be an ISO date"))?;
        let date_time = date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc()
            .fixed_offset();
        conditions.push(BookCondition::ReleaseDate {
            operator: DateOperator::After { date_time },
        });
    }

    Ok(LegacyBookQuery {
        search: BookSearch {
            condition: Some(BookCondition::AllOf { conditions }),
            full_text_search: search_term,
        },
        page,
    })
}

pub(crate) fn parse_duplicate_books_page(raw_query: Option<&str>) -> Result<PageRequest, ApiError> {
    RawValues::parse(raw_query).page_request_with_fallback(
        BOOK_SORTS,
        Some(SortOrder {
            property: "fileHash".into(),
            direction: SortDirection::Ascending,
        }),
    )
}

fn is<T>(value: T) -> EqualityOperator<T> {
    EqualityOperator::Is { value }
}

fn boolean_operator(value: bool) -> BooleanOperator {
    if value {
        BooleanOperator::IsTrue
    } else {
        BooleanOperator::IsFalse
    }
}

fn push_any_series<T>(conditions: &mut Vec<SeriesCondition>, values: Vec<T>, map: impl FnMut(T) -> SeriesCondition) {
    push_series_conditions(conditions, values.into_iter().map(map).collect());
}

fn push_series_conditions(conditions: &mut Vec<SeriesCondition>, nested: Vec<SeriesCondition>) {
    if !nested.is_empty() {
        conditions.push(SeriesCondition::AnyOf { conditions: nested });
    }
}

fn push_any_book<T>(conditions: &mut Vec<BookCondition>, values: Vec<T>, map: impl FnMut(T) -> BookCondition) {
    let nested = values.into_iter().map(map).collect::<Vec<_>>();
    if !nested.is_empty() {
        conditions.push(BookCondition::AnyOf { conditions: nested });
    }
}

fn release_year_condition(year: i32) -> Option<SeriesCondition> {
    let start = Utc
        .with_ymd_and_hms(year.checked_sub(1)?, 12, 31, 12, 0, 0)
        .single()?
        .fixed_offset();
    let end = Utc
        .with_ymd_and_hms(year.checked_add(1)?, 1, 1, 12, 0, 0)
        .single()?
        .fixed_offset();
    Some(SeriesCondition::AllOf {
        conditions: vec![
            SeriesCondition::ReleaseDate {
                operator: DateOperator::After { date_time: start },
            },
            SeriesCondition::ReleaseDate {
                operator: DateOperator::Before { date_time: end },
            },
        ],
    })
}

fn parse_list<T>(values: &RawValues, key: &str, mut parse: impl FnMut(&str) -> Option<T>) -> Result<Vec<T>, ApiError> {
    values
        .list(key)
        .into_iter()
        .map(|value| parse(&value).ok_or_else(|| ApiError::bad_request(format!("invalid {key} value: {value}"))))
        .collect()
}

fn parse_series_status(value: &str) -> Option<SeriesStatus> {
    match value {
        "ENDED" => Some(SeriesStatus::Ended),
        "ONGOING" => Some(SeriesStatus::Ongoing),
        "ABANDONED" => Some(SeriesStatus::Abandoned),
        "HIATUS" => Some(SeriesStatus::Hiatus),
        _ => None,
    }
}

fn parse_read_status(value: &str) -> Option<ReadStatus> {
    match value {
        "UNREAD" => Some(ReadStatus::Unread),
        "READ" => Some(ReadStatus::Read),
        "IN_PROGRESS" => Some(ReadStatus::InProgress),
        _ => None,
    }
}

fn parse_media_status(value: &str) -> Option<MediaStatus> {
    match value {
        "UNKNOWN" => Some(MediaStatus::Unknown),
        "ERROR" => Some(MediaStatus::Error),
        "READY" => Some(MediaStatus::Ready),
        "UNSUPPORTED" => Some(MediaStatus::Unsupported),
        "OUTDATED" => Some(MediaStatus::Outdated),
        _ => None,
    }
}

fn parse_regex(value: &str) -> Option<LegacySeriesRegex> {
    let (pattern, field) = value.rsplit_once(',')?;
    let field = match field.to_ascii_lowercase().as_str() {
        "title" => LegacySeriesRegexField::Title,
        "title_sort" => LegacySeriesRegexField::TitleSort,
        _ => return None,
    };
    Some(LegacySeriesRegex {
        pattern: pattern.to_owned(),
        field,
    })
}

#[derive(Debug)]
struct RawValues {
    values: HashMap<String, Vec<String>>,
}

impl RawValues {
    fn parse(raw_query: Option<&str>) -> Self {
        let mut values = HashMap::<String, Vec<String>>::new();
        for (key, value) in url::form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes()) {
            values.entry(key.into_owned()).or_default().push(value.into_owned());
        }
        Self { values }
    }

    fn all(&self, key: &str) -> &[String] {
        self.values.get(key).map(Vec::as_slice).unwrap_or_default()
    }

    fn first(&self, key: &str) -> Option<&str> {
        self.all(key).first().map(String::as_str)
    }

    fn list(&self, key: &str) -> Vec<String> {
        let values = self.all(key);
        if values.len() == 1 {
            values[0].split(',').map(str::to_owned).collect()
        } else {
            values.to_vec()
        }
    }

    fn optional_boolean(&self, key: &str) -> Result<Option<bool>, ApiError> {
        self.first(key).map(|value| parse_boolean(value, key)).transpose()
    }

    fn page_request(&self, allowed_sorts: &[&str], search: Option<&str>) -> Result<PageRequest, ApiError> {
        let fallback = search
            .is_some_and(|search| !search.trim().is_empty())
            .then(|| SortOrder {
                property: "relevance".into(),
                direction: SortDirection::Ascending,
            });
        self.page_request_with_fallback(allowed_sorts, fallback)
    }

    fn page_request_with_fallback(
        &self,
        allowed_sorts: &[&str],
        fallback: Option<SortOrder>,
    ) -> Result<PageRequest, ApiError> {
        let page = self.first("page").and_then(|value| value.parse().ok()).unwrap_or(0);
        let size = self
            .first("size")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|size| *size > 0)
            .unwrap_or(20)
            .min(MAX_PAGE_SIZE);
        let unpaged = self.optional_boolean("unpaged")?.unwrap_or(false);
        let mut sort = Vec::new();
        let mut syntactic_sort = false;
        for value in self.all("sort") {
            let Some(order) = parse_sort(value) else {
                continue;
            };
            syntactic_sort = true;
            let fallback_property = fallback.as_ref().map(|value| value.property.as_str());
            if allowed_sorts.contains(&order.property.as_str()) || fallback_property == Some(order.property.as_str()) {
                sort.push(order);
            }
        }
        if !syntactic_sort && let Some(fallback) = fallback {
            sort.push(fallback);
        }
        Ok(PageRequest {
            page,
            size,
            unpaged,
            sort,
        })
    }
}

fn parse_boolean(value: &str, field: &str) -> Result<bool, ApiError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(ApiError::bad_request(format!("invalid boolean value for {field}"))),
    }
}

fn parse_sort(value: &str) -> Option<SortOrder> {
    let mut segments = value.split(',');
    let property = segments.next()?.trim();
    if property.is_empty() {
        return None;
    }
    let direction = match segments.next().unwrap_or("asc").trim().to_ascii_lowercase().as_str() {
        "asc" => SortDirection::Ascending,
        "desc" => SortDirection::Descending,
        _ => return None,
    };
    if segments.next().is_some() {
        return None;
    }
    Some(SortOrder {
        property: property.to_owned(),
        direction,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_parser_translates_spring_lists_authors_booleans_and_years() {
        let parsed = parse_legacy_series_query(Some(
            "library_id=one,two&tag=first&tag=second&author=Doe%2C+Jane%2Cwriter&author=ignored&age_rating=12,none&release_year=2020,nope&deleted=YES&complete=0&oneshot=on",
        ))
        .unwrap();

        let Some(SeriesCondition::AllOf { conditions }) = parsed.search.condition else {
            panic!("expected all-of condition");
        };
        assert_eq!(conditions.len(), 8);
        let SeriesCondition::AnyOf { conditions: libraries } = &conditions[0] else {
            panic!("expected libraries");
        };
        assert_eq!(libraries.len(), 2);
        let SeriesCondition::AnyOf { conditions: tags } = &conditions[1] else {
            panic!("expected tags");
        };
        assert_eq!(tags.len(), 2);
        assert!(conditions.iter().any(|condition| matches!(
            condition,
            SeriesCondition::AnyOf { conditions }
                if matches!(&conditions[0], SeriesCondition::Author {
                    operator: EqualityOperator::Is { value: AuthorMatch { name: Some(name), role: Some(role) } }
                } if name == "Doe, Jane" && role == "writer")
        )));
    }

    #[test]
    fn legacy_page_falls_back_caps_and_only_reports_applied_sorts() {
        let query = parse_legacy_series_query(Some(
            "search=alpha&page=-2&size=9999&sort=unknown,desc&sort=metadata.titleSort,desc",
        ))
        .unwrap();
        assert_eq!(query.page.page, 0);
        assert_eq!(query.page.size, 2_000);
        assert_eq!(query.page.sort.len(), 1);
        assert_eq!(query.page.sort[0].property, "metadata.titleSort");
        assert_eq!(query.page.sort[0].direction, SortDirection::Descending);

        let fallback = parse_legacy_book_query(Some("page=bad&size=0")).unwrap();
        assert_eq!(fallback.page.page, 0);
        assert_eq!(fallback.page.size, 20);
    }

    #[test]
    fn search_defaults_to_relevance_only_without_a_syntactic_sort() {
        let relevant = parse_legacy_book_query(Some("search=alpha&sort=bad,direction")).unwrap();
        assert_eq!(relevant.page.sort[0].property, "relevance");

        let unknown = parse_legacy_book_query(Some("search=alpha&sort=unknown,asc")).unwrap();
        assert!(unknown.page.sort.is_empty());

        let explicit = parse_legacy_series_query(Some("search=alpha&sort=relevance,desc")).unwrap();
        assert_eq!(explicit.page.sort[0].property, "relevance");
        assert_eq!(explicit.page.sort[0].direction, SortDirection::Descending);

        let irrelevant = parse_legacy_series_query(Some("sort=relevance,asc")).unwrap();
        assert!(irrelevant.page.sort.is_empty());
    }

    #[test]
    fn duplicate_page_defaults_file_hash_and_preserves_syntactic_unknown_sort() {
        let default = parse_duplicate_books_page(Some("page=bad&size=0&unpaged=yes")).unwrap();
        assert_eq!(default.page, 0);
        assert_eq!(default.size, 20);
        assert!(default.unpaged);
        assert_eq!(default.sort[0].property, "fileHash");

        let unknown = parse_duplicate_books_page(Some("sort=unknown,desc")).unwrap();
        assert!(unknown.sort.is_empty());

        let malformed = parse_duplicate_books_page(Some("sort=name,sideways")).unwrap();
        assert_eq!(malformed.sort[0].property, "fileHash");
        assert!(parse_duplicate_books_page(Some("unpaged=maybe")).is_err());
    }

    #[test]
    fn invalid_typed_values_fail_while_unknown_and_invalid_years_are_ignored() {
        assert!(parse_legacy_series_query(Some("status=INVALID")).is_err());
        assert!(parse_legacy_series_query(Some("read_status=read")).is_err());
        assert!(parse_legacy_series_query(Some("deleted=maybe")).is_err());
        assert!(parse_legacy_book_query(Some("media_status=INVALID")).is_err());
        assert!(parse_legacy_book_query(Some("released_after=2024-02-30")).is_err());

        let parsed = parse_legacy_series_query(Some("unknown=value&release_year=nope&author=malformed")).unwrap();
        assert!(matches!(
            parsed.search.condition,
            Some(SeriesCondition::AllOf { conditions }) if conditions.is_empty()
        ));
    }

    #[test]
    fn regex_uses_the_first_parameter_and_splits_on_the_last_comma() {
        let parsed = parse_legacy_series_query(Some(
            "search_regex=alpha%2Cbeta%2CTITLE_SORT&search_regex=ignored%2Ctitle",
        ))
        .unwrap();
        assert_eq!(
            parsed.regex,
            Some(LegacySeriesRegex {
                pattern: "alpha,beta".into(),
                field: LegacySeriesRegexField::TitleSort,
            })
        );

        assert!(
            parse_legacy_series_query(Some("search_regex=malformed"))
                .unwrap()
                .regex
                .is_none()
        );
        assert!(
            parse_legacy_series_query(Some("search_regex=alpha,unknown"))
                .unwrap()
                .regex
                .is_none()
        );
    }
}
