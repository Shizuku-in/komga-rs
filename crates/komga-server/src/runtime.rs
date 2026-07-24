//! Background task processing and periodic library scan scheduling.

use std::{collections::HashMap, time::Duration};

use komga_core::{ScanError, ScanInterval, scan_library_files};
use komga_storage::{StorageError, TaskRecord};
use serde::Deserialize;
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::AppState;

/// Cloneable control used to coordinate HTTP and background-service shutdown.
#[derive(Clone)]
pub struct ShutdownHandle {
    token: CancellationToken,
}

impl ShutdownHandle {
    /// Requests shutdown of the HTTP server, workers, scheduler, and SSE streams.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// Resolves when shutdown has been requested, consuming this handle.
    pub async fn cancelled_owned(self) {
        self.token.cancelled().await;
    }
}

/// Owned handles for background services started around an [`AppState`].
pub struct AppRuntime {
    shutdown: ShutdownHandle,
    task_notify: std::sync::Arc<tokio::sync::Notify>,
    schedule_notify: std::sync::Arc<tokio::sync::Notify>,
    handles: Vec<JoinHandle<()>>,
}

impl AppRuntime {
    /// Returns a shutdown handle suitable for signal handling and Axum.
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        self.shutdown.clone()
    }

    /// Cancels background work and waits for every service to release its resources.
    pub async fn shutdown(self) {
        self.shutdown.cancel();
        self.task_notify.notify_waiters();
        self.schedule_notify.notify_waiters();
        for handle in self.handles {
            if let Err(error) = handle.await {
                tracing::error!(%error, "background service terminated unexpectedly");
            }
        }
    }
}

