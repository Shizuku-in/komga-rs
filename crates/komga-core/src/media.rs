//! Persisted media models and format classification.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

/// Canonical ZIP media type stored by Komga.
pub const ZIP_MEDIA_TYPE: &str = "application/zip";
/// Canonical PDF media type stored by Komga.
pub const PDF_MEDIA_TYPE: &str = "application/pdf";
/// Canonical EPUB media type stored by Komga.
pub const EPUB_MEDIA_TYPE: &str = "application/epub+zip";
/// Generic RAR media type used by older detectors.
pub const RAR_MEDIA_TYPE: &str = "application/x-rar-compressed";
/// RAR 1.5 through 4.x media type stored by the Rust analyzer.
pub const RAR4_MEDIA_TYPE: &str = "application/x-rar-compressed; version=4";
/// RAR 5.x media type stored by the Rust analyzer.
pub const RAR5_MEDIA_TYPE: &str = "application/x-rar-compressed; version=5";
/// JPEG media type produced by PDF rendering and generated thumbnails.
pub const JPEG_MEDIA_TYPE: &str = "image/jpeg";
/// Kotlin class name persisted for [`MediaExtensionEpub`].
pub const EPUB_EXTENSION_CLASS: &str = "org.gotson.komga.domain.model.MediaExtensionEpub";
/// Kotlin subtype used for EPUB spine resources.
pub const EPUB_PAGE_SUBTYPE: &str = "EPUB_PAGE";
/// Kotlin subtype used for non-spine EPUB resources.
pub const EPUB_ASSET_SUBTYPE: &str = "EPUB_ASSET";

/// Rendering profile selected from a detected media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaProfile {
    /// Image pages stored in ZIP or RAR containers.
    Divina,
    /// Portable Document Format pages rendered on demand.
    Pdf,
    /// Electronic Publication resources and reading positions.
    Epub,
}

impl MediaProfile {
    /// Classifies a persisted media type using Komga's profile mapping.
    pub fn from_media_type(media_type: &str) -> Option<Self> {
        match media_type {
            ZIP_MEDIA_TYPE | RAR_MEDIA_TYPE | RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE | "application/vnd.rar" => {
                Some(Self::Divina)
            }
            PDF_MEDIA_TYPE => Some(Self::Pdf),
            EPUB_MEDIA_TYPE => Some(Self::Epub),
            _ => None,
        }
    }

    /// Returns the uppercase identifier exposed by Komga's REST API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Divina => "DIVINA",
            Self::Pdf => "PDF",
            Self::Epub => "EPUB",
        }
    }
}

/// Width and height in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimension {
    /// Horizontal pixel count.
    pub width: i32,
    /// Vertical pixel count.
    pub height: i32,
}

/// One naturally ordered image page found in a book container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookPage {
    /// Exact entry name inside the container.
    pub file_name: String,
    /// Detected page media type.
    pub media_type: String,
    /// Optional dimensions controlled by the library analysis setting.
    pub dimension: Option<Dimension>,
    /// Page hash, empty until page hashing is ported.
    pub file_hash: String,
    /// Uncompressed entry size when available.
    pub file_size: Option<i64>,
}

/// A non-page file found in a book container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    /// Exact entry name inside the container.
    pub file_name: String,
    /// Detected media type, if detection succeeded.
    pub media_type: Option<String>,
    /// Kotlin EPUB subtype; absent for CBZ files.
    pub sub_type: Option<String>,
    /// Uncompressed entry size when available.
    pub file_size: Option<i64>,
}

/// Lifecycle state of analyzed media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaStatus {
    /// The book has never been analyzed.
    Unknown,
    /// Analysis failed.
    Error,
    /// Analysis completed and pages can be served.
    Ready,
    /// The detected container is not implemented.
    Unsupported,
    /// The source file changed after its previous analysis.
    Outdated,
}

impl MediaStatus {
    /// Returns the identifier stored by Komga's schema.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Error => "ERROR",
            Self::Ready => "READY",
            Self::Unsupported => "UNSUPPORTED",
            Self::Outdated => "OUTDATED",
        }
    }
}

impl TryFrom<&str> for MediaStatus {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, String> {
        match value {
            "UNKNOWN" => Ok(Self::Unknown),
            "ERROR" => Ok(Self::Error),
            "READY" => Ok(Self::Ready),
            "UNSUPPORTED" => Ok(Self::Unsupported),
            "OUTDATED" => Ok(Self::Outdated),
            _ => Err(format!("invalid media status {value:?}")),
        }
    }
}

/// A nested EPUB navigation entry used by the table of contents and landmarks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpubTocEntry {
    /// Human-readable entry title.
    pub title: String,
    /// Resource link, if the entry points to content.
    pub href: Option<String>,
    /// Nested navigation entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<EpubTocEntry>,
}

