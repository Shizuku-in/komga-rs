//! Durable administrative history records.

use std::{collections::BTreeMap, path::Path};

use chrono::{Local, NaiveDateTime};
use serde::Serialize;

use crate::{Book, HashedPageTarget, new_id};

/// One durable administrative event stored in Komga's history tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalEvent {
    /// Opaque event identifier.
    pub id: String,
    /// Kotlin-compatible event type discriminator.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Local date and time at which the event occurred.
    pub timestamp: NaiveDateTime,
    /// Related book identifier, when applicable.
    pub book_id: Option<String>,
    /// Related series identifier, when applicable.
    pub series_id: Option<String>,
    /// Event-specific string properties.
    pub properties: BTreeMap<String, String>,
}

impl HistoricalEvent {
    /// Creates a historical event stamped with the current local time.
    #[must_use]
    pub fn new(
        event_type: impl Into<String>,
        book_id: Option<String>,
        series_id: Option<String>,
        properties: BTreeMap<String, String>,
    ) -> Self {
        Self {
            id: new_id(),
            event_type: event_type.into(),
            timestamp: Local::now().naive_local(),
            book_id,
            series_id,
            properties,
        }
    }

    /// Creates the Kotlin-compatible audit record for one removed duplicate page.
    #[must_use]
    pub fn duplicate_page_deleted(book: &Book, page: &HashedPageTarget) -> Self {
        let name = book
            .url
            .to_file_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|()| book.url.to_string());
        Self::new(
            "DuplicatePageDeleted",
            Some(book.id.clone()),
            Some(book.series_id.clone()),
            BTreeMap::from([
                ("name".into(), name),
                ("page number".into(), page.page_number.to_string()),
                ("page file name".into(), page.file_name.clone()),
                ("page file hash".into(), page.file_hash.clone()),
                ("page file size".into(), page.file_size.to_string()),
                ("page media type".into(), page.media_type.clone()),
            ]),
        )
    }

    /// Creates the Kotlin-compatible audit record for a user-requested source-file deletion.
    #[must_use]
    pub fn book_file_deleted(book: &Book) -> Self {
        Self::book_file_deleted_for_reason(book, "File was deleted by user request")
    }

    /// Creates the Kotlin-compatible audit record for a source file deleted for a specified reason.
    #[must_use]
    pub fn book_file_deleted_for_reason(book: &Book, reason: &str) -> Self {
        let name = book
            .url
            .to_file_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|()| book.url.to_string());
        Self::new(
            "BookFileDeleted",
            Some(book.id.clone()),
            Some(book.series_id.clone()),
            BTreeMap::from([("reason".into(), reason.into()), ("name".into(), name)]),
        )
    }

    /// Creates the Kotlin-compatible audit record for one completed book import.
    #[must_use]
    pub fn book_imported(book: &Book, source: &Path, upgrade: bool) -> Self {
        let name = book
            .url
            .to_file_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|()| book.url.to_string());
        Self::new(
            "BookImported",
            Some(book.id.clone()),
            Some(book.series_id.clone()),
            BTreeMap::from([
                ("name".into(), name),
                ("source".into(), source.to_string_lossy().into_owned()),
                ("upgrade".into(), if upgrade { "Yes".into() } else { "No".into() }),
            ]),
        )
    }

    /// Creates the Kotlin-compatible audit record for an empty series directory deletion.
    #[must_use]
    pub fn series_folder_deleted(series_id: &str, path: &Path) -> Self {
        Self::new(
            "SeriesFolderDeleted",
            None,
            Some(series_id.to_owned()),
            BTreeMap::from([
                ("reason".into(), "Folder was deleted because it was empty".into()),
                ("name".into(), path.to_string_lossy().into_owned()),
            ]),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Local;
    use url::Url;

    use super::*;

    fn book() -> Book {
        let now = Local::now().naive_local();
        Book {
            id: "book".into(),
            name: "Volume".into(),
            url: Url::from_file_path("/library/Series/Volume.cbz").unwrap(),
            file_last_modified: now,
            file_size: 42,
            file_hash: String::new(),
            file_hash_koreader: String::new(),
            number: 1,
            series_id: "series".into(),
            library_id: "library".into(),
            deleted_date: None,
            oneshot: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    #[test]
    fn source_deletion_events_match_kotlin_fields() {
        let event = HistoricalEvent::book_file_deleted(&book());
        assert_eq!(event.event_type, "BookFileDeleted");
        assert_eq!(event.book_id.as_deref(), Some("book"));
        assert_eq!(event.series_id.as_deref(), Some("series"));
        assert_eq!(event.properties["reason"], "File was deleted by user request");
        assert_eq!(event.properties["name"], "/library/Series/Volume.cbz");

        let event = HistoricalEvent::series_folder_deleted("series", Path::new("/library/Series"));
        assert_eq!(event.event_type, "SeriesFolderDeleted");
        assert_eq!(event.book_id, None);
        assert_eq!(event.series_id.as_deref(), Some("series"));
        assert_eq!(event.properties["reason"], "Folder was deleted because it was empty");
        assert_eq!(event.properties["name"], "/library/Series");

        let event = HistoricalEvent::book_imported(&book(), Path::new("/imports/Volume.cbz"), true);
        assert_eq!(event.event_type, "BookImported");
        assert_eq!(event.book_id.as_deref(), Some("book"));
        assert_eq!(event.series_id.as_deref(), Some("series"));
        assert_eq!(event.properties["name"], "/library/Series/Volume.cbz");
        assert_eq!(event.properties["source"], "/imports/Volume.cbz");
        assert_eq!(event.properties["upgrade"], "Yes");

        let event = HistoricalEvent::book_file_deleted_for_reason(&book(), "File was deleted to import an upgrade");
        assert_eq!(event.properties["reason"], "File was deleted to import an upgrade");
    }
}
