//! Administrative analysis of import candidates outside configured libraries.

use std::{
    fs,
    path::{Path as FilePath, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use axum::{
    Json,
    body::Body,
    extract::{Extension, Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Local, NaiveDateTime};
use komga_core::{Book, Media, MediaStatus, new_id};
use komga_media::{
    MediaAnalysisError, MediaOperationError, MobipocketSource, analyze_book, analyze_materialized_mobipocket,
    extract_comic_info_metadata, extract_epub_metadata, extract_page, page_output_dimension, page_output_media_type,
};
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use url::Url;
use walkdir::WalkDir;

use crate::{AppState, auth::AuthContext, dto::PageDto, error::ApiError, routes::require_admin};

const CACHE_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const CACHE_CAPACITY: u64 = 10_000;

#[derive(Clone)]
pub(crate) struct TransientBookCache {
    cache: Cache<String, Arc<TransientBook>>,
}

impl TransientBookCache {
    pub(crate) fn new() -> Self {
        Self::with_idle_timeout(CACHE_IDLE_TIMEOUT)
    }

    fn with_idle_timeout(idle_timeout: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_idle(idle_timeout)
                .build(),
        }
    }

    async fn get(&self, id: &str) -> Option<Arc<TransientBook>> {
        self.cache.get(id).await
    }

    async fn insert(&self, book: TransientBook) {
        self.cache.insert(book.book.id.clone(), Arc::new(book)).await;
    }
}

#[derive(Debug, Clone)]
struct TransientBook {
    book: Book,
    media: Media,
    number: Option<f32>,
    series_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScanRequest {
    path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransientBookDto {
    id: String,
    name: String,
    url: String,
    file_last_modified: NaiveDateTime,
    size_bytes: i64,
    size: String,
    status: String,
    media_type: String,
    pages: Vec<PageDto>,
    files: Vec<String>,
    comment: String,
    number: Option<f32>,
    series_id: Option<String>,
}

pub(crate) async fn scan(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(request): Json<ScanRequest>,
) -> Result<Json<Vec<TransientBookDto>>, ApiError> {
    require_admin(&auth)?;
    let root = validate_scan_root(&request.path, &state).await?;
    let books = tokio::task::spawn_blocking(move || scan_directory(&root))
        .await
        .map_err(|error| {
            tracing::error!(%error, "transient book scanner panicked");
            ApiError::internal("Could not scan import directory")
        })?;
    let mut result = Vec::with_capacity(books.len());
    for book in books {
        let transient = TransientBook {
            media: Media::result(&book.id, MediaStatus::Unknown, None, None),
            book,
            number: None,
            series_id: None,
        };
        result.push(to_dto(&transient));
        state.transient_books.insert(transient).await;
    }
    Ok(Json(result))
}

pub(crate) async fn analyze(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<TransientBookDto>, ApiError> {
    require_admin(&auth)?;
    let transient = state
        .transient_books
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found("Transient book not found"))?;
    let book = transient.book.clone();
    ensure_source_file(&book)?;
    let (media, metadata) = if let Some(source) = mobipocket_source(&book) {
        let source_media_type = source.media_type();
        match state.mobi.get_or_convert(&book, &state.shutdown).await {
            Ok(converted) => {
                let converted_path = converted.path.clone();
                let mut materialized_book = book.clone();
                materialized_book.url = Url::from_file_path(&converted_path)
                    .map_err(|()| ApiError::internal("Could not analyze transient book"))?;
                let book_id = book.id.clone();
                tokio::task::spawn_blocking(move || {
                    let media = analyze_materialized_mobipocket(&converted_path, &book_id, source, true, || false)?;
                    let metadata = embedded_metadata(&materialized_book, &media);
                    Ok::<_, MediaAnalysisError>((media, metadata))
                })
                .await
                .map_err(|error| {
                    tracing::error!(%error, %id, "transient MOBI/AZW analyzer panicked");
                    ApiError::internal("Could not analyze transient book")
                })?
                .map_err(|MediaAnalysisError::Cancelled| ApiError::internal("Could not analyze transient book"))?
            }
            Err(crate::mobi::MaterializationError::Drm) => (
                Media::result(
                    &book.id,
                    MediaStatus::Unsupported,
                    Some(source_media_type.to_owned()),
                    Some("ERR_1040"),
                ),
                CandidateMetadata::default(),
            ),
            Err(crate::mobi::MaterializationError::Cancelled) => {
                return Err(ApiError::internal("Could not analyze transient book"));
            }
            Err(error) => {
                tracing::warn!(%error, %id, "could not materialize transient MOBI/AZW");
                (
                    Media::result(
                        &book.id,
                        MediaStatus::Error,
                        Some(source_media_type.to_owned()),
                        Some("ERR_1041"),
                    ),
                    CandidateMetadata::default(),
                )
            }
        }
    } else {
        let analyzed_book = book.clone();
        tokio::task::spawn_blocking(move || {
            let media = analyze_book(&analyzed_book, true, || false)?;
            let metadata = embedded_metadata(&analyzed_book, &media);
            Ok::<_, MediaAnalysisError>((media, metadata))
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %id, "transient book analyzer panicked");
            ApiError::internal("Could not analyze transient book")
        })?
        .map_err(|MediaAnalysisError::Cancelled| ApiError::internal("Could not analyze transient book"))?
    };
    let series_id = match_series(&state, &metadata.series_names).await?;
    let updated = TransientBook {
        book,
        media,
        number: metadata.number,
        series_id,
    };
    let dto = to_dto(&updated);
    state.transient_books.insert(updated).await;
    Ok(Json(dto))
}

pub(crate) async fn page(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((id, page)): Path<(String, i32)>,
) -> Result<Response, ApiError> {
    require_admin(&auth)?;
    let transient = state
        .transient_books
        .get(&id)
        .await
        .ok_or_else(|| ApiError::not_found("Transient book not found"))?;
    ensure_source_file(&transient.book)?;
    if page <= 0 {
        return Err(ApiError::bad_request("Page number does not exist"));
    }
    if transient.media.status != MediaStatus::Ready {
        return Err(ApiError::not_found("Book analysis failed"));
    }
    if usize::try_from(page).map_or(true, |page| page > transient.media.pages.len()) {
        return Err(ApiError::bad_request("Page number does not exist"));
    }
    let book = transient.book.clone();
    let media = transient.media.clone();
    let content = tokio::task::spawn_blocking(move || extract_page(&book, &media, page))
        .await
        .map_err(|error| {
            tracing::error!(%error, %id, "transient page extractor panicked");
            ApiError::internal("Could not read transient book page")
        })?
        .map_err(map_page_error)?;
    let mut response = Body::from(content.bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content.media_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    Ok(response)
}

async fn validate_scan_root(path: &str, state: &AppState) -> Result<PathBuf, ApiError> {
    let requested = PathBuf::from(path);
    let metadata = fs::symlink_metadata(&requested).map_err(|_| invalid_scan_path())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || fs::read_dir(&requested).is_err() {
        return Err(invalid_scan_path());
    }
    let root = fs::canonicalize(requested).map_err(|_| invalid_scan_path())?;
    for library in state.storage.list_libraries().await? {
        let Some(library_root) = library.root_path().and_then(|path| fs::canonicalize(path).ok()) else {
            continue;
        };
        if root.starts_with(&library_root) || library_root.starts_with(&root) {
            return Err(invalid_scan_path());
        }
    }
    Ok(root)
}

fn invalid_scan_path() -> ApiError {
    ApiError::bad_request("ERR_1017")
}

fn scan_directory(root: &FilePath) -> Vec<Book> {
    let mut books = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !is_hidden(entry.path()))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && is_supported(entry.path()))
        .filter_map(|entry| transient_book(entry.path()))
        .collect::<Vec<_>>();
    books.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
    books
}

fn transient_book(path: &FilePath) -> Option<Book> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || fs::File::open(path).is_err() {
        return None;
    }
    let path = fs::canonicalize(path).ok()?;
    let url = Url::from_file_path(&path).ok()?;
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    Some(Book {
        id: new_id(),
        name: path.file_stem()?.to_string_lossy().into_owned(),
        url,
        file_last_modified: DateTime::<Local>::from(std::cmp::max(modified, created)).naive_local(),
        file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        file_hash: String::new(),
        file_hash_koreader: String::new(),
        number: 0,
        series_id: String::new(),
        library_id: String::new(),
        deleted_date: None,
        oneshot: false,
        created_date: None,
        last_modified_date: None,
    })
}

fn is_hidden(path: &FilePath) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn is_supported(path: &FilePath) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "cbz" | "zip" | "cbr" | "rar" | "pdf" | "epub" | "mobi" | "azw" | "azw3"
    )
}

