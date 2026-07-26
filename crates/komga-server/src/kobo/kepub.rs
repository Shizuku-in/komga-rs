//! External kepubify execution and temporary converted-file caching.

use std::{
    ffi::OsStr,
    fmt,
    fs::File as StdFile,
    hash::Hash,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use chrono::NaiveDateTime;
use komga_core::Book;
use moka::future::Cache;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{process::Command, time::timeout};
use tokio_util::sync::CancellationToken;

const VALIDATION_TIMEOUT: Duration = Duration::from_secs(3);
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DIAGNOSTIC_LIMIT: u64 = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    book_id: String,
    file_last_modified: NaiveDateTime,
}

/// One converted file whose temporary directory lives as long as this value.
pub(crate) struct CachedKepub {
    _directory: TempDir,
    pub(crate) path: PathBuf,
    pub(crate) length: u64,
}

impl fmt::Debug for CachedKepub {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedKepub")
            .field("path", &self.path)
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

/// Failures while invoking the configured KEPUB converter.
#[derive(Debug, Clone, Error)]
pub(crate) enum KepubConversionError {
    #[error("Kepub conversion is not available")]
    Unavailable,
    #[error("book URL is not a local file")]
    InvalidSource,
    #[error("could not prepare KEPUB conversion: {0}")]
    Prepare(String),
    #[error("could not start kepubify: {0}")]
    Spawn(String),
    #[error("kepubify conversion timed out")]
    Timeout,
    #[error("kepubify conversion was cancelled")]
    Cancelled,
    #[error("kepubify exited unsuccessfully{0}")]
    Exit(String),
    #[error("kepubify did not produce a converted file")]
    MissingOutput,
}

/// Cloneable process service shared by media analysis and Kobo downloads.
#[derive(Clone)]
pub(crate) struct KepubService {
    executable: Option<Arc<PathBuf>>,
    cache: Cache<CacheKey, Arc<CachedKepub>>,
    conversion_timeout: Duration,
}

impl KepubService {
    pub(crate) fn disabled() -> Self {
        Self {
            executable: None,
            cache: Cache::builder().time_to_idle(CACHE_IDLE_TIMEOUT).build(),
            conversion_timeout: CONVERSION_TIMEOUT,
        }
    }

    pub(crate) async fn configure(path: Option<PathBuf>) -> Self {
        Self::configure_with_timeouts(path, VALIDATION_TIMEOUT, CONVERSION_TIMEOUT).await
    }

    async fn configure_with_timeouts(
        path: Option<PathBuf>,
        validation_timeout: Duration,
        conversion_timeout: Duration,
    ) -> Self {
        let Some(path) = path else {
            tracing::info!("Kepub conversion unavailable: kepubify path is not set");
            return Self::disabled();
        };
        let executable = if executable_file(&path) || command_succeeds(&path, validation_timeout).await {
            tracing::info!(path = %path.display(), "Kepub conversion available");
            Some(Arc::new(path))
        } else {
            tracing::warn!(path = %path.display(), "Kepub conversion unavailable: kepubify path is invalid");
            None
        };
        Self {
            executable,
            cache: Cache::builder().time_to_idle(CACHE_IDLE_TIMEOUT).build(),
            conversion_timeout,
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        self.executable.is_some()
    }

    pub(crate) async fn get_or_convert(
        &self,
        book: &Book,
        shutdown: &CancellationToken,
    ) -> Result<Arc<CachedKepub>, KepubConversionError> {
        let executable = self.executable.clone().ok_or(KepubConversionError::Unavailable)?;
        let key = CacheKey {
            book_id: book.id.clone(),
            file_last_modified: book.file_last_modified,
        };
        let book = book.clone();
        let shutdown = shutdown.clone();
        let conversion_timeout = self.conversion_timeout;
        self.cache
            .try_get_with(key, async move {
                convert(executable.as_ref(), &book, &shutdown, conversion_timeout)
                    .await
                    .map(Arc::new)
            })
            .await
            .map_err(|error| (*error).clone())
    }
}

async fn command_succeeds(command: &Path, duration: Duration) -> bool {
    let Ok(mut child) = Command::new(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    else {
        return false;
    };
    match timeout(duration, child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.kill().await;
            false
        }
    }
}

async fn convert(
    executable: &Path,
    book: &Book,
    shutdown: &CancellationToken,
    conversion_timeout: Duration,
) -> Result<CachedKepub, KepubConversionError> {
    let source = book
        .url
        .to_file_path()
        .map_err(|()| KepubConversionError::InvalidSource)?;
    let directory = tempfile::Builder::new()
        .prefix("komga-kepub-")
        .tempdir()
        .map_err(|error| KepubConversionError::Prepare(error.to_string()))?;
    let stem = source
        .file_stem()
        .unwrap_or_else(|| OsStr::new("book"))
        .to_string_lossy();
    let destination = directory.path().join(format!("{stem}.kepub.epub"));
    let diagnostic_path = directory.path().join("kepubify.stderr.log");
    let diagnostic =
        StdFile::create(&diagnostic_path).map_err(|error| KepubConversionError::Prepare(error.to_string()))?;
    let mut child = Command::new(executable)
        .arg(&source)
        .arg("-o")
        .arg(&destination)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(diagnostic))
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| KepubConversionError::Spawn(error.to_string()))?;