/// Alternative location expressions inside an EPUB resource.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct R2Location {
    /// Fragment identifiers inside the resource.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fragments: Vec<String>,
    /// Progression inside the resource from zero to one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progression: Option<f32>,
    /// One-based position in the publication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i32>,
    /// Progression inside the complete publication from zero to one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_progression: Option<f32>,
}

/// Text context attached to a Readium locator.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct R2Text {
    /// Text following the location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Text preceding the location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// Text highlighted at the location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight: Option<String>,
}

/// Readium locator persisted for EPUB reading positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct R2Locator {
    /// Resource URI without a fragment.
    pub href: String,
    /// Resource media type.
    #[serde(rename = "type")]
    pub media_type: String,
    /// Optional chapter or section title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Alternative location expressions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locations: Option<R2Location>,
    /// Text context around the location.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<R2Text>,
    /// Komga-specific Kobo span identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kobo_span: Option<String>,
}

/// EPUB-specific analyzed data stored as gzip-compressed JSON by Kotlin.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaExtensionEpub {
    /// Publication table of contents.
    #[serde(default)]
    pub toc: Vec<EpubTocEntry>,
    /// Publication landmarks.
    #[serde(default)]
    pub landmarks: Vec<EpubTocEntry>,
    /// Publication page list.
    #[serde(default)]
    pub page_list: Vec<EpubTocEntry>,
    /// Whether the publication declares or behaves as fixed layout.
    #[serde(default)]
    pub is_fixed_layout: bool,
    /// Computed Readium reading positions.
    #[serde(default)]
    pub positions: Vec<R2Locator>,
}

/// Format-specific analyzed data attached to a media record.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaExtension {
    /// EPUB navigation, layout, and reading-position data.
    Epub(MediaExtensionEpub),
}

impl MediaExtension {
    /// Returns the Kotlin class name stored beside the serialized extension.
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Epub(_) => EPUB_EXTENSION_CLASS,
        }
    }
}

/// Complete analyzed-media record for one book.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// Analysis state.
    pub status: MediaStatus,
    /// Detected container media type.
    pub media_type: Option<String>,
    /// Naturally ordered image pages.
    pub pages: Vec<BookPage>,
    /// Stored page count, which may differ from image pages for EPUB documents.
    pub page_count: i32,
    /// Non-page files found in the container.
    pub files: Vec<MediaFile>,
    /// Kotlin-compatible analysis error or warning code.
    pub comment: Option<String>,
    /// Format-specific analyzed data.
    pub extension: Option<MediaExtension>,
    /// Owning book identifier.
    pub book_id: String,
    /// Whether an EPUB can be served with the Divina profile.
    pub epub_divina_compatible: bool,
    /// Whether an EPUB is already KEPUB-formatted.
    pub epub_is_kepub: bool,
    /// Persisted creation timestamp when loaded from storage.
    pub created_date: Option<NaiveDateTime>,
    /// Persisted modification timestamp when loaded from storage.
    pub last_modified_date: Option<NaiveDateTime>,
}

impl Media {
    /// Creates a media result without pages, files, or a format extension.
    pub fn result(book_id: &str, status: MediaStatus, media_type: Option<String>, comment: Option<&str>) -> Self {
        Self {
            status,
            media_type,
            pages: Vec::new(),
            page_count: 0,
            files: Vec::new(),
            comment: comment.map(str::to_owned),
            extension: None,
            book_id: book_id.to_owned(),
            epub_divina_compatible: false,
            epub_is_kepub: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    /// Returns the rendering profile inferred from the persisted media type.
    pub fn profile(&self) -> Option<MediaProfile> {
        self.media_type.as_deref().and_then(MediaProfile::from_media_type)
    }
}

/// Generated or selected book poster stored in `THUMBNAIL_BOOK`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailBook {
    /// Stable thumbnail identifier.
    pub id: String,
    /// Inline thumbnail bytes, if database-backed.
    pub thumbnail: Option<Vec<u8>>,
    /// Sidecar file URL, if filesystem-backed.
    pub url: Option<String>,
    /// Whether clients should display this thumbnail.
    pub selected: bool,
    /// Kotlin thumbnail type such as `GENERATED`.
    pub thumbnail_type: String,
    /// Encoded image media type.
    pub media_type: String,
    /// Encoded byte length.
    pub file_size: i64,
    /// Encoded image dimensions.
    pub dimension: Dimension,
    /// Owning book identifier.
    pub book_id: String,
}

impl ThumbnailBook {
    /// Converts this thumbnail to the public REST representation.
    pub fn to_dto(&self) -> ThumbnailBookDto {
        ThumbnailBookDto {
            id: self.id.clone(),
            book_id: self.book_id.clone(),
            thumbnail_type: self.thumbnail_type.clone(),
            selected: self.selected,
            media_type: self.media_type.clone(),
            file_size: self.file_size,
            width: self.dimension.width,
            height: self.dimension.height,
        }
    }
}

/// Series poster stored in `THUMBNAIL_SERIES`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailSeries {
    /// Stable thumbnail identifier.
    pub id: String,
    /// Inline thumbnail bytes, if database-backed.
    pub thumbnail: Option<Vec<u8>>,
    /// Sidecar file URL, if filesystem-backed.
    pub url: Option<String>,
    /// Whether clients should display this thumbnail.
    pub selected: bool,
    /// Kotlin thumbnail type such as `USER_UPLOADED`.
    pub thumbnail_type: String,
    /// Encoded image media type.
    pub media_type: String,
    /// Encoded byte length.
    pub file_size: i64,
    /// Encoded image dimensions.
    pub dimension: Dimension,
    /// Owning series identifier.
    pub series_id: String,
}