fn mobipocket_source(book: &Book) -> Option<MobipocketSource> {
    MobipocketSource::from_path(&book.url.to_file_path().ok()?)
}

fn ensure_source_file(book: &Book) -> Result<(), ApiError> {
    let path = book
        .url
        .to_file_path()
        .map_err(|()| ApiError::not_found("File not found, it may have moved"))?;
    let metadata = fs::symlink_metadata(path).map_err(|_| ApiError::not_found("File not found, it may have moved"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ApiError::not_found("File not found, it may have moved"));
    }
    Ok(())
}

#[derive(Default)]
struct CandidateMetadata {
    number: Option<f32>,
    series_names: Vec<String>,
}

fn embedded_metadata(book: &Book, media: &Media) -> CandidateMetadata {
    let comic_plain = extract_comic_info_metadata(book, media, false).ok().flatten();
    let comic_volume = extract_comic_info_metadata(book, media, true).ok().flatten();
    let epub = extract_epub_metadata(book, media).ok().flatten();
    let number = comic_plain
        .as_ref()
        .and_then(|metadata| metadata.book.number_sort)
        .or_else(|| epub.as_ref().and_then(|metadata| metadata.book.number_sort));
    let mut series_names = Vec::new();
    for name in [
        comic_volume.and_then(|metadata| metadata.series.title),
        comic_plain.and_then(|metadata| metadata.series.title),
        epub.and_then(|metadata| metadata.series.title),
    ]
    .into_iter()
    .flatten()
    {
        let name = name.trim();
        if !name.is_empty() && !series_names.iter().any(|existing| existing == name) {
            series_names.push(name.to_owned());
        }
    }
    CandidateMetadata { number, series_names }
}

