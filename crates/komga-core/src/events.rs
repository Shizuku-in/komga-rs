//! Framework-independent domain change notifications.

use crate::{Book, CollectionDto, Library, ReadListDto, Series};

/// Library identity carried by library events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEvent {
    /// Library identifier.
    pub library_id: String,
}

impl From<&Library> for LibraryEvent {
    fn from(value: &Library) -> Self {
        Self {
            library_id: value.id.clone(),
        }
    }
}

/// Series identity carried by series events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesEvent {
    /// Series identifier.
    pub series_id: String,
    /// Owning library identifier.
    pub library_id: String,
}

impl From<&Series> for SeriesEvent {
    fn from(value: &Series) -> Self {
        Self {
            series_id: value.id.clone(),
            library_id: value.library_id.clone(),
        }
    }
}

/// Book identity carried by book events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookEvent {
    /// Book identifier.
    pub book_id: String,
    /// Parent series identifier.
    pub series_id: String,
    /// Owning library identifier.
    pub library_id: String,
}

impl From<&Book> for BookEvent {
    fn from(value: &Book) -> Self {
        Self {
            book_id: value.id.clone(),
            series_id: value.series_id.clone(),
            library_id: value.library_id.clone(),
        }
    }
}

/// Collection or read-list identity and current ordered members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerEvent {
    /// Container identifier.
    pub container_id: String,
    /// Current series or book identifiers in stored order.
    pub item_ids: Vec<String>,
}

impl From<&CollectionDto> for ContainerEvent {
    fn from(value: &CollectionDto) -> Self {
        Self {
            container_id: value.id.clone(),
            item_ids: value.series_ids.clone(),
        }
    }
}

impl From<&ReadListDto> for ContainerEvent {
    fn from(value: &ReadListDto) -> Self {
        Self {
            container_id: value.id.clone(),
            item_ids: value.book_ids.clone(),
        }
    }
}

/// One user's reading-progress change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadProgressEvent {
    /// Book identifier.
    pub book_id: String,
    /// User identifier.
    pub user_id: String,
}

/// One user's series-level reading-progress change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesReadProgressEvent {
    /// Series identifier.
    pub series_id: String,
    /// User identifier.
    pub user_id: String,
}

/// Book-thumbnail change details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookThumbnailEvent {
    /// Book identifier.
    pub book_id: String,
    /// Parent series identifier.
    pub series_id: String,
    /// Whether the affected thumbnail is selected.
    pub selected: bool,
}

/// Series-thumbnail change details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeriesThumbnailEvent {
    /// Series identifier.
    pub series_id: String,
    /// Whether the affected thumbnail is selected.
    pub selected: bool,
}

/// Collection or read-list thumbnail change details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerThumbnailEvent {
    /// Parent container identifier.
    pub container_id: String,
    /// Whether the affected thumbnail is selected.
    pub selected: bool,
}

/// Administrative book-import result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookImportEvent {
    /// Imported book identifier on success.
    pub book_id: Option<String>,
    /// Source filesystem path.
    pub source_file: String,
    /// Whether the import succeeded.
    pub success: bool,
    /// Optional failure detail.
    pub message: Option<String>,
}

/// User update details relevant to active sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserEvent {
    /// User identifier.
    pub user_id: String,
    /// Whether existing sessions must expire.
    pub expire_sessions: bool,
}

/// Changes emitted after durable domain mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    /// A library was created.
    LibraryAdded(LibraryEvent),
    /// A library was changed.
    LibraryUpdated(LibraryEvent),
    /// A library was deleted.
    LibraryDeleted(LibraryEvent),
    /// A library scan completed; Kotlin does not expose this event over SSE.
    LibraryScanned(LibraryEvent),
    /// A series was created.
    SeriesAdded(SeriesEvent),
    /// A series was changed or soft-deleted.
    SeriesUpdated(SeriesEvent),
    /// A series was permanently deleted.
    SeriesDeleted(SeriesEvent),
    /// A book was created.
    BookAdded(BookEvent),
    /// A book was changed or soft-deleted.
    BookUpdated(BookEvent),
    /// A book was permanently deleted.
    BookDeleted(BookEvent),
    /// A book import completed.
    BookImported(BookImportEvent),
    /// A collection was created.
    CollectionAdded(ContainerEvent),
    /// A collection was changed.
    CollectionUpdated(ContainerEvent),
    /// A collection was deleted.
    CollectionDeleted(ContainerEvent),
    /// A read list was created.
    ReadListAdded(ContainerEvent),
    /// A read list was changed.
    ReadListUpdated(ContainerEvent),
    /// A read list was deleted.
    ReadListDeleted(ContainerEvent),
    /// Reading progress for one book changed.
    ReadProgressChanged(ReadProgressEvent),
    /// Reading progress for one book was deleted.
    ReadProgressDeleted(ReadProgressEvent),
    /// Series reading progress changed.
    ReadProgressSeriesChanged(SeriesReadProgressEvent),
    /// Series reading progress was deleted.
    ReadProgressSeriesDeleted(SeriesReadProgressEvent),
    /// A book thumbnail was added or selected.
    ThumbnailBookAdded(BookThumbnailEvent),
    /// A book thumbnail was deleted.
    ThumbnailBookDeleted(BookThumbnailEvent),
    /// A series thumbnail was added or selected.
    ThumbnailSeriesAdded(SeriesThumbnailEvent),
    /// A series thumbnail was deleted.
    ThumbnailSeriesDeleted(SeriesThumbnailEvent),
    /// A collection thumbnail was added or selected.
    ThumbnailSeriesCollectionAdded(ContainerThumbnailEvent),
    /// A collection thumbnail was deleted.
    ThumbnailSeriesCollectionDeleted(ContainerThumbnailEvent),
    /// A read-list thumbnail was added or selected.
    ThumbnailReadListAdded(ContainerThumbnailEvent),
    /// A read-list thumbnail was deleted.
    ThumbnailReadListDeleted(ContainerThumbnailEvent),
    /// A user was updated.
    UserUpdated(UserEvent),
    /// A user was deleted.
    UserDeleted(UserEvent),
}
