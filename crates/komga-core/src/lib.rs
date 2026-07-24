//! Framework-independent domain types and authorization policy for Komga.
//!
//! This crate contains the behavior shared by the HTTP and persistence layers.
//! It intentionally has no dependency on Axum or SQLx. Types that cross the
//! existing Komga REST boundary preserve the Kotlin service's serialized names.

#![warn(missing_docs)]

mod content;
mod media;
mod metadata;
mod progress;
mod scanner;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

pub use content::{
    AlternateTitleDto, AuthorDto, BookCondition, BookDto, BookMetadataAggregationDto, BookMetadataDto, BookSearch,
    BooleanOperator, ContentPage, EqualityOperator, MediaDto, PageRequest, ReadProgressDto, ReadStatus,
    ReadStatusOperator, SeriesCondition, SeriesDto, SeriesMetadataDto, SeriesSearch, SortDirection, SortOrder,
    StringOperator, WebLinkDto,
};
pub use media::{
    BookPage, Dimension, EPUB_ASSET_SUBTYPE, EPUB_EXTENSION_CLASS, EPUB_MEDIA_TYPE, EPUB_PAGE_SUBTYPE, EpubTocEntry,
    JPEG_MEDIA_TYPE, Media, MediaExtension, MediaExtensionEpub, MediaFile, MediaProfile, MediaStatus, PDF_MEDIA_TYPE,
    R2Location, R2Locator, R2Text, RAR_MEDIA_TYPE, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, ThumbnailBook, ZIP_MEDIA_TYPE,
};
pub use metadata::{
    AlternateTitle, Author, BookMetadata, BookMetadataAggregation, BookMetadataPatch, MetadataError, MetadataUpdate,
    ReadingDirection, SeriesMetadata, SeriesMetadataPatch, SeriesStatus, WebLink, aggregate_book_metadata,
};
pub use progress::{
    R2Device, R2Progression, ReadProgress, ReadProgressError, completed_progress, page_progress, progression,
};
pub use scanner::{Book, ScanError, ScanResult, ScannedSeries, Series, scan_library_files};

/// Every application role recognized by the current Komga compatibility layer.
///
/// `USER` is not stored as a role. It is added only when a [`UserDto`] is
/// produced, matching Spring Security's implicit authenticated-user authority.
pub const ALL_ROLES: [UserRole; 5] = [
    UserRole::Admin,
    UserRole::FileDownload,
    UserRole::PageStreaming,
    UserRole::KoboSync,
    UserRole::KoreaderSync,
];

/// A capability that can be granted to a Komga user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UserRole {
    /// Manage the server and bypass library visibility restrictions.
    Admin,
    /// Download original book files and generated archives.
    FileDownload,
    /// Stream individual book pages.
    PageStreaming,
    /// Synchronize books with Kobo devices.
    KoboSync,
    /// Synchronize reading progress with KOReader.
    KoreaderSync,
}

impl UserRole {
    /// Returns the uppercase identifier stored by the Kotlin database schema.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "ADMIN",
            Self::FileDownload => "FILE_DOWNLOAD",
            Self::PageStreaming => "PAGE_STREAMING",
            Self::KoboSync => "KOBO_SYNC",
            Self::KoreaderSync => "KOREADER_SYNC",
        }
    }
}

impl TryFrom<&str> for UserRole {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "ADMIN" => Ok(Self::Admin),
            "FILE_DOWNLOAD" => Ok(Self::FileDownload),
            "PAGE_STREAMING" => Ok(Self::PageStreaming),
            "KOBO_SYNC" => Ok(Self::KoboSync),
            "KOREADER_SYNC" => Ok(Self::KoreaderSync),
            _ => Err(()),
        }
    }
}

/// Determines whether an age boundary admits or rejects matching content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AllowExclude {
    /// Admit content at or below the configured age.
    AllowOnly,
    /// Reject content at or above the configured age.
    Exclude,
}

