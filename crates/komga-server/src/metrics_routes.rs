//! Kotlin-compatible application metrics and task instrumentation.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
};
use serde::Serialize;

use crate::{AppState, auth::AuthContext, error::ApiError, routes::require_admin};

const METRIC_NAMES: [&str; 9] = [
    "komga.books",
    "komga.books.filesize",
    "komga.collections",
    "komga.libraries",
    "komga.readlists",
    "komga.series",
    "komga.sidecars",
    "komga.tasks.execution",
    "komga.tasks.failure",
];

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct TimerValue {
    count: u64,
    total_time: f64,
    max: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct TaskMetricsSnapshot {
    execution: BTreeMap<String, TimerValue>,
    failures: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TaskMetrics {
    inner: Arc<Mutex<TaskMetricsSnapshot>>,
}

impl TaskMetrics {
    pub(crate) fn record_execution(&self, task_type: &str, duration: Duration) {
        let mut metrics = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let timer = metrics.execution.entry(task_type.to_owned()).or_default();
        let seconds = duration.as_secs_f64();
        timer.count = timer.count.saturating_add(1);
        timer.total_time += seconds;
        timer.max = timer.max.max(seconds);
    }

    pub(crate) fn record_failure(&self, task_type: &str) {
        let mut metrics = self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = metrics.failures.entry(task_type.to_owned()).or_default();
        *count = count.saturating_add(1);
    }

    fn snapshot(&self) -> TaskMetricsSnapshot {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn counts(&self, task_type: &str) -> (u64, u64) {
        let snapshot = self.snapshot();
        (
            snapshot.execution.get(task_type).map_or(0, |timer| timer.count),
            snapshot.failures.get(task_type).copied().unwrap_or(0),
        )
    }
}

#[derive(Serialize)]
pub(crate) struct MetricNames {
    names: &'static [&'static str],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetricDto {
    name: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_unit: Option<String>,
    measurements: Vec<MeasurementDto>,
    available_tags: Vec<AvailableTagDto>,
}

#[derive(Serialize)]
struct MeasurementDto {
    statistic: &'static str,
    value: f64,
}

#[derive(Serialize)]
struct AvailableTagDto {
    tag: &'static str,
    values: Vec<String>,
}

pub(crate) async fn list(Extension(auth): Extension<AuthContext>) -> Result<Json<MetricNames>, ApiError> {
    require_admin(&auth)?;
    Ok(Json(MetricNames { names: &METRIC_NAMES }))
}

pub(crate) async fn get(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(name): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<MetricDto>, ApiError> {
    require_admin(&auth)?;
    let tags = parse_tags(raw_query.as_deref())?;
    let metric = match name.as_str() {
        "komga.libraries" => {
            require_no_tags(&name, &tags)?;
            let snapshot = state.storage.content_metrics_snapshot().await?;
            scalar_metric(&name, "The number of libraries", "count", snapshot.libraries as f64)
        }
        "komga.collections" => {
            require_no_tags(&name, &tags)?;
            let snapshot = state.storage.content_metrics_snapshot().await?;
            scalar_metric(&name, "The number of collections", "count", snapshot.collections as f64)
        }
        "komga.readlists" => {
            require_no_tags(&name, &tags)?;
            let snapshot = state.storage.content_metrics_snapshot().await?;
            scalar_metric(&name, "The number of readlists", "count", snapshot.read_lists as f64)
        }
        "komga.series" | "komga.books" | "komga.books.filesize" | "komga.sidecars" => {
            let snapshot = state.storage.content_metrics_snapshot().await?;
            let (description, base_unit, values) = match name.as_str() {
                "komga.series" => ("The number of series", "count", snapshot.series_by_library),
                "komga.books" => ("The number of books", "count", snapshot.books_by_library),
                "komga.books.filesize" => (
                    "The cumulated filesize of books",
                    "bytes",
                    snapshot.book_file_size_by_library,
                ),
                _ => ("The number of sidecars", "count", snapshot.sidecars_by_library),
            };
            grouped_metric(&name, description, base_unit, "library", values, &tags)?
        }
        "komga.tasks.execution" => task_execution_metric(&name, &state.task_metrics.snapshot(), &tags)?,
        "komga.tasks.failure" => task_failure_metric(&name, &state.task_metrics.snapshot(), &tags)?,
        _ => return Err(metric_not_found(&name)),
    };
    Ok(Json(metric))
}

fn parse_tags(raw_query: Option<&str>) -> Result<Vec<(String, String)>, ApiError> {
    url::form_urlencoded::parse(raw_query.unwrap_or_default().as_bytes())
        .filter(|(key, _)| key == "tag")
        .map(|(_, value)| {
            let (key, value) = value
                .split_once(':')
                .filter(|(key, value)| !key.is_empty() && !value.is_empty())
                .ok_or_else(|| ApiError::bad_request("tag must use the key:value format"))?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect()
}

fn require_no_tags(name: &str, tags: &[(String, String)]) -> Result<(), ApiError> {
    if tags.is_empty() {
        Ok(())
    } else {
        Err(metric_not_found(name))
    }
}

fn scalar_metric(name: &str, description: &str, base_unit: &str, value: f64) -> MetricDto {
    MetricDto {
        name: name.to_owned(),
        description: description.to_owned(),
        base_unit: Some(base_unit.to_owned()),
        measurements: vec![MeasurementDto {
            statistic: "VALUE",
            value,
        }],
        available_tags: Vec::new(),
    }
}

fn grouped_metric(
    name: &str,
    description: &str,
    base_unit: &str,
    tag_name: &'static str,
    values: BTreeMap<String, i64>,
    tags: &[(String, String)],
) -> Result<MetricDto, ApiError> {
    let value = selected_group_value(name, tag_name, &values, tags)?;
    let available_tags = if tags.is_empty() && !values.is_empty() {
        vec![AvailableTagDto {
            tag: tag_name,
            values: values.keys().cloned().collect(),
        }]
    } else {
        Vec::new()
    };
    Ok(MetricDto {
        name: name.to_owned(),
        description: description.to_owned(),
        base_unit: Some(base_unit.to_owned()),
        measurements: vec![MeasurementDto {
            statistic: "VALUE",
            value,
        }],
        available_tags,
    })
}

fn selected_group_value(
    metric_name: &str,
    tag_name: &str,
    values: &BTreeMap<String, i64>,
    tags: &[(String, String)],
) -> Result<f64, ApiError> {
    match tags {
        [] => Ok(values.values().sum::<i64>() as f64),
        [(key, value)] if key == tag_name => values
            .get(value)
            .map(|value| *value as f64)
            .ok_or_else(|| metric_not_found(metric_name)),
        _ => Err(metric_not_found(metric_name)),
    }
}

fn task_execution_metric(
    name: &str,
    snapshot: &TaskMetricsSnapshot,
    tags: &[(String, String)],
) -> Result<MetricDto, ApiError> {
    let timer = select_timer(name, &snapshot.execution, tags)?;
    let available_tags = task_available_tags(&snapshot.execution, tags);
    Ok(MetricDto {
        name: name.to_owned(),
        description: "Task execution time".into(),
        base_unit: Some("seconds".into()),
        measurements: vec![
            MeasurementDto {
                statistic: "COUNT",
                value: timer.count as f64,
            },
            MeasurementDto {
                statistic: "TOTAL_TIME",
                value: timer.total_time,
            },
            MeasurementDto {
                statistic: "MAX",
                value: timer.max,
            },
        ],
        available_tags,
    })
}

fn select_timer(
    name: &str,
    values: &BTreeMap<String, TimerValue>,
    tags: &[(String, String)],
) -> Result<TimerValue, ApiError> {
    match tags {
        [] => Ok(values.values().fold(TimerValue::default(), |mut total, value| {
            total.count = total.count.saturating_add(value.count);
            total.total_time += value.total_time;
            total.max = total.max.max(value.max);
            total
        })),
        [(key, value)] if key == "type" => values.get(value).copied().ok_or_else(|| metric_not_found(name)),
        _ => Err(metric_not_found(name)),
    }
}

fn task_failure_metric(
    name: &str,
    snapshot: &TaskMetricsSnapshot,
    tags: &[(String, String)],
) -> Result<MetricDto, ApiError> {
    let count = match tags {
        [] => snapshot.failures.values().copied().sum::<u64>(),
        [(key, value)] if key == "type" => snapshot
            .failures
            .get(value)
            .copied()
            .ok_or_else(|| metric_not_found(name))?,
        _ => return Err(metric_not_found(name)),
    };
    Ok(MetricDto {
        name: name.to_owned(),
        description: "Count of failed tasks".into(),
        base_unit: None,
        measurements: vec![MeasurementDto {
            statistic: "COUNT",
            value: count as f64,
        }],
        available_tags: if tags.is_empty() && !snapshot.failures.is_empty() {
            vec![AvailableTagDto {
                tag: "type",
                values: snapshot.failures.keys().cloned().collect(),
            }]
        } else {
            Vec::new()
        },
    })
}

fn task_available_tags(values: &BTreeMap<String, TimerValue>, tags: &[(String, String)]) -> Vec<AvailableTagDto> {
    if tags.is_empty() && !values.is_empty() {
        vec![AvailableTagDto {
            tag: "type",
            values: values.keys().cloned().collect(),
        }]
    } else {
        Vec::new()
    }
}

fn metric_not_found(name: &str) -> ApiError {
    ApiError::not_found(format!("No metric found with name {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_metrics_aggregate_and_filter_by_type() {
        let metrics = TaskMetrics::default();
        metrics.record_execution("ScanLibrary", Duration::from_millis(1_500));
        metrics.record_execution("ScanLibrary", Duration::from_millis(500));
        metrics.record_execution("AnalyzeBook", Duration::from_secs(3));
        metrics.record_failure("AnalyzeBook");

        let snapshot = metrics.snapshot();
        let all = task_execution_metric("komga.tasks.execution", &snapshot, &[]).unwrap();
        assert_eq!(all.measurements[0].value, 3.0);
        assert_eq!(all.measurements[1].value, 5.0);
        assert_eq!(all.measurements[2].value, 3.0);
        assert_eq!(all.available_tags[0].values, ["AnalyzeBook", "ScanLibrary"]);

        let scan = task_execution_metric(
            "komga.tasks.execution",
            &snapshot,
            &[("type".into(), "ScanLibrary".into())],
        )
        .unwrap();
        assert_eq!(scan.measurements[0].value, 2.0);
        assert_eq!(scan.measurements[1].value, 2.0);
        assert_eq!(scan.measurements[2].value, 1.5);

        let failures = task_failure_metric("komga.tasks.failure", &snapshot, &[]).unwrap();
        assert_eq!(failures.measurements[0].value, 1.0);
        assert_eq!(failures.available_tags[0].values, ["AnalyzeBook"]);
    }

    #[test]
    fn metric_tags_require_key_value_and_known_values() {
        assert!(parse_tags(Some("tag=library%3Aone")).is_ok());
        assert!(parse_tags(Some("tag=library")).is_err());
        assert!(
            selected_group_value(
                "komga.books",
                "library",
                &BTreeMap::from([("one".into(), 2)]),
                &[("library".into(), "missing".into())],
            )
            .is_err()
        );
    }
}
