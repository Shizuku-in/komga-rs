//! Request payloads at the Komga REST compatibility boundary.
//!
//! These types intentionally mirror existing camel-case JSON names. Update
//! payloads use [`Patch`] to distinguish an omitted property from an explicit
//! JSON `null`, because those operations have different meanings in Komga.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use komga_core::{
    AlternateTitle, Author, BookMetadataPatch, MetadataUpdate, R2Locator, ReadingDirection, ScanInterval, SeriesCover,
    SeriesMetadataPatch, SeriesStatus, WebLink,
};
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PageDto {
    pub(crate) number: i32,
    pub(crate) file_name: String,
    pub(crate) media_type: String,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) size_bytes: Option<i64>,
    pub(crate) size: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct PositionsDto {
    pub(crate) total: i32,
    pub(crate) positions: Vec<R2Locator>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserCreationDto {
    pub(crate) email: String,
    pub(crate) password: String,
    #[serde(default)]
    pub(crate) roles: BTreeSet<String>,
    pub(crate) age_restriction: Option<AgeRestrictionUpdateDto>,
    pub(crate) labels_allow: Option<BTreeSet<String>>,
    pub(crate) labels_exclude: Option<BTreeSet<String>>,
    pub(crate) shared_libraries: Option<SharedLibrariesUpdateDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasswordUpdateDto {
    pub(crate) password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiKeyRequestDto {
    pub(crate) comment: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgeRestrictionUpdateDto {
    pub(crate) age: i64,
    pub(crate) restriction: RestrictionUpdate,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum RestrictionUpdate {
    AllowOnly,
    Exclude,
    None,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SharedLibrariesUpdateDto {
    #[serde(deserialize_with = "deserialize_boolish")]
    pub(crate) all: bool,
    #[serde(default)]
    pub(crate) library_ids: BTreeSet<String>,
}

fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Boolish {
        Bool(bool),
        String(String),
    }

    match Boolish::deserialize(deserializer)? {
        Boolish::Bool(value) => Ok(value),
        Boolish::String(value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Boolish::String(value) if value.eq_ignore_ascii_case("false") => Ok(false),
        Boolish::String(value) => Err(serde::de::Error::custom(format!("invalid boolean: {value}"))),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserUpdateDto {
    #[serde(default)]
    pub(crate) age_restriction: Patch<AgeRestrictionUpdateDto>,
    #[serde(default)]
    pub(crate) labels_allow: Patch<BTreeSet<String>>,
    #[serde(default)]
    pub(crate) labels_exclude: Patch<BTreeSet<String>>,
    #[serde(default)]
    pub(crate) roles: Patch<BTreeSet<String>>,
    #[serde(default)]
    pub(crate) shared_libraries: Patch<SharedLibrariesUpdateDto>,
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LibraryCreationDto {
    pub(crate) name: String,
    pub(crate) root: String,
    pub(crate) import_comic_info_book: bool,
    pub(crate) import_comic_info_series: bool,
    pub(crate) import_comic_info_collection: bool,
    pub(crate) import_comic_info_read_list: bool,
    pub(crate) import_comic_info_series_append_volume: bool,
    pub(crate) import_epub_book: bool,
    pub(crate) import_epub_series: bool,
    pub(crate) import_mylar_series: bool,
    pub(crate) import_local_artwork: bool,
    pub(crate) import_barcode_isbn: bool,
    pub(crate) scan_force_modified_time: bool,
    pub(crate) scan_interval: ScanInterval,
    pub(crate) scan_on_startup: bool,
    pub(crate) scan_cbx: bool,
    pub(crate) scan_pdf: bool,
    pub(crate) scan_epub: bool,
    pub(crate) scan_directory_exclusions: BTreeSet<String>,
    pub(crate) repair_extensions: bool,
    pub(crate) convert_to_cbz: bool,
    pub(crate) empty_trash_after_scan: bool,
    pub(crate) series_cover: SeriesCover,
    pub(crate) hash_files: bool,
    pub(crate) hash_pages: bool,
    pub(crate) hash_koreader: bool,
    pub(crate) analyze_dimensions: bool,
    pub(crate) oneshots_directory: Option<String>,
}

impl Default for LibraryCreationDto {
    fn default() -> Self {
        Self {
            name: String::new(),
            root: String::new(),
            import_comic_info_book: true,
            import_comic_info_series: true,
            import_comic_info_collection: true,
            import_comic_info_read_list: true,
            import_comic_info_series_append_volume: true,
            import_epub_book: true,
            import_epub_series: true,
            import_mylar_series: true,
            import_local_artwork: true,
            import_barcode_isbn: true,
            scan_force_modified_time: false,
            scan_interval: ScanInterval::Every6h,
            scan_on_startup: false,
            scan_cbx: true,
            scan_pdf: true,
            scan_epub: true,
            scan_directory_exclusions: BTreeSet::new(),
            repair_extensions: false,
            convert_to_cbz: false,
            empty_trash_after_scan: false,
            series_cover: SeriesCover::First,
            hash_files: true,
            hash_pages: false,
            hash_koreader: false,
            analyze_dimensions: true,
            oneshots_directory: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct LibraryUpdateDto {
    pub(crate) name: Patch<String>,
    pub(crate) root: Patch<String>,
    pub(crate) import_comic_info_book: Patch<bool>,
    pub(crate) import_comic_info_series: Patch<bool>,
    pub(crate) import_comic_info_collection: Patch<bool>,
    pub(crate) import_comic_info_read_list: Patch<bool>,
    pub(crate) import_comic_info_series_append_volume: Patch<bool>,
    pub(crate) import_epub_book: Patch<bool>,
    pub(crate) import_epub_series: Patch<bool>,
    pub(crate) import_mylar_series: Patch<bool>,
    pub(crate) import_local_artwork: Patch<bool>,
    pub(crate) import_barcode_isbn: Patch<bool>,
    pub(crate) scan_force_modified_time: Patch<bool>,
    pub(crate) scan_interval: Patch<ScanInterval>,
    pub(crate) scan_on_startup: Patch<bool>,
    pub(crate) scan_cbx: Patch<bool>,
    pub(crate) scan_pdf: Patch<bool>,
    pub(crate) scan_epub: Patch<bool>,
    pub(crate) scan_directory_exclusions: Patch<BTreeSet<String>>,
    pub(crate) repair_extensions: Patch<bool>,
    pub(crate) convert_to_cbz: Patch<bool>,
    pub(crate) empty_trash_after_scan: Patch<bool>,
    pub(crate) series_cover: Patch<SeriesCover>,
    pub(crate) hash_files: Patch<bool>,
    pub(crate) hash_pages: Patch<bool>,
    pub(crate) hash_koreader: Patch<bool>,
    pub(crate) analyze_dimensions: Patch<bool>,
    pub(crate) oneshots_directory: Patch<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientSettingGlobalUpdateDto {
    pub(crate) value: String,
    pub(crate) allow_unauthorized: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClientSettingUserUpdateDto {
    pub(crate) value: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct DirectoryRequestDto {
    pub(crate) path: String,
    pub(crate) show_files: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthorUpdateDto {
    name: String,
    role: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebLinkUpdateDto {
    label: String,
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AlternateTitleUpdateDto {
    label: String,
    title: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct BookMetadataUpdateDto {
    title: Patch<String>,
    title_lock: Patch<bool>,
    summary: Patch<String>,
    summary_lock: Patch<bool>,
    number: Patch<String>,
    number_lock: Patch<bool>,
    number_sort: Patch<f64>,
    number_sort_lock: Patch<bool>,
    release_date: Patch<NaiveDate>,
    release_date_lock: Patch<bool>,
    authors: Patch<Vec<AuthorUpdateDto>>,
    authors_lock: Patch<bool>,
    tags: Patch<BTreeSet<String>>,
    tags_lock: Patch<bool>,
    isbn: Patch<String>,
    isbn_lock: Patch<bool>,
    links: Patch<Vec<WebLinkUpdateDto>>,
    links_lock: Patch<bool>,
}

impl BookMetadataUpdateDto {
    pub(crate) fn into_patch(self) -> BookMetadataPatch {
        BookMetadataPatch {
            title: self.title.preserve_null(),
            title_lock: self.title_lock.preserve_null(),
            summary: self.summary.clear_null(),
            summary_lock: self.summary_lock.preserve_null(),
            number: self.number.preserve_null(),
            number_lock: self.number_lock.preserve_null(),
            number_sort: self.number_sort.preserve_null(),
            number_sort_lock: self.number_sort_lock.preserve_null(),
            release_date: self.release_date.clear_null(),
            release_date_lock: self.release_date_lock.preserve_null(),
            authors: self.authors.clear_null_with(|authors| {
                authors
                    .into_iter()
                    .map(|author| Author {
                        name: author.name,
                        role: author.role,
                    })
                    .collect()
            }),
            authors_lock: self.authors_lock.preserve_null(),
            tags: self.tags.clear_null(),
            tags_lock: self.tags_lock.preserve_null(),
            isbn: self.isbn.clear_null(),
            isbn_lock: self.isbn_lock.preserve_null(),
            links: self.links.clear_null_with(|links| {
                links
                    .into_iter()
                    .map(|link| WebLink {
                        label: link.label,
                        url: link.url,
                    })
                    .collect()
            }),
            links_lock: self.links_lock.preserve_null(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SeriesMetadataUpdateDto {
    status: Patch<SeriesStatus>,
    status_lock: Patch<bool>,
    title: Patch<String>,
    title_lock: Patch<bool>,
    title_sort: Patch<String>,
    title_sort_lock: Patch<bool>,
    summary: Patch<String>,
    summary_lock: Patch<bool>,
    publisher: Patch<String>,
    publisher_lock: Patch<bool>,
    reading_direction: Patch<ReadingDirection>,
    reading_direction_lock: Patch<bool>,
    age_rating: Patch<i32>,
    age_rating_lock: Patch<bool>,
    language: Patch<String>,
    language_lock: Patch<bool>,
    genres: Patch<BTreeSet<String>>,
    genres_lock: Patch<bool>,
    tags: Patch<BTreeSet<String>>,
    tags_lock: Patch<bool>,
    total_book_count: Patch<i32>,
    total_book_count_lock: Patch<bool>,
    sharing_labels: Patch<BTreeSet<String>>,
    sharing_labels_lock: Patch<bool>,
    links: Patch<Vec<WebLinkUpdateDto>>,
    links_lock: Patch<bool>,
    alternate_titles: Patch<Vec<AlternateTitleUpdateDto>>,
    alternate_titles_lock: Patch<bool>,
}

impl SeriesMetadataUpdateDto {
    pub(crate) fn into_patch(self) -> SeriesMetadataPatch {
        SeriesMetadataPatch {
            status: self.status.preserve_null(),
            status_lock: self.status_lock.preserve_null(),
            title: self.title.preserve_null(),
            title_lock: self.title_lock.preserve_null(),
            title_sort: self.title_sort.preserve_null(),
            title_sort_lock: self.title_sort_lock.preserve_null(),
            summary: self.summary.preserve_null(),
            summary_lock: self.summary_lock.preserve_null(),
            reading_direction: self.reading_direction.clear_null(),
            reading_direction_lock: self.reading_direction_lock.preserve_null(),
            publisher: self.publisher.preserve_null(),
            publisher_lock: self.publisher_lock.preserve_null(),
            age_rating: self.age_rating.clear_null(),
            age_rating_lock: self.age_rating_lock.preserve_null(),
            language: self.language.preserve_null(),
            language_lock: self.language_lock.preserve_null(),
            genres: self.genres.clear_null(),
            genres_lock: self.genres_lock.preserve_null(),
            tags: self.tags.clear_null(),
            tags_lock: self.tags_lock.preserve_null(),
            total_book_count: self.total_book_count.clear_null(),
            total_book_count_lock: self.total_book_count_lock.preserve_null(),
            sharing_labels: self.sharing_labels.clear_null(),
            sharing_labels_lock: self.sharing_labels_lock.preserve_null(),
            links: self.links.clear_null_with(|links| {
                links
                    .into_iter()
                    .map(|link| WebLink {
                        label: link.label,
                        url: link.url,
                    })
                    .collect()
            }),
            links_lock: self.links_lock.preserve_null(),
            alternate_titles: self.alternate_titles.clear_null_with(|titles| {
                titles
                    .into_iter()
                    .map(|title| AlternateTitle {
                        label: title.label,
                        title: title.title,
                    })
                    .collect()
            }),
            alternate_titles_lock: self.alternate_titles_lock.preserve_null(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) enum Patch<T> {
    /// The property was absent and existing state must remain unchanged.
    #[default]
    Missing,
    /// The property was present as JSON `null` and should be cleared or rejected.
    Null,
    /// The property was present with a new value.
    Value(T),
}

impl<'de, T> Deserialize<'de> for Patch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

impl<T> Patch<T> {
    fn preserve_null(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Null => None,
        }
    }

    fn clear_null(self) -> MetadataUpdate<T> {
        match self {
            Self::Missing => MetadataUpdate::Keep,
            Self::Null => MetadataUpdate::Clear,
            Self::Value(value) => MetadataUpdate::Set(value),
        }
    }

    fn clear_null_with<U>(self, map: impl FnOnce(T) -> U) -> MetadataUpdate<U> {
        match self {
            Self::Missing => MetadataUpdate::Keep,
            Self::Null => MetadataUpdate::Clear,
            Self::Value(value) => MetadataUpdate::Set(map(value)),
        }
    }
}