    let status = tokio::select! {
        () = shutdown.cancelled() => {
            let _ = child.kill().await;
            return Err(KepubConversionError::Cancelled);
        }
        result = timeout(conversion_timeout, child.wait()) => match result {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(KepubConversionError::Spawn(error.to_string())),
            Err(_) => {
                let _ = child.kill().await;
                return Err(KepubConversionError::Timeout);
            }
        }
    };
    if !status.success() {
        let diagnostic = read_diagnostic(&diagnostic_path);
        let suffix = if diagnostic.is_empty() {
            String::new()
        } else {
            format!(": {diagnostic}")
        };
        return Err(KepubConversionError::Exit(suffix));
    }
    let metadata = std::fs::metadata(&destination).map_err(|_| KepubConversionError::MissingOutput)?;
    if !metadata.is_file() {
        return Err(KepubConversionError::MissingOutput);
    }
    Ok(CachedKepub {
        _directory: directory,
        path: destination,
        length: metadata.len(),
    })
}

fn read_diagnostic(path: &Path) -> String {
    let Ok(file) = StdFile::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = file.take(DIAGNOSTIC_LIMIT).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use chrono::Local;
    use url::Url;

    use super::*;

    fn book(path: &Path) -> Book {
        Book {
            id: "book".into(),
            name: "Book".into(),
            url: Url::from_file_path(path).unwrap(),
            file_last_modified: Local::now().naive_local(),
            file_size: i64::try_from(fs::metadata(path).unwrap().len()).unwrap(),
            file_hash: String::new(),
            file_hash_koreader: String::new(),
            number: 1,
            series_id: "series".into(),
            library_id: "library".into(),
            deleted_date: None,
            oneshot: false,
            created_date: None,
            last_modified_date: None,
        }
    }

    fn script(directory: &Path, body: &str) -> PathBuf {
        let path = directory.join("kepubify");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[tokio::test]
    async fn converts_once_for_concurrent_and_repeated_cache_access() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        let count = temporary.path().join("count");
        fs::write(&source, b"epub").unwrap();
        let executable = script(
            temporary.path(),
            &format!("echo run >> '{}'; cp \"$1\" \"$3\"", count.display()),
        );
        let service = KepubService::configure(Some(executable)).await;
        let book = book(&source);
        let shutdown = CancellationToken::new();
        let (first, second) = tokio::join!(
            service.get_or_convert(&book, &shutdown),
            service.get_or_convert(&book, &shutdown)
        );
        let first = first.unwrap();
        let second = second.unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 1);
        assert_eq!(fs::read(&first.path).unwrap(), b"epub");
    }

    #[tokio::test]
    async fn conversion_failures_are_not_cached() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        let count = temporary.path().join("count");
        fs::write(&source, b"epub").unwrap();
        let executable = script(
            temporary.path(),
            &format!("echo run >> '{}'; echo failed >&2; exit 2", count.display()),
        );
        let service = KepubService::configure(Some(executable)).await;
        let book = book(&source);
        let shutdown = CancellationToken::new();

        assert!(service.get_or_convert(&book, &shutdown).await.is_err());
        assert!(service.get_or_convert(&book, &shutdown).await.is_err());
        assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 2);
    }

    #[tokio::test]
    async fn conversion_timeout_terminates_the_process() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        fs::write(&source, b"epub").unwrap();
        let executable = script(temporary.path(), "sleep 5");
        let service = KepubService::configure_with_timeouts(
            Some(executable),
            Duration::from_millis(50),
            Duration::from_millis(50),
        )
        .await;

        let error = service
            .get_or_convert(&book(&source), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, KepubConversionError::Timeout));
    }
}