impl AppState {
    /// Starts scan workers and the periodic library scheduler.
    ///
    /// State construction intentionally does not start background work, allowing
    /// HTTP tests and embedding applications to control lifecycle explicitly.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the configured task pool size cannot be read.
    pub async fn start_runtime(&self) -> Result<AppRuntime, StorageError> {
        let worker_count = self.storage.task_pool_size().await?;
        let mut handles = Vec::with_capacity(worker_count + 1);
        for index in 0..worker_count {
            handles.push(tokio::spawn(worker_loop(self.clone(), index)));
        }
        handles.push(tokio::spawn(scheduler_loop(self.clone())));
        self.task_notify.notify_waiters();
        Ok(AppRuntime {
            shutdown: ShutdownHandle {
                token: self.shutdown.clone(),
            },
            task_notify: self.task_notify.clone(),
            schedule_notify: self.schedule_notify.clone(),
            handles,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanLibraryPayload {
    library_id: String,
    scan_deep: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskOutcome {
    Completed,
    Cancelled,
}

async fn worker_loop(state: AppState, index: usize) {
    let owner = format!("komga-rs-{}-{index}", std::process::id());
    loop {
        let notified = state.task_notify.notified();
        tokio::pin!(notified);
        if state.shutdown.is_cancelled() {
            break;
        }
        match state.tasks.take_first_of_type(&owner, "ScanLibrary").await {
            Ok(Some(task)) => {
                let outcome = process_scan_task(&state, &task).await;
                let result = match outcome {
                    TaskOutcome::Completed => state.tasks.delete(&task.id).await,
                    TaskOutcome::Cancelled => state.tasks.release(&task.id, &owner).await,
                };
                if let Err(error) = result {
                    tracing::error!(%error, task_id = %task.id, "could not finalize task ownership");
                }
            }
            Ok(None) => {
                tokio::select! {
                    () = state.shutdown.cancelled() => break,
                    () = &mut notified => {}
                }
            }
            Err(error) => {
                tracing::error!(%error, worker = %owner, "could not claim task");
                tokio::select! {
                    () = state.shutdown.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
    }
}

async fn process_scan_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<ScanLibraryPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid ScanLibrary payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => {
            tracing::warn!(library_id = %payload.library_id, "scan task references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load library for scan");
            return TaskOutcome::Completed;
        }
    };
    let cancellation = state.shutdown.clone();
    let scan = tokio::task::spawn_blocking(move || scan_library_files(&library, || cancellation.is_cancelled())).await;
    let scan = match scan {
        Ok(Ok(scan)) => scan,
        Ok(Err(ScanError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(ScanError::RootUnavailable(path))) => {
            if let Err(error) = state.storage.set_library_unavailable(&payload.library_id, true).await {
                tracing::error!(%error, library_id = %payload.library_id, "could not mark library unavailable");
            }
            tracing::error!(library_id = %payload.library_id, root = %path.display(), "library scan failed");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "filesystem scanner panicked");
            return TaskOutcome::Completed;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .apply_library_scan(&payload.library_id, &scan, payload.scan_deep)
        .await
    {
        Ok(stats) => {
            tracing::info!(library_id = %payload.library_id, ?stats, "library scan completed");
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not persist library scan");
        }
    }
    TaskOutcome::Completed
}

#[derive(Debug, Clone, Copy)]
struct Schedule {
    interval: ScanInterval,
    next: Instant,
}

async fn scheduler_loop(state: AppState) {
    let mut schedules = HashMap::<String, Schedule>::new();
    match state.storage.list_libraries().await {
        Ok(libraries) => {
            for library in &libraries {
                if library.scan_on_startup
                    && let Err(error) = state
                        .enqueue_task(&TaskRecord::scan_library(&library.id, false, 4))
                        .await
                {
                    tracing::error!(%error, library_id = %library.id, "could not enqueue startup scan");
                }
            }
            reconcile_schedules(&mut schedules, &libraries);
        }
        Err(error) => tracing::error!(%error, "could not initialize library schedules"),
    }

    loop {
        let next = schedules.values().map(|schedule| schedule.next).min();
        let sleep = async {
            if let Some(next) = next {
                tokio::time::sleep_until(next).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(sleep);
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            () = state.schedule_notify.notified() => {
                match state.storage.list_libraries().await {
                    Ok(libraries) => reconcile_schedules(&mut schedules, &libraries),
                    Err(error) => tracing::error!(%error, "could not refresh library schedules"),
                }
            }
            () = &mut sleep => {
                let now = Instant::now();
                let due = schedules
                    .iter()
                    .filter(|(_, schedule)| schedule.next <= now)
                    .map(|(library_id, _)| library_id.clone())
                    .collect::<Vec<_>>();
                for library_id in due {
                    if let Err(error) = state
                        .enqueue_task(&TaskRecord::scan_library(&library_id, false, 4))
                        .await
                    {
                        tracing::error!(%error, %library_id, "could not enqueue periodic scan");
                    }
                    if let Some(schedule) = schedules.get_mut(&library_id) {
                        schedule.next = now + scan_interval_duration(schedule.interval);
                    }
                }
            }
        }
    }
}

fn reconcile_schedules(schedules: &mut HashMap<String, Schedule>, libraries: &[komga_core::Library]) {
    let now = Instant::now();
    schedules.retain(|library_id, _| libraries.iter().any(|library| library.id == *library_id));
    for library in libraries {
        if library.scan_interval == ScanInterval::Disabled {
            schedules.remove(&library.id);
            continue;
        }
        match schedules.get(&library.id) {
            Some(schedule) if schedule.interval == library.scan_interval => {}
            _ => {
                schedules.insert(
                    library.id.clone(),
                    Schedule {
                        interval: library.scan_interval,
                        next: now + scan_interval_duration(library.scan_interval),
                    },
                );
            }
        }
    }
}

fn scan_interval_duration(interval: ScanInterval) -> Duration {
    match interval {
        ScanInterval::Disabled => Duration::MAX,
        ScanInterval::Hourly => Duration::from_secs(60 * 60),
        ScanInterval::Every6h => Duration::from_secs(6 * 60 * 60),
        ScanInterval::Every12h => Duration::from_secs(12 * 60 * 60),
        ScanInterval::Daily => Duration::from_secs(24 * 60 * 60),
        ScanInterval::Weekly => Duration::from_secs(7 * 24 * 60 * 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_intervals_match_kotlin_durations() {
        assert_eq!(scan_interval_duration(ScanInterval::Hourly), Duration::from_secs(3600));
        assert_eq!(
            scan_interval_duration(ScanInterval::Every6h),
            Duration::from_secs(21600)
        );
        assert_eq!(
            scan_interval_duration(ScanInterval::Every12h),
            Duration::from_secs(43200)
        );
        assert_eq!(scan_interval_duration(ScanInterval::Daily), Duration::from_secs(86400));
        assert_eq!(
            scan_interval_duration(ScanInterval::Weekly),
            Duration::from_secs(604800)
        );
    }
}
