//! Shared temporary EPUB materialization for MOBI/AZW-backed server operations.

use std::{fmt, hash::Hash, path::PathBuf, sync::Arc, time::Duration};

use chrono::NaiveDateTime;
use komga_core::{AZW_MEDIA_TYPE, AZW3_MEDIA_TYPE, Book, MOBI_MEDIA_TYPE, Media};
use komga_media::{MobiConversionError, MobiVariant, convert_mobi_to_epub};
use moka::future::Cache;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::spawn_blocking,
};
use tokio_util::sync::CancellationToken;

const CACHE_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONVERSION_PERMITS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    book_id: String,
    file_last_modified: NaiveDateTime,
    file_size: i64,
}

/// A converted EPUB whose temporary directory is retained by the cache value.
pub(crate) struct CachedMobi {
    _directory: TempDir,
    pub(crate) path: PathBuf,
    pub(crate) length: u64,
    pub(crate) variant: MobiVariant,
}

/// A source-book view that keeps any cached MOBI/AZW conversion alive while used.
pub(crate) struct MaterializedBook {
    pub(crate) book: Book,
    _converted: Option<Arc<CachedMobi>>,
}

impl fmt::Debug for CachedMobi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedMobi")
            .field("path", &self.path)
            .field("length", &self.length)
            .field("variant", &self.variant)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Error)]
pub(crate) enum MaterializationError {
    #[error("book URL is not a local file")]
    InvalidSource,
    #[error("MOBI/AZW materialization was cancelled")]
    Cancelled,
    #[error("MOBI/AZW is protected with DRM")]
    Drm,
    #[error("MOBI/AZW conversion failed: {0}")]
    Conversion(String),
    #[error("MOBI/AZW conversion worker failed: {0}")]
    Worker(String),
}

/// Cloneable cache with a two-conversion global concurrency limit.
#[derive(Clone)]
pub(crate) struct MobiService {
    cache: Cache<CacheKey, Arc<CachedMobi>>,
    conversion_limit: Arc<Semaphore>,
}

impl MobiService {
    pub(crate) fn new() -> Self {
        Self {
            cache: Cache::builder().time_to_idle(CACHE_IDLE_TIMEOUT).build(),
            conversion_limit: Arc::new(Semaphore::new(CONVERSION_PERMITS)),
        }
    }

    pub(crate) async fn get_or_convert(
        &self,
        book: &Book,
        shutdown: &CancellationToken,
    ) -> Result<Arc<CachedMobi>, MaterializationError> {
        let key = CacheKey {
            book_id: book.id.clone(),
            file_last_modified: book.file_last_modified,
            file_size: book.file_size,
        };
        let book = book.clone();
        let shutdown = shutdown.clone();
        let conversion_limit = self.conversion_limit.clone();
        self.cache
            .try_get_with(key, async move {
                let permit = acquire_permit(conversion_limit, &shutdown).await?;
                materialize(book, shutdown, permit).await.map(Arc::new)
            })
            .await
            .map_err(|error| (*error).clone())
    }

    pub(crate) async fn materialize_book(
        &self,
        book: &Book,
        media: &Media,
        shutdown: &CancellationToken,
    ) -> Result<MaterializedBook, MaterializationError> {
        if !matches!(
            media.media_type.as_deref(),
            Some(MOBI_MEDIA_TYPE | AZW_MEDIA_TYPE | AZW3_MEDIA_TYPE)
        ) {
            return Ok(MaterializedBook {
                book: book.clone(),
                _converted: None,
            });
        }
        let converted = self.get_or_convert(book, shutdown).await?;
        let mut materialized = book.clone();
        materialized.url = url::Url::from_file_path(&converted.path)
            .map_err(|()| MaterializationError::Conversion("temporary EPUB path is invalid".to_owned()))?;
        Ok(MaterializedBook {
            book: materialized,
            _converted: Some(converted),
        })
    }
}

async fn acquire_permit(
    semaphore: Arc<Semaphore>,
    shutdown: &CancellationToken,
) -> Result<OwnedSemaphorePermit, MaterializationError> {
    tokio::select! {
        () = shutdown.cancelled() => Err(MaterializationError::Cancelled),
        permit = semaphore.acquire_owned() => permit.map_err(|error| MaterializationError::Conversion(error.to_string())),
    }
}

async fn materialize(
    book: Book,
    shutdown: CancellationToken,
    _permit: OwnedSemaphorePermit,
) -> Result<CachedMobi, MaterializationError> {
    let source = book
        .url
        .to_file_path()
        .map_err(|()| MaterializationError::InvalidSource)?;
    let directory = tempfile::Builder::new()
        .prefix("komga-mobi-")
        .tempdir()
        .map_err(|error| MaterializationError::Conversion(error.to_string()))?;
    let destination = directory.path().join("materialized.epub");
    let worker_destination = destination.clone();
    let worker_shutdown = shutdown.clone();
    let variant =
        spawn_blocking(move || convert_mobi_to_epub(&source, &worker_destination, || worker_shutdown.is_cancelled()))
            .await
            .map_err(|error| MaterializationError::Worker(error.to_string()))?
            .map_err(map_conversion_error)?;
    if shutdown.is_cancelled() {
        return Err(MaterializationError::Cancelled);
    }
    let length = std::fs::metadata(&destination)
        .map_err(|error| MaterializationError::Conversion(error.to_string()))?
        .len();
    Ok(CachedMobi {
        _directory: directory,
        path: destination,
        length,
        variant,
    })
}

fn map_conversion_error(error: MobiConversionError) -> MaterializationError {
    match error {
        MobiConversionError::Cancelled => MaterializationError::Cancelled,
        MobiConversionError::Drm => MaterializationError::Drm,
        error => MaterializationError::Conversion(error.to_string()),
    }
}
