//! Durable administrative history records.

use std::collections::BTreeMap;

use chrono::{Local, NaiveDateTime};
use serde::Serialize;

use crate::new_id;

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
}