async fn match_series(state: &AppState, names: &[String]) -> Result<Option<String>, ApiError> {
    if names.is_empty() {
        return Ok(None);
    }
    let names = names.iter().map(|name| name.to_lowercase()).collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for library in state.storage.list_libraries().await? {
        for series in state.storage.list_series_by_library_id(&library.id).await? {
            if series.deleted_date.is_some() {
                continue;
            }
            let Some(metadata) = state.storage.find_series_metadata(&series.id).await? else {
                continue;
            };
            candidates.push((metadata.title.to_lowercase(), series.id));
        }
    }
    candidates.sort();
    Ok(candidates
        .iter()
        .find(|(title, _)| names.iter().any(|name| title == name))
        .or_else(|| {
            candidates
                .iter()
                .find(|(title, _)| names.iter().any(|name| title.contains(name)))
        })
        .map(|(_, id)| id.clone()))
}

fn to_dto(transient: &TransientBook) -> TransientBookDto {
    let pages = transient
        .media
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let dimension = page_output_dimension(&transient.media, page);
            PageDto {
                number: i32::try_from(index + 1).unwrap_or(i32::MAX),
                file_name: page.file_name.clone(),
                media_type: page_output_media_type(&transient.media, page).to_owned(),
                width: dimension.map(|value| value.width),
                height: dimension.map(|value| value.height),
                size_bytes: page.file_size,
                size: page.file_size.map(binary_size).unwrap_or_default(),
            }
        })
        .collect();
    TransientBookDto {
        id: transient.book.id.clone(),
        name: transient.book.name.clone(),
        url: transient
            .book
            .url
            .to_file_path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        file_last_modified: transient.book.file_last_modified,
        size_bytes: transient.book.file_size,
        size: binary_size(transient.book.file_size),
        status: transient.media.status.as_str().to_owned(),
        media_type: transient.media.media_type.clone().unwrap_or_default(),
        pages,
        files: transient
            .media
            .files
            .iter()
            .map(|file| file.file_name.clone())
            .collect(),
        comment: transient.media.comment.clone().unwrap_or_default(),
        number: transient.number,
        series_id: transient.series_id.clone(),
    }
}

fn binary_size(bytes: i64) -> String {
    if bytes < 0 {
        return String::new();
    }
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} {}", UNITS[unit]);
    }
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{} {}", rounded as i64, UNITS[unit])
    } else {
        format!("{rounded:.1} {}", UNITS[unit])
    }
}

fn map_page_error(error: MediaOperationError) -> ApiError {
    match error {
        MediaOperationError::PageOutOfBounds => ApiError::bad_request("Page number does not exist"),
        MediaOperationError::NotReady | MediaOperationError::UnsupportedProfile => {
            ApiError::not_found("Book analysis failed")
        }
        MediaOperationError::ResourceNotFound | MediaOperationError::Read(_) => {
            ApiError::not_found("File not found, it may have moved")
        }
        MediaOperationError::Image(_) => ApiError::internal("Could not render transient book page"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_expires_entries_after_the_idle_timeout() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("book.cbz");
        fs::write(&source, b"book").unwrap();
        let book = transient_book(&source).unwrap();
        let id = book.id.clone();
        let cache = TransientBookCache::with_idle_timeout(Duration::from_millis(10));
        cache
            .insert(TransientBook {
                media: Media::result(&id, MediaStatus::Unknown, None, None),
                book,
                number: None,
                series_id: None,
            })
            .await;
        assert!(cache.get(&id).await.is_some());
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(cache.get(&id).await.is_none());
    }
}