/// An age threshold and the policy applied at that threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeRestriction {
    /// Inclusive age threshold used by [`User::is_content_allowed`].
    pub age: u32,
    /// Whether the threshold acts as an allow-list or exclusion rule.
    pub restriction: AllowExclude,
}

/// Age and sharing-label restrictions attached to a user.
///
/// Instances created with [`ContentRestrictions::new`] are normalized: labels
/// are trimmed and lowercased, blank labels are removed, and an excluded label
/// is removed from the allow set when it occurs in both sets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentRestrictions {
    /// Optional age-based rule.
    pub age_restriction: Option<AgeRestriction>,
    /// Normalized labels that may admit otherwise restricted content.
    pub labels_allow: BTreeSet<String>,
    /// Normalized labels that always reject matching content.
    pub labels_exclude: BTreeSet<String>,
}

impl ContentRestrictions {
    /// Creates a normalized set of content restrictions.
    ///
    /// Exclusion wins when the same label occurs in both input collections.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use komga_core::ContentRestrictions;
    ///
    /// let restrictions = ContentRestrictions::new(
    ///     None,
    ///     [" Kids ".to_owned(), "blocked".to_owned()],
    ///     ["BLOCKED".to_owned()],
    /// );
    ///
    /// assert_eq!(restrictions.labels_allow, BTreeSet::from(["kids".to_owned()]));
    /// assert_eq!(restrictions.labels_exclude, BTreeSet::from(["blocked".to_owned()]));
    /// ```
    pub fn new(
        age_restriction: Option<AgeRestriction>,
        labels_allow: impl IntoIterator<Item = String>,
        labels_exclude: impl IntoIterator<Item = String>,
    ) -> Self {
        let labels_exclude = normalized_labels(labels_exclude);
        let labels_allow = normalized_labels(labels_allow)
            .difference(&labels_exclude)
            .cloned()
            .collect();

        Self {
            age_restriction,
            labels_allow,
            labels_exclude,
        }
    }
}

fn normalized_labels(labels: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    labels
        .into_iter()
        .map(|label| label.trim().to_lowercase())
        .filter(|label| !label.is_empty())
        .collect()
}

/// A persisted Komga user and the policy needed to authorize that user.
///
/// [`User::password_hash`] contains a BCrypt hash and must never be serialized
/// in an API response. Use [`User::to_dto`] for the public representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// Stable identifier used by Komga database relations.
    pub id: String,
    /// Login email. Persistence lookups treat this value case-insensitively.
    pub email: String,
    /// BCrypt password hash, including its version and cost prefix.
    pub password_hash: String,
    /// Explicit capabilities; the implicit `USER` role is not stored here.
    pub roles: BTreeSet<UserRole>,
    /// Whether all libraries are visible without per-library entries.
    pub shared_all_libraries: bool,
    /// Libraries visible when [`User::shared_all_libraries`] is false.
    pub shared_library_ids: BTreeSet<String>,
    /// Age and sharing-label restrictions applied to visible content.
    pub restrictions: ContentRestrictions,
}

impl User {
    /// Creates a user with a generated ID and unrestricted library visibility.
    ///
    /// The role set is empty. Claim and user-creation workflows are responsible
    /// for assigning the roles appropriate to their API contract.
    pub fn new(email: String, password_hash: String) -> Self {
        Self {
            id: new_id(),
            email,
            password_hash,
            roles: BTreeSet::new(),
            shared_all_libraries: true,
            shared_library_ids: BTreeSet::new(),
            restrictions: ContentRestrictions::default(),
        }
    }

    /// Returns whether the user has the administrator capability.
    pub fn is_admin(&self) -> bool {
        self.roles.contains(&UserRole::Admin)
    }

    /// Returns whether the user may access a library.
    ///
    /// Administrators always have access, matching the Kotlin implementation.
    pub fn can_access_library(&self, library_id: &str) -> bool {
        self.is_admin() || self.shared_all_libraries || self.shared_library_ids.contains(library_id)
    }

