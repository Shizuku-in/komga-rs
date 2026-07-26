use chrono::{DateTime, NaiveDateTime, Utc};
use komga_core::{SyncBookChange, SyncBookProjection, SyncPointReadList, SyncReadProgress};
use serde::{Deserialize, Serialize};

const DUMMY_ID: &str = "00000000-0000-0000-0000-000000000001";

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct AuthDto {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub tracking_id: String,
    pub user_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ResourcesDto {
    pub resources: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct AmountDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency_code: Option<&'static str>,
    pub total_amount: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ContributorDto {
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PublisherDto {
    pub imprint: &'static str,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct KoboSeriesDto {
    pub id: String,
    pub name: String,
    pub number: String,
    pub number_float: f64,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) enum FormatDto {
    #[serde(rename = "EPUB3FL")]
    Epub3Fl,
    #[serde(rename = "EPUB3")]
    Epub3,
    #[serde(rename = "KEPUB")]
    Kepub,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct DownloadUrlDto {
    pub drm_type: &'static str,
    pub format: FormatDto,
    pub size: i64,
    pub platform: &'static str,
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct KoboBookMetadataDto {
    pub categories: Vec<&'static str>,
    pub contributor_roles: Vec<ContributorDto>,
    pub contributors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_image_id: Option<String>,
    pub cross_revision_id: String,
    pub current_display_price: AmountDto,
    pub current_love_display_price: AmountDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub download_urls: Vec<DownloadUrlDto>,
    pub entitlement_id: String,
    pub external_ids: Vec<String>,
    pub genre: &'static str,
    pub is_eligible_for_kobo_love: bool,
    pub is_internet_archive: bool,
    pub is_pre_order: bool,
    pub is_social_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isbn: Option<String>,
    pub language: String,
    pub phonetic_pronunciations: std::collections::BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_date: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<PublisherDto>,
    pub revision_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<KoboSeriesDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_title: Option<String>,
    pub title: String,
    pub work_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct PeriodDto {
    pub from: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct BookEntitlementDto {
    pub accessibility: &'static str,
    pub active_period: PeriodDto,
    pub created: DateTime<Utc>,
    pub cross_revision_id: String,
    pub id: String,
    pub is_hidden_from_archive: bool,
    pub is_locked: bool,
    pub is_removed: bool,
    pub last_modified: DateTime<Utc>,
    pub origin_category: &'static str,
    pub revision_id: String,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct BookEntitlementContainerDto {
    pub book_entitlement: BookEntitlementDto,
    pub book_metadata: KoboBookMetadataDto,
    pub reading_state: Option<ReadingStateDto>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct LocationDto {
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default = "kobo_span_type")]
    pub r#type: Option<String>,
    pub source: String,
}

fn kobo_span_type() -> Option<String> {
    Some("KoboSpan".into())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct BookmarkDto {
    pub last_modified: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_source_progress_percent: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<LocationDto>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct StatisticsDto {
    pub last_modified: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_time_minutes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spent_reading_minutes: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum StatusDto {
    #[serde(rename = "ReadyToRead")]
    ReadyToRead,
    #[serde(rename = "Finished")]
    Finished,
    #[serde(rename = "Reading")]
    Reading,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct StatusInfoDto {
    pub last_modified: DateTime<Utc>,
    pub status: StatusDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub times_started_reading: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_time_finished: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_time_started_reading: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ReadingStateDto {
    #[serde(default)]
    pub created: Option<DateTime<Utc>>,
    pub current_bookmark: BookmarkDto,
    pub entitlement_id: String,
    pub last_modified: DateTime<Utc>,
    #[serde(default)]
    pub priority_timestamp: Option<DateTime<Utc>>,
    pub statistics: StatisticsDto,
    pub status_info: StatusInfoDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct WrappedReadingStateDto {
    pub reading_state: ReadingStateDto,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ReadingStateUpdateDto {
    #[serde(default)]
    pub reading_states: Vec<ReadingStateDto>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) enum ResultDto {
    #[serde(rename = "Success")]
    Success,
    #[serde(rename = "Failure")]
    Failure,
    #[serde(rename = "Ignored")]
    Ignored,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct WrappedResultDto {
    pub result: ResultDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ReadingStateUpdateResultDto {
    pub entitlement_id: String,
    pub current_bookmark_result: WrappedResultDto,
    pub statistics_result: WrappedResultDto,
    pub status_info_result: WrappedResultDto,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct RequestResultDto {
    pub request_result: ResultDto,
    pub update_results: Vec<ReadingStateUpdateResultDto>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct TagItemDto {
    pub revision_id: String,
    pub r#type: &'static str,
}

#[derive(Debug, Serialize)]
pub(super) enum TagTypeDto {
    #[serde(rename = "UserTag")]
    UserTag,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct TagDto {
    pub id: String,
    pub created: DateTime<Utc>,
    pub last_modified: DateTime<Utc>,
    pub name: String,
    pub r#type: TagTypeDto,
    pub items: Option<Vec<TagItemDto>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct WrappedTagDto {
    pub tag: TagDto,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum SyncResultDto {
    NewEntitlement {
        #[serde(rename = "NewEntitlement")]
        value: BookEntitlementContainerDto,
    },
    ChangedEntitlement {
        #[serde(rename = "ChangedEntitlement")]
        value: BookEntitlementContainerDto,
    },
    ChangedProductMetadata {
        #[serde(rename = "ChangedProductMetadata")]
        value: KoboBookMetadataDto,
    },
    ChangedReadingState {
        #[serde(rename = "ChangedReadingState")]
        value: WrappedReadingStateDto,
    },
    NewTag {
        #[serde(rename = "NewTag")]
        value: WrappedTagDto,
    },
    ChangedTag {
        #[serde(rename = "ChangedTag")]
        value: WrappedTagDto,
    },
    DeletedTag {
        #[serde(rename = "DeletedTag")]
        value: WrappedTagDto,
    },
}

pub(super) fn entitlement(change: &SyncBookChange, removed: bool) -> BookEntitlementDto {
    let id = change.snapshot.book_id.clone();
    BookEntitlementDto {
        accessibility: "Full",
        active_period: PeriodDto { from: Utc::now() },
        created: utc(change.snapshot.created_date),
        cross_revision_id: id.clone(),
        id: id.clone(),
        is_hidden_from_archive: false,
        is_locked: false,
        is_removed: removed,
        last_modified: utc(change.snapshot.file_last_modified),
        origin_category: "Imported",
        revision_id: id,
        status: "Active",
    }
}

pub(super) fn metadata(
    projection: &SyncBookProjection,
    cover_image_id: Option<String>,
    file_size: i64,
    download_url: String,
    kepub_conversion_available: bool,
) -> KoboBookMetadataDto {
    let publication = projection
        .release_date
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .unwrap_or(projection.metadata_created_date);
    let format = if projection.epub_is_fixed_layout {
        FormatDto::Epub3Fl
    } else if projection.epub_is_kepub || kepub_conversion_available {
        FormatDto::Kepub
    } else {
        FormatDto::Epub3
    };
    let language = projection.language.chars().take(2).collect::<String>();
    let language = if language.trim().is_empty() {
        "en".to_owned()
    } else {
        language
    };
    KoboBookMetadataDto {
        categories: vec![DUMMY_ID],
        contributor_roles: projection
            .authors
            .iter()
            .cloned()
            .map(|name| ContributorDto { name })
            .collect(),
        contributors: projection.authors.clone(),
        cover_image_id,
        cross_revision_id: projection.book_id.clone(),
        current_display_price: AmountDto {
            currency_code: Some("USD"),
            total_amount: 0,
        },
        current_love_display_price: AmountDto {
            currency_code: None,
            total_amount: 0,
        },
        description: Some(if projection.summary.is_empty() {
            " ".into()
        } else {
            projection.summary.clone()
        }),
        download_urls: vec![DownloadUrlDto {
            drm_type: "None",
            format,
            size: file_size,
            platform: "Generic",
            url: download_url,
        }],
        entitlement_id: projection.book_id.clone(),
        external_ids: Vec::new(),
        genre: DUMMY_ID,
        is_eligible_for_kobo_love: false,
        is_internet_archive: false,
        is_pre_order: false,
        is_social_enabled: true,
        isbn: projection.isbn.clone(),
        language,
        phonetic_pronunciations: std::collections::BTreeMap::new(),
        publication_date: Some(utc(publication)),
        publisher: Some(PublisherDto {
            imprint: "",
            name: projection.publisher.clone(),
        }),
        revision_id: projection.book_id.clone(),
        series: (!projection.oneshot).then(|| KoboSeriesDto {
            id: projection.series_id.clone(),
            name: projection.series_title.clone(),
            number: projection.number.clone(),
            number_float: projection.number_sort,
        }),
        slug: None,
        sub_title: None,
        title: projection.title.clone(),
        work_id: projection.book_id.clone(),
    }
}

pub(super) fn should_convert_kepub(projection: &SyncBookProjection, kepub_conversion_available: bool) -> bool {
    !projection.epub_is_fixed_layout && !projection.epub_is_kepub && kepub_conversion_available
}

pub(super) fn removed_metadata(book_id: &str) -> KoboBookMetadataDto {
    KoboBookMetadataDto {
        categories: vec![DUMMY_ID],
        contributor_roles: Vec::new(),
        contributors: Vec::new(),
        cover_image_id: Some(book_id.to_owned()),
        cross_revision_id: book_id.to_owned(),
        current_display_price: AmountDto {
            currency_code: Some("USD"),
            total_amount: 0,
        },
        current_love_display_price: AmountDto {
            currency_code: None,
            total_amount: 0,
        },
        description: None,
        download_urls: Vec::new(),
        entitlement_id: book_id.to_owned(),
        external_ids: Vec::new(),
        genre: DUMMY_ID,
        is_eligible_for_kobo_love: false,
        is_internet_archive: false,
        is_pre_order: false,
        is_social_enabled: true,
        isbn: None,
        language: "en".into(),
        phonetic_pronunciations: std::collections::BTreeMap::new(),
        publication_date: None,
        publisher: None,
        revision_id: book_id.to_owned(),
        series: None,
        slug: None,
        sub_title: None,
        title: book_id.to_owned(),
        work_id: book_id.to_owned(),
    }
}

pub(super) fn reading_state(
    progress: Option<&SyncReadProgress>,
    book_id: &str,
    book_created: NaiveDateTime,
) -> ReadingStateDto {
    let Some(progress) = progress else {
        let created = utc(book_created);
        return ReadingStateDto {
            created: Some(created),
            current_bookmark: BookmarkDto {
                last_modified: created,
                progress_percent: None,
                content_source_progress_percent: None,
                location: None,
            },
            entitlement_id: book_id.to_owned(),
            last_modified: created,
            priority_timestamp: Some(created),
            statistics: StatisticsDto {
                last_modified: created,
                remaining_time_minutes: None,
                spent_reading_minutes: None,
            },
            status_info: StatusInfoDto {
                last_modified: created,
                status: StatusDto::ReadyToRead,
                times_started_reading: Some(0),
                last_time_finished: None,
                last_time_started_reading: None,
            },
        };
    };
    let created = utc(progress.created_date);
    let modified = utc(progress.last_modified_date);
    let locator = progress.progress.locator.as_ref();
    ReadingStateDto {
        created: Some(created),
        current_bookmark: BookmarkDto {
            last_modified: modified,
            progress_percent: locator
                .and_then(|value| value.locations.as_ref())
                .and_then(|value| value.total_progression)
                .map(|value| value * 100.0),
            content_source_progress_percent: locator
                .and_then(|value| value.locations.as_ref())
                .and_then(|value| value.progression)
                .map(|value| value * 100.0),
            location: locator.map(|value| LocationDto {
                value: value.kobo_span.clone(),
                r#type: Some("KoboSpan".into()),
                source: value.href.clone(),
            }),
        },
        entitlement_id: book_id.to_owned(),
        last_modified: modified,
        priority_timestamp: Some(modified),
        statistics: StatisticsDto {
            last_modified: modified,
            remaining_time_minutes: None,
            spent_reading_minutes: None,
        },
        status_info: StatusInfoDto {
            last_modified: modified,
            status: if progress.progress.completed {
                StatusDto::Finished
            } else {
                StatusDto::Reading
            },
            times_started_reading: Some(1),
            last_time_finished: None,
            last_time_started_reading: None,
        },
    }
}

pub(super) fn wrapped_tag(read_list: &SyncPointReadList, book_ids: Option<&[String]>) -> WrappedTagDto {
    WrappedTagDto {
        tag: TagDto {
            id: read_list.read_list_id.clone(),
            created: utc(read_list.created_date),
            last_modified: utc(read_list.last_modified_date),
            name: read_list.read_list_name.clone(),
            r#type: TagTypeDto::UserTag,
            items: book_ids.map(|ids| {
                ids.iter()
                    .cloned()
                    .map(|revision_id| TagItemDto {
                        revision_id,
                        r#type: "ProductRevisionTagItem",
                    })
                    .collect()
            }),
        },
    }
}

fn utc(value: NaiveDateTime) -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(value, Utc)
}
