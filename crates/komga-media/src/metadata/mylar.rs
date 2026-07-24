use komga_core::{MetadataUpdate, Series, SeriesMetadataPatch, SeriesStatus};

use crate::MediaOperationError;

use super::comic_age_rating;

/// Parses a Mylar `series.json` sidecar from a non-one-shot series root.
pub fn extract_mylar_metadata(series: &Series) -> Result<Option<SeriesMetadataPatch>, MediaOperationError> {
    if series.oneshot {
        return Ok(None);
    }
    let directory = series
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("series URL is not a file URL".into()))?;
    let path = directory.join("series.json");
    if !path.is_file() {
        return Ok(None);
    }

    #[derive(serde::Deserialize)]
    struct Mylar {
        #[serde(default, alias = "series", alias = "title")]
        name: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default, alias = "description_text", alias = "summary")]
        description: Option<String>,
        #[serde(default)]
        publisher: Option<String>,
        #[serde(default, alias = "content_rating")]
        age_rating: Option<serde_json::Value>,
        #[serde(default, alias = "total_issues", alias = "issue_count")]
        total_book_count: Option<i32>,
    }

    let bytes = std::fs::read(path).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let value: Mylar = serde_json::from_slice(&bytes).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let status = value
        .status
        .as_deref()
        .and_then(|value| match value.to_ascii_lowercase().as_str() {
            "ended" | "complete" | "completed" => Some(SeriesStatus::Ended),
            "continuing" | "ongoing" => Some(SeriesStatus::Ongoing),
            "hiatus" => Some(SeriesStatus::Hiatus),
            "abandoned" | "cancelled" => Some(SeriesStatus::Abandoned),
            _ => None,
        });
    let age_rating = value.age_rating.and_then(|value| match value {
        serde_json::Value::Number(number) => number.as_i64().and_then(|number| i32::try_from(number).ok()),
        serde_json::Value::String(value) => comic_age_rating(&value),
        _ => None,
    });

    Ok(Some(SeriesMetadataPatch {
        status,
        title: value.name.filter(|value| !value.trim().is_empty()),
        summary: value.description,
        publisher: value.publisher,
        age_rating: age_rating.map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        total_book_count: value
            .total_book_count
            .filter(|value| *value > 0)
            .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        ..Default::default()
    }))
}