impl ThumbnailSeries {
    /// Converts this thumbnail to the public REST representation.
    pub fn to_dto(&self) -> ThumbnailSeriesDto {
        ThumbnailSeriesDto {
            id: self.id.clone(),
            series_id: self.series_id.clone(),
            thumbnail_type: self.thumbnail_type.clone(),
            selected: self.selected,
            media_type: self.media_type.clone(),
            file_size: self.file_size,
            width: self.dimension.width,
            height: self.dimension.height,
        }
    }
}

/// Book thumbnail metadata returned by the REST API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailBookDto {
    /// Stable thumbnail identifier.
    pub id: String,
    /// Owning book identifier.
    pub book_id: String,
    /// Kotlin thumbnail type.
    #[serde(rename = "type")]
    pub thumbnail_type: String,
    /// Whether this thumbnail is selected.
    pub selected: bool,
    /// Encoded image media type.
    pub media_type: String,
    /// Encoded byte length.
    pub file_size: i64,
    /// Encoded width.
    pub width: i32,
    /// Encoded height.
    pub height: i32,
}

/// Series thumbnail metadata returned by the REST API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailSeriesDto {
    /// Stable thumbnail identifier.
    pub id: String,
    /// Owning series identifier.
    pub series_id: String,
    /// Kotlin thumbnail type.
    #[serde(rename = "type")]
    pub thumbnail_type: String,
    /// Whether this thumbnail is selected.
    pub selected: bool,
    /// Encoded image media type.
    pub media_type: String,
    /// Encoded byte length.
    pub file_size: i64,
    /// Encoded width.
    pub width: i32,
    /// Encoded height.
    pub height: i32,
}

/// Uploaded collection poster metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailCollectionDto {
    /// Stable identifier.
    pub id: String,
    /// Owning collection.
    pub collection_id: String,
    /// Kotlin thumbnail type.
    #[serde(rename = "type")]
    pub thumbnail_type: String,
    /// Whether selected.
    pub selected: bool,
    /// Encoded media type.
    pub media_type: String,
    /// Encoded size.
    pub file_size: i64,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

/// Uploaded read-list poster metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailReadListDto {
    /// Stable identifier.
    pub id: String,
    /// Owning read list.
    pub read_list_id: String,
    /// Kotlin thumbnail type.
    #[serde(rename = "type")]
    pub thumbnail_type: String,
    /// Whether selected.
    pub selected: bool,
    /// Encoded media type.
    pub media_type: String,
    /// Encoded size.
    pub file_size: i64,
    /// Width in pixels.
    pub width: i32,
    /// Height in pixels.
    pub height: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_profiles_cover_kotlin_container_types() {
        for (media_type, expected) in [
            (ZIP_MEDIA_TYPE, MediaProfile::Divina),
            (RAR_MEDIA_TYPE, MediaProfile::Divina),
            (RAR4_MEDIA_TYPE, MediaProfile::Divina),
            (RAR5_MEDIA_TYPE, MediaProfile::Divina),
            ("application/vnd.rar", MediaProfile::Divina),
            (PDF_MEDIA_TYPE, MediaProfile::Pdf),
            (EPUB_MEDIA_TYPE, MediaProfile::Epub),
        ] {
            assert_eq!(MediaProfile::from_media_type(media_type), Some(expected));
        }
        assert_eq!(MediaProfile::from_media_type("application/octet-stream"), None);
    }

    #[test]
    fn epub_extension_uses_kotlin_json_field_names() {
        let extension = MediaExtensionEpub {
            is_fixed_layout: true,
            positions: vec![R2Locator {
                href: "chapter.xhtml".into(),
                media_type: "application/xhtml+xml".into(),
                title: None,
                locations: Some(R2Location {
                    position: Some(1),
                    ..Default::default()
                }),
                text: None,
                kobo_span: Some("kobo.1.1".into()),
            }],
            ..Default::default()
        };
        let json = serde_json::to_value(&extension).unwrap();
        assert_eq!(json["isFixedLayout"], true);
        assert_eq!(json["positions"][0]["type"], "application/xhtml+xml");
        assert_eq!(json["positions"][0]["koboSpan"], "kobo.1.1");
    }
}
