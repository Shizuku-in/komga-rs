//! Device synchronization snapshots and classified differences.

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::ReadProgress;

/// Stable identifier used for Komga's virtual Kobo On Deck tag.
pub const ON_DECK_READ_LIST_ID: &str = "KOMGA-ONDECK";

/// One immutable view of a user's synchronizable library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPoint {
    /// Opaque snapshot identifier.
    pub id: String,
    /// User whose visible content was captured.
    pub user_id: String,
    /// API key that initiated the sync, when available.
    pub api_key_id: Option<String>,
    /// Snapshot creation time in UTC.
    pub created_date: NaiveDateTime,
}

/// Book state captured in a [`SyncPoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPointBook {
    /// Owning snapshot identifier.
    pub sync_point_id: String,
    /// Book identifier.
    pub book_id: String,
    /// Original book creation time.
    pub created_date: NaiveDateTime,
    /// Book row modification time.
    pub last_modified_date: NaiveDateTime,
    /// Source file modification time.
    pub file_last_modified: NaiveDateTime,
    /// Source file size in bytes.
    pub file_size: i64,
    /// Standard source hash, which may be empty when hashing is disabled.
    pub file_hash: String,
    /// Book metadata modification time.
    pub metadata_last_modified_date: NaiveDateTime,
    /// User progress modification time, when progress exists.
    pub read_progress_last_modified_date: Option<NaiveDateTime>,
    /// Selected book thumbnail at snapshot time.
    pub thumbnail_id: Option<String>,
    /// Whether this entry has been claimed by a sync response.
    pub synced: bool,
}

/// Read-list state captured in a [`SyncPoint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPointReadList {
    /// Owning snapshot identifier.
    pub sync_point_id: String,
    /// Read-list or virtual tag identifier.
    pub read_list_id: String,
    /// Display name.
    pub read_list_name: String,
    /// Creation time.
    pub created_date: NaiveDateTime,
    /// Modification time used for incremental comparisons.
    pub last_modified_date: NaiveDateTime,
    /// Whether this entry has been claimed by a sync response.
    pub synced: bool,
}

/// A captured read-list membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPointReadListBook {
    /// Owning snapshot identifier.
    pub sync_point_id: String,
    /// Read-list identifier.
    pub read_list_id: String,
    /// Member book identifier.
    pub book_id: String,
}

/// Token carried in Kobo's `x-kobo-synctoken` header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KomgaSyncToken {
    /// Token schema version.
    #[serde(default = "sync_token_version")]
    pub version: u32,
    /// Opaque official Kobo token retained for future proxying.
    pub raw_kobo_sync_token: String,
    /// Snapshot currently being paged, if any.
    pub ongoing_sync_point_id: Option<String>,
    /// Most recent completely delivered snapshot, if any.
    pub last_successful_sync_point_id: Option<String>,
}

const fn sync_token_version() -> u32 {
    1
}

impl Default for KomgaSyncToken {
    fn default() -> Self {
        Self {
            version: sync_token_version(),
            raw_kobo_sync_token: String::new(),
            ongoing_sync_point_id: None,
            last_successful_sync_point_id: None,
        }
    }
}

impl KomgaSyncToken {
    /// Creates the version-one empty token used for a first synchronization.
    pub fn initial() -> Self {
        Self::default()
    }
}

/// Incremental operation represented by one synchronization source item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncChangeKind {
    /// The item did not exist in the previous successful snapshot.
    Added,
    /// Content, metadata, cover, or tag membership changed.
    Changed,
    /// The item no longer exists in the current snapshot.
    Removed,
    /// Only the user's reading state changed.
    ProgressChanged,
}

/// Metadata needed to map a live book into a device response.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncBookProjection {
    /// Book identifier.
    pub book_id: String,
    /// Parent series identifier.
    pub series_id: String,
    /// Display title.
    pub title: String,
    /// Display number within the series.
    pub number: String,
    /// Numeric number used by Kobo.
    pub number_sort: f32,
    /// Optional normalized ISBN.
    pub isbn: Option<String>,
    /// Summary; an empty summary is normalized by the transport layer.
    pub summary: String,
    /// Publication date, when known.
    pub release_date: Option<NaiveDate>,
    /// Metadata creation date used as a publication fallback.
    pub metadata_created_date: NaiveDateTime,
    /// Series title.
    pub series_title: String,
    /// Series publisher.
    pub publisher: String,
    /// Series language tag.
    pub language: String,
    /// Whether the book is represented as a one-shot series.
    pub oneshot: bool,
    /// Whether the EPUB is already Kobo-formatted.
    pub epub_is_kepub: bool,
    /// Whether the EPUB package declares pre-paginated layout.
    pub epub_is_fixed_layout: bool,
    /// Book contributor names in persisted order.
    pub authors: Vec<String>,
}

/// Reading progress with persistence timestamps required by Kobo DTOs.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncReadProgress {
    /// Normalized domain progression.
    pub progress: ReadProgress,
    /// Database row creation time.
    pub created_date: NaiveDateTime,
    /// Database row modification time.
    pub last_modified_date: NaiveDateTime,
}

/// One classified book item claimed by a synchronization page.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncBookChange {
    /// Incremental category.
    pub kind: SyncChangeKind,
    /// Snapshot row used for entitlement timestamps and removal responses.
    pub snapshot: SyncPointBook,
    /// Live metadata for non-removed books.
    pub projection: Option<SyncBookProjection>,
    /// Current user reading progress, if present.
    pub read_progress: Option<SyncReadProgress>,
}

/// One classified read-list or virtual tag claimed by a sync page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReadListChange {
    /// Incremental category.
    pub kind: SyncChangeKind,
    /// Snapshot row used for tag timestamps.
    pub snapshot: SyncPointReadList,
    /// Current member book identifiers for added or changed tags.
    pub book_ids: Vec<String>,
}

/// Transactionally claimed synchronization page.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncBatch {
    /// Book changes in protocol category and stable identifier order.
    pub books: Vec<SyncBookChange>,
    /// Read-list changes in protocol category and stable identifier order.
    pub read_lists: Vec<SyncReadListChange>,
    /// Whether unclaimed source items remain.
    pub has_more: bool,
}