    /// Evaluates Komga's combined age and sharing-label policy.
    ///
    /// Allow rules are combined with logical OR. After admission, any matching
    /// exclusion rule rejects the content. Input labels are normalized before
    /// comparison.
    pub fn is_content_allowed(&self, age_rating: Option<u32>, sharing_labels: &BTreeSet<String>) -> bool {
        let labels = normalized_labels(sharing_labels.iter().cloned());
        let age_allowed = self.restrictions.age_restriction.as_ref().and_then(|restriction| {
            (restriction.restriction == AllowExclude::AllowOnly)
                .then(|| age_rating.is_some_and(|age| age <= restriction.age))
        });
        let label_allowed =
            (!self.restrictions.labels_allow.is_empty()).then(|| !self.restrictions.labels_allow.is_disjoint(&labels));

        let allowed = match (age_allowed, label_allowed) {
            (None, Some(label)) => label,
            (Some(age), None) => age,
            (Some(age), Some(label)) => age || label,
            (None, None) => true,
        };
        if !allowed {
            return false;
        }

        let age_denied = self.restrictions.age_restriction.as_ref().is_some_and(|restriction| {
            restriction.restriction == AllowExclude::Exclude && age_rating.is_some_and(|age| age >= restriction.age)
        });
        let label_denied = !self.restrictions.labels_exclude.is_disjoint(&labels);
        !age_denied && !label_denied
    }

    /// Produces the REST-safe representation of this user.
    ///
    /// This omits the password hash and adds the implicit `USER` role expected
    /// by existing Komga clients.
    pub fn to_dto(&self) -> UserDto {
        let roles = self
            .roles
            .iter()
            .map(|role| role.as_str().to_owned())
            .chain(std::iter::once("USER".to_owned()))
            .collect();

        UserDto {
            id: self.id.clone(),
            email: self.email.clone(),
            roles,
            shared_all_libraries: self.shared_all_libraries,
            shared_libraries_ids: self.shared_library_ids.clone(),
            labels_allow: self.restrictions.labels_allow.clone(),
            labels_exclude: self.restrictions.labels_exclude.clone(),
            age_restriction: self.restrictions.age_restriction.clone(),
        }
    }
}

/// Public user representation used by the Komga REST API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDto {
    /// Stable user identifier.
    pub id: String,
    /// Login email.
    pub email: String,
    /// Explicit role names plus the implicit `USER` role.
    pub roles: BTreeSet<String>,
    /// Whether the user can access every library.
    pub shared_all_libraries: bool,
    /// Explicitly shared library identifiers.
    pub shared_libraries_ids: BTreeSet<String>,
    /// Normalized labels that may admit content.
    pub labels_allow: BTreeSet<String>,
    /// Normalized labels that reject content.
    pub labels_exclude: BTreeSet<String>,
    /// Optional age policy; omitted from JSON when no policy is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_restriction: Option<AgeRestriction>,
}

/// REST representation of an API key record.
///
/// Creation responses contain the one-time plaintext key. List responses
/// replace [`ApiKey::key`] with `******`; persistence stores only its SHA-512
/// digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    /// Stable API key record identifier.
    pub id: String,
    /// Identifier of the owning user.
    pub user_id: String,
    /// Plaintext key, redaction marker, or persisted digest depending on layer.
    pub key: String,
    /// User-supplied description, unique per user.
    pub comment: String,
    /// UTC creation time serialized in Komga-compatible form.
    pub created_date: String,
    /// UTC modification time serialized in Komga-compatible form.
    pub last_modified_date: String,
}

/// How often a library is scheduled for a periodic scan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScanInterval {
    /// Do not schedule periodic scans.
    Disabled,
    /// Scan once per hour.
    Hourly,
    /// Scan every six hours.
    #[default]
    #[serde(rename = "EVERY_6H")]
    Every6h,
    /// Scan every twelve hours.
    #[serde(rename = "EVERY_12H")]
    Every12h,
    /// Scan once per day.
    Daily,
    /// Scan once per week.
    Weekly,
}

