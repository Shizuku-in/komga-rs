//! Request payloads at the Komga REST compatibility boundary.
//!
//! These types intentionally mirror existing camel-case JSON names. Update
//! payloads use [`Patch`] to distinguish an omitted property from an explicit
//! JSON `null`, because those operations have different meanings in Komga.

use std::collections::BTreeSet;

use komga_core::{R2Locator, ScanInterval, SeriesCover};
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
