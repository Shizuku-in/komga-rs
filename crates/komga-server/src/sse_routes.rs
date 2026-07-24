use std::{collections::BTreeMap, convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    extract::{Extension, State},
    response::sse::{Event, KeepAlive, Sse},
};
use serde::Serialize;

use crate::{AppState, auth::AuthContext};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskQueueStatus {
    count: i64,
    count_by_type: BTreeMap<String, i64>,
}

pub(crate) async fn events(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let is_admin = auth.user.is_admin();
    let stream = stream! {
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
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("heartbeat"))
}
