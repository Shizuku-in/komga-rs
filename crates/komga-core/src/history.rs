//! Durable administrative history records.

use std::collections::BTreeMap;

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
}