impl ScanInterval {
    /// Returns the identifier stored by Komga's SQLite schema.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "DISABLED",
            Self::Hourly => "HOURLY",
            Self::Every6h => "EVERY_6H",
            Self::Every12h => "EVERY_12H",
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
        }
    }
}

impl TryFrom<&str> for ScanInterval {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_ascii_uppercase().as_str() {
            "DISABLED" => Ok(Self::Disabled),
            "HOURLY" => Ok(Self::Hourly),
            "EVERY_6H" => Ok(Self::Every6h),
            "EVERY_12H" => Ok(Self::Every12h),
            "DAILY" => Ok(Self::Daily),
            "WEEKLY" => Ok(Self::Weekly),
            _ => Err(()),
        }
    }
}

/// Strategy used to choose a series cover from its books.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SeriesCover {
    /// Always use the first book.
    #[default]
    First,
    /// Prefer the first unread book, falling back to the first book.
    FirstUnreadOrFirst,
    /// Prefer the first unread book, falling back to the last book.
    FirstUnreadOrLast,
    /// Always use the last book.
    Last,
}

impl SeriesCover {
    /// Returns the identifier stored by Komga's SQLite schema.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::First => "FIRST",
            Self::FirstUnreadOrFirst => "FIRST_UNREAD_OR_FIRST",
            Self::FirstUnreadOrLast => "FIRST_UNREAD_OR_LAST",
            Self::Last => "LAST",
        }
    }
}

impl TryFrom<&str> for SeriesCover {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_ascii_uppercase().as_str() {
            "FIRST" => Ok(Self::First),
            "FIRST_UNREAD_OR_FIRST" => Ok(Self::FirstUnreadOrFirst),
            "FIRST_UNREAD_OR_LAST" => Ok(Self::FirstUnreadOrLast),
            "LAST" => Ok(Self::Last),
            _ => Err(()),
        }
    }
}

/// A configured media library and all scan/import options persisted by Komga.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    /// Stable library identifier.
    pub id: String,
    /// User-visible library name.
    pub name: String,
    /// Root directory represented as a file URL in SQLite.
    pub root: Url,
    /// Import book metadata from ComicInfo.xml.
    pub import_comic_info_book: bool,
    /// Import series metadata from ComicInfo.xml.
    pub import_comic_info_series: bool,
    /// Import collections from ComicInfo.xml.
    pub import_comic_info_collection: bool,
    /// Import read lists from ComicInfo.xml.
    pub import_comic_info_read_list: bool,
    /// Append volume data while importing ComicInfo series metadata.
    pub import_comic_info_series_append_volume: bool,
    /// Import book metadata from EPUB files.
    pub import_epub_book: bool,
    /// Import series metadata from EPUB files.
    pub import_epub_series: bool,
    /// Import Mylar series metadata.
    pub import_mylar_series: bool,
    /// Import local artwork files.
    pub import_local_artwork: bool,
    /// Import ISBN values from barcodes.
    pub import_barcode_isbn: bool,
    /// Use file modification times to force rescans.
    pub scan_force_modified_time: bool,
    /// Periodic scan interval.
    pub scan_interval: ScanInterval,
    /// Whether a scan should be scheduled at process startup.
    pub scan_on_startup: bool,
    /// Include CBX archives while scanning.
    pub scan_cbx: bool,
    /// Include PDF files while scanning.
    pub scan_pdf: bool,
    /// Include EPUB files while scanning.
    pub scan_epub: bool,
    /// Directory names excluded from scans.
    pub scan_directory_exclusions: BTreeSet<String>,
    /// Repair extensions that do not match detected media types.
    pub repair_extensions: bool,
    /// Convert supported books to CBZ.
    pub convert_to_cbz: bool,
    /// Purge unavailable entries after scans.
    pub empty_trash_after_scan: bool,
    /// Series cover selection strategy.
    pub series_cover: SeriesCover,
    /// Hash source book files.
    pub hash_files: bool,
    /// Hash individual pages.
    pub hash_pages: bool,
    /// Generate KOReader-compatible hashes.
    pub hash_koreader: bool,
    /// Analyze image dimensions.
    pub analyze_dimensions: bool,
    /// Optional directory name used for one-shot books.
    pub oneshots_directory: Option<String>,
    /// UTC timestamp set when the root becomes unavailable.
    pub unavailable_date: Option<String>,
    /// UTC creation timestamp supplied by persistence.
    pub created_date: Option<String>,
    /// UTC modification timestamp supplied by persistence.
    pub last_modified_date: Option<String>,
}

