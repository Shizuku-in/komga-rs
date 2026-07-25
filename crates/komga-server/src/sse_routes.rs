use std::{collections::BTreeMap, convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    extract::{Extension, State},
    response::sse::{Event, KeepAlive, Sse},
};
use serde::Serialize;
use serde_json::json;
use tokio::sync::broadcast;

use komga_core::DomainEvent;

use crate::{AppState, auth::AuthContext};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskQueueStatus {
    count: i64,
    count_by_type: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Audience {
    All,
    Admin,
    User(String),
}

impl Audience {
    fn allows(&self, is_admin: bool, user_id: &str) -> bool {
        match self {
            Self::All => true,
            Self::Admin => is_admin,
            Self::User(expected) => expected == user_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DomainSseEvent {
    name: &'static str,
    data: String,
    audience: Audience,
}

pub(crate) async fn events(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let is_admin = auth.user.is_admin();
    let user_id = auth.user.id.clone();
    let stream = stream! {
        // Subscribe inside the response stream so the extracted AppState keeps
        // the broadcast sender alive after the router has produced the response.
        let mut domain_events = state.subscribe_domain_events();
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = state.shutdown.cancelled() => break,
                _ = interval.tick() => {
                    if !is_admin {
                        continue;
                    }
                    match state.tasks.count_by_simple_type().await {
                        Ok(count_by_type) => {
                            let status = TaskQueueStatus {
                                count: count_by_type.values().sum(),
                                count_by_type,
                            };
                            let data = serde_json::to_string(&status)
                                .expect("TaskQueueStatus serialization cannot fail");
                            yield Ok(Event::default().event("TaskQueueStatus").data(data));
                        }
                        Err(error) => tracing::error!(%error, "could not count tasks for SSE"),
                    }
                }
                received = domain_events.recv() => {
                    match received {
                        Ok(event) => {
                            if let Some(event) = domain_sse_event(&event)
                                && event.audience.allows(is_admin, &user_id)
                            {
                                yield Ok(Event::default().event(event.name).data(event.data));
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(%skipped, %user_id, "SSE receiver lagged behind domain events");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("heartbeat"))
}

fn domain_sse_event(event: &DomainEvent) -> Option<DomainSseEvent> {
    let (name, data, audience) = match event {
        DomainEvent::LibraryAdded(value) => ("LibraryAdded", json!({"libraryId": value.library_id}), Audience::All),
        DomainEvent::LibraryUpdated(value) => ("LibraryChanged", json!({"libraryId": value.library_id}), Audience::All),
        DomainEvent::LibraryDeleted(value) => ("LibraryDeleted", json!({"libraryId": value.library_id}), Audience::All),
        DomainEvent::LibraryScanned(_) => return None,
        DomainEvent::SeriesAdded(value) => (
            "SeriesAdded",
            json!({"seriesId": value.series_id, "libraryId": value.library_id}),
            Audience::All,
        ),
        DomainEvent::SeriesUpdated(value) => (
            "SeriesChanged",
            json!({"seriesId": value.series_id, "libraryId": value.library_id}),
            Audience::All,
        ),
        DomainEvent::SeriesDeleted(value) => (
            "SeriesDeleted",
            json!({"seriesId": value.series_id, "libraryId": value.library_id}),
            Audience::All,
        ),
        DomainEvent::BookAdded(value) => (
            "BookAdded",
            json!({"bookId": value.book_id, "seriesId": value.series_id, "libraryId": value.library_id}),
            Audience::All,
        ),
        DomainEvent::BookUpdated(value) => (
            "BookChanged",
            json!({"bookId": value.book_id, "seriesId": value.series_id, "libraryId": value.library_id}),
            Audience::All,
        ),
        DomainEvent::BookDeleted(value) => (
            "BookDeleted",
            json!({"bookId": value.book_id, "seriesId": value.series_id, "libraryId": value.library_id}),
            Audience::All,
        ),
        DomainEvent::BookImported(value) => (
            "BookImported",
            json!({
                "bookId": value.book_id,
                "sourceFile": value.source_file,
                "success": value.success,
                "message": value.message,
            }),
            Audience::Admin,
        ),
        DomainEvent::CollectionAdded(value) => (
            "CollectionAdded",
            json!({"collectionId": value.container_id, "seriesIds": value.item_ids}),
            Audience::All,
        ),
        DomainEvent::CollectionUpdated(value) => (
            "CollectionChanged",
            json!({"collectionId": value.container_id, "seriesIds": value.item_ids}),
            Audience::All,
        ),
        DomainEvent::CollectionDeleted(value) => (
            "CollectionDeleted",
            json!({"collectionId": value.container_id, "seriesIds": value.item_ids}),
            Audience::All,
        ),
        DomainEvent::ReadListAdded(value) => (
            "ReadListAdded",
            json!({"readListId": value.container_id, "bookIds": value.item_ids}),
            Audience::All,
        ),
        DomainEvent::ReadListUpdated(value) => (
            "ReadListChanged",
            json!({"readListId": value.container_id, "bookIds": value.item_ids}),
            Audience::All,
        ),
        DomainEvent::ReadListDeleted(value) => (
            "ReadListDeleted",
            json!({"readListId": value.container_id, "bookIds": value.item_ids}),
            Audience::All,
        ),
        DomainEvent::ReadProgressChanged(value) => (
            "ReadProgressChanged",
            json!({"bookId": value.book_id, "userId": value.user_id}),
            Audience::User(value.user_id.clone()),
        ),
        DomainEvent::ReadProgressDeleted(value) => (
            "ReadProgressDeleted",
            json!({"bookId": value.book_id, "userId": value.user_id}),
            Audience::User(value.user_id.clone()),
        ),
        DomainEvent::ReadProgressSeriesChanged(value) => (
            "ReadProgressSeriesChanged",
            json!({"seriesId": value.series_id, "userId": value.user_id}),
            Audience::User(value.user_id.clone()),
        ),
        DomainEvent::ReadProgressSeriesDeleted(value) => (
            "ReadProgressSeriesDeleted",
            json!({"seriesId": value.series_id, "userId": value.user_id}),
            Audience::User(value.user_id.clone()),
        ),
        DomainEvent::ThumbnailBookAdded(value) => (
            "ThumbnailBookAdded",
            json!({"bookId": value.book_id, "seriesId": value.series_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailBookDeleted(value) => (
            "ThumbnailBookDeleted",
            json!({"bookId": value.book_id, "seriesId": value.series_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailSeriesAdded(value) => (
            "ThumbnailSeriesAdded",
            json!({"seriesId": value.series_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailSeriesDeleted(value) => (
            "ThumbnailSeriesDeleted",
            json!({"seriesId": value.series_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailSeriesCollectionAdded(value) => (
            "ThumbnailSeriesCollectionAdded",
            json!({"collectionId": value.container_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailSeriesCollectionDeleted(value) => (
            "ThumbnailSeriesCollectionDeleted",
            json!({"collectionId": value.container_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailReadListAdded(value) => (
            "ThumbnailReadListAdded",
            json!({"readListId": value.container_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::ThumbnailReadListDeleted(value) => (
            "ThumbnailReadListDeleted",
            json!({"readListId": value.container_id, "selected": value.selected}),
            Audience::All,
        ),
        DomainEvent::UserUpdated(value) if value.expire_sessions => (
            "SessionExpired",
            json!({"userId": value.user_id}),
            Audience::User(value.user_id.clone()),
        ),
        DomainEvent::UserUpdated(_) => return None,
        DomainEvent::UserDeleted(value) => (
            "SessionExpired",
            json!({"userId": value.user_id}),
            Audience::User(value.user_id.clone()),
        ),
    };
    Some(DomainSseEvent {
        name,
        data: data.to_string(),
        audience,
    })
}

#[cfg(test)]
mod tests {
    use komga_core::{
        BookEvent, BookImportEvent, ContainerEvent, DomainEvent, LibraryEvent, ReadProgressEvent, UserEvent,
    };

    use super::*;

    #[test]
    fn maps_kotlin_names_payloads_and_audiences() {
        let book = domain_sse_event(&DomainEvent::BookUpdated(BookEvent {
            book_id: "book".into(),
            series_id: "series".into(),
            library_id: "library".into(),
        }))
        .unwrap();
        assert_eq!(book.name, "BookChanged");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&book.data).unwrap(),
            json!({"bookId":"book","seriesId":"series","libraryId":"library"})
        );
        assert!(book.audience.allows(false, "reader"));

        let collection = domain_sse_event(&DomainEvent::CollectionAdded(ContainerEvent {
            container_id: "collection".into(),
            item_ids: vec!["series".into()],
        }))
        .unwrap();
        assert_eq!(collection.name, "CollectionAdded");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&collection.data).unwrap(),
            json!({"collectionId":"collection","seriesIds":["series"]})
        );

        let progress = domain_sse_event(&DomainEvent::ReadProgressChanged(ReadProgressEvent {
            book_id: "book".into(),
            user_id: "target".into(),
        }))
        .unwrap();
        assert!(progress.audience.allows(false, "target"));
        assert!(!progress.audience.allows(true, "other"));

        let imported = domain_sse_event(&DomainEvent::BookImported(BookImportEvent {
            book_id: None,
            source_file: "/imports/book.cbz".into(),
            success: false,
            message: Some("failed".into()),
        }))
        .unwrap();
        assert_eq!(imported.name, "BookImported");
        assert!(imported.audience.allows(true, "admin"));
        assert!(!imported.audience.allows(false, "reader"));

        assert!(
            domain_sse_event(&DomainEvent::LibraryScanned(LibraryEvent {
                library_id: "library".into()
            }))
            .is_none()
        );
        assert!(
            domain_sse_event(&DomainEvent::UserUpdated(UserEvent {
                user_id: "user".into(),
                expire_sessions: false,
            }))
            .is_none()
        );
    }
}
