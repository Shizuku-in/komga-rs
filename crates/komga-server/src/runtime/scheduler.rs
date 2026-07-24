use std::{collections::HashMap, time::Duration};

use komga_core::ScanInterval;
use komga_storage::TaskRecord;
use tokio::time::Instant;

use crate::AppState;

#[derive(Debug, Clone, Copy)]
struct Schedule {
    interval: ScanInterval,
    next: Instant,
}

pub(super) async fn scheduler_loop(state: AppState) {
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