impl Library {
    /// Creates a library with the defaults used by Kotlin's creation DTO.
    pub fn new(name: String, root: Url) -> Self {
        Self {
            id: new_id(),
            name,
            root,
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
            unavailable_date: None,
            created_date: None,
            last_modified_date: None,
        }
    }

    /// Returns the root as a local filesystem path when it is a file URL.
    pub fn root_path(&self) -> Option<std::path::PathBuf> {
        self.root.to_file_path().ok()
    }

    /// Returns whether changes between two versions require a root rescan.
    pub fn should_rescan(&self, updated: &Self) -> bool {
        self.root != updated.root
            || self.oneshots_directory != updated.oneshots_directory
            || self.scan_cbx != updated.scan_cbx
            || self.scan_pdf != updated.scan_pdf
            || self.scan_epub != updated.scan_epub
            || self.scan_force_modified_time != updated.scan_force_modified_time
            || self.scan_directory_exclusions != updated.scan_directory_exclusions
    }
}

/// Generates a lowercase, hyphen-free UUID identifier.
///
/// The value is opaque to clients and fits Komga's string ID columns.
pub fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrictions_are_normalized_and_exclude_wins() {
        let restrictions =
            ContentRestrictions::new(None, [" Kids ".into(), "ADULT".into(), "".into()], ["adult".into()]);

        assert_eq!(restrictions.labels_allow, BTreeSet::from(["kids".into()]));
        assert_eq!(restrictions.labels_exclude, BTreeSet::from(["adult".into()]));
    }

    #[test]
    fn dto_adds_implicit_user_role_and_uses_komga_field_names() {
        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.roles.insert(UserRole::PageStreaming);

        let json = serde_json::to_value(user.to_dto()).unwrap();
        assert_eq!(json["sharedAllLibraries"], true);
        assert_eq!(json["roles"], serde_json::json!(["PAGE_STREAMING", "USER"]));
        assert!(json.get("ageRestriction").is_none());
    }

    #[test]
    fn content_rules_match_komga_allow_and_exclude_semantics() {
        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 12,
                restriction: AllowExclude::AllowOnly,
            }),
            ["family".into()],
            ["blocked".into()],
        );

        assert!(user.is_content_allowed(Some(10), &BTreeSet::new()));
        assert!(user.is_content_allowed(Some(16), &BTreeSet::from(["family".into()])));
        assert!(!user.is_content_allowed(Some(16), &BTreeSet::new()));
        assert!(!user.is_content_allowed(Some(10), &BTreeSet::from(["blocked".into()])));
    }

    #[test]
    fn library_defaults_and_rescan_rules_match_kotlin() {
        let root = Url::from_file_path(std::env::temp_dir()).unwrap();
        let library = Library::new("Library".into(), root);

        assert_eq!(library.scan_interval, ScanInterval::Every6h);
        assert!(library.scan_cbx && library.scan_pdf && library.scan_epub);
        assert!(library.hash_files && library.analyze_dimensions);
        assert!(!library.should_rescan(&library));

        let mut updated = library.clone();
        updated.scan_directory_exclusions.insert("ignored".into());
        assert!(library.should_rescan(&updated));
    }
}
