//! In-process KEPUB conversion with an optional external compatibility fallback.

use std::{
    ffi::OsStr,
    fmt,
    fs::File as StdFile,
    hash::Hash,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, RwLock},
    time::Duration,
};

use chrono::NaiveDateTime;
use komga_core::Book;
use komga_media::{convert_epub_to_kepub, validate_kepub_conversion};
use moka::future::Cache;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    process::Command,
    sync::{OwnedSemaphorePermit, Semaphore},
    task::spawn_blocking,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

const VALIDATION_TIMEOUT: Duration = Duration::from_secs(3);
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(10);
const CACHE_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DIAGNOSTIC_LIMIT: u64 = 8 * 1024;
const CONVERSION_PERMITS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    book_id: String,
    file_last_modified: NaiveDateTime,
    file_size: i64,
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

/// Failures while producing a validated temporary KEPUB.
#[derive(Debug, Clone, Error)]
pub(crate) enum KepubConversionError {
    #[error("book URL is not a local file")]
    InvalidSource,
    #[error("could not prepare KEPUB conversion: {0}")]
    Prepare(String),
    #[error("in-process KEPUB conversion failed: {0}")]
    BuiltIn(String),
    #[error("in-process KEPUB conversion worker failed: {0}")]
    Worker(String),
    #[error("could not start kepubify fallback: {0}")]
    Spawn(String),
    #[error("kepubify fallback timed out")]
    Timeout,
    #[error("KEPUB conversion was cancelled")]
    Cancelled,
    #[error("kepubify fallback exited unsuccessfully{0}")]
    Exit(String),
    #[error("kepubify fallback did not produce a converted file")]
    MissingOutput,
    #[error("generated KEPUB failed validation: {0}")]
    Validation(String),
}

#[derive(Clone)]
struct ExternalConfiguration {
    executable: Option<Arc<PathBuf>>,
    effective_path: Option<PathBuf>,
}

/// Cloneable conversion service shared by media analysis and Kobo downloads.
#[derive(Clone)]
pub(crate) struct KepubService {
    external: Arc<RwLock<ExternalConfiguration>>,
    cache: Cache<CacheKey, Arc<CachedKepub>>,
    conversion_limit: Arc<Semaphore>,
    conversion_timeout: Duration,
}

impl KepubService {
    pub(crate) async fn configure(path: Option<PathBuf>) -> Self {
        Self::configure_with_timeouts(path, VALIDATION_TIMEOUT, CONVERSION_TIMEOUT).await
    }

    async fn configure_with_timeouts(
        path: Option<PathBuf>,
        validation_timeout: Duration,
        conversion_timeout: Duration,
    ) -> Self {
        Self {
            external: Arc::new(RwLock::new(
                ExternalConfiguration::configure(path, validation_timeout).await,
            )),
            cache: Cache::builder().time_to_idle(CACHE_IDLE_TIMEOUT).build(),
            conversion_limit: Arc::new(Semaphore::new(CONVERSION_PERMITS)),
            conversion_timeout,
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        true
    }

    pub(crate) fn effective_path(&self) -> Option<PathBuf> {
        self.external
            .read()
            .expect("kepub configuration lock poisoned")
            .effective_path
            .clone()
    }

    pub(crate) async fn reconfigure(&self, database_path: Option<PathBuf>, fallback_path: Option<PathBuf>) {
        let mut configuration = ExternalConfiguration::configure(database_path, VALIDATION_TIMEOUT).await;
        if configuration.executable.is_none() && fallback_path.is_some() {
            configuration = ExternalConfiguration::configure(fallback_path, VALIDATION_TIMEOUT).await;
        }
        *self.external.write().expect("kepub configuration lock poisoned") = configuration;
    }

    pub(crate) async fn get_or_convert(
        &self,
        book: &Book,
        shutdown: &CancellationToken,
    ) -> Result<Arc<CachedKepub>, KepubConversionError> {
        let executable = self
            .external
            .read()
            .expect("kepub configuration lock poisoned")
            .executable
            .clone();
        let key = CacheKey {
            book_id: book.id.clone(),
            file_last_modified: book.file_last_modified,
            file_size: book.file_size,
        };
        let book = book.clone();
        let shutdown = shutdown.clone();
        let conversion_limit = self.conversion_limit.clone();
        let conversion_timeout = self.conversion_timeout;
        self.cache
            .try_get_with(key, async move {
                let permit = acquire_permit(conversion_limit, &shutdown).await?;
                convert(executable, &book, &shutdown, conversion_timeout, permit)
                    .await
                    .map(Arc::new)
            })
            .await
            .map_err(|error| (*error).clone())
    }
}

impl ExternalConfiguration {
    fn disabled() -> Self {
        Self {
            executable: None,
            effective_path: None,
        }
    }

    async fn configure(path: Option<PathBuf>, validation_timeout: Duration) -> Self {
        let Some(path) = path else {
            tracing::info!("External kepubify fallback is not configured");
            return Self::disabled();
        };
        if executable_file(&path) || command_succeeds(&path, validation_timeout).await {
            tracing::info!(path = %path.display(), "External kepubify fallback is available");
            Self {
                executable: Some(Arc::new(path.clone())),
                effective_path: Some(path),
            }
        } else {
            tracing::warn!(path = %path.display(), "External kepubify fallback path is invalid");
            Self::disabled()
        }
    }
}

async fn acquire_permit(
    semaphore: Arc<Semaphore>,
    shutdown: &CancellationToken,
) -> Result<OwnedSemaphorePermit, KepubConversionError> {
    tokio::select! {
        () = shutdown.cancelled() => Err(KepubConversionError::Cancelled),
        permit = semaphore.acquire_owned() => permit.map_err(|error| KepubConversionError::Prepare(error.to_string())),
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
    executable: Option<Arc<PathBuf>>,
    book: &Book,
    shutdown: &CancellationToken,
    conversion_timeout: Duration,
    _permit: OwnedSemaphorePermit,
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
    let built_in_destination = directory.path().join(format!("{stem}.internal.kepub.epub"));
    let built_in = run_built_in(source.clone(), built_in_destination.clone(), shutdown.clone()).await;
    match built_in {
        Ok(length) => {
            return Ok(CachedKepub {
                _directory: directory,
                path: built_in_destination,
                length,
            });
        }
        Err(BuiltInFailure::Cancelled) => return Err(KepubConversionError::Cancelled),
        Err(BuiltInFailure::Worker(error)) => return Err(KepubConversionError::Worker(error)),
        Err(BuiltInFailure::Conversion(error)) if error.allows_external_fallback() && executable.is_some() => {
            tracing::warn!(%error, path = %source.display(), "In-process KEPUB conversion failed; using external fallback");
        }
        Err(BuiltInFailure::Conversion(error)) => return Err(KepubConversionError::BuiltIn(error.to_string())),
    }

    let destination = directory.path().join(format!("{stem}.kepub.epub"));
    let diagnostic_path = directory.path().join("kepubify.stderr.log");
    run_external(
        executable.expect("fallback executable checked above").as_ref(),
        &source,
        &destination,
        &diagnostic_path,
        shutdown,
        conversion_timeout,
    )
    .await?;
    validate_output(source, destination.clone(), shutdown.clone()).await?;
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

enum BuiltInFailure {
    Cancelled,
    Worker(String),
    Conversion(komga_media::KepubConversionError),
}

async fn run_built_in(
    source: PathBuf,
    destination: PathBuf,
    shutdown: CancellationToken,
) -> Result<u64, BuiltInFailure> {
    spawn_blocking(move || {
        let source_file = StdFile::open(&source)
            .map_err(|error| BuiltInFailure::Conversion(komga_media::KepubConversionError::Io(error)))?;
        let destination_file = StdFile::create(&destination)
            .map_err(|error| BuiltInFailure::Conversion(komga_media::KepubConversionError::Io(error)))?;
        convert_epub_to_kepub(source_file, destination_file, || shutdown.is_cancelled())
            .map_err(BuiltInFailure::Conversion)?;
        let source_file = StdFile::open(&source)
            .map_err(|error| BuiltInFailure::Conversion(komga_media::KepubConversionError::Io(error)))?;
        let destination_file = StdFile::open(&destination)
            .map_err(|error| BuiltInFailure::Conversion(komga_media::KepubConversionError::Io(error)))?;
        validate_kepub_conversion(source_file, destination_file, || shutdown.is_cancelled())
            .map_err(BuiltInFailure::Conversion)?;
        std::fs::metadata(&destination)
            .map(|metadata| metadata.len())
            .map_err(|error| BuiltInFailure::Conversion(komga_media::KepubConversionError::Io(error)))
    })
    .await
    .map_err(|error| BuiltInFailure::Worker(error.to_string()))?
    .map_err(|error| match error {
        BuiltInFailure::Conversion(komga_media::KepubConversionError::Cancelled) => BuiltInFailure::Cancelled,
        error => error,
    })
}

async fn run_external(
    executable: &Path,
    source: &Path,
    destination: &Path,
    diagnostic_path: &Path,
    shutdown: &CancellationToken,
    conversion_timeout: Duration,
) -> Result<(), KepubConversionError> {
    let diagnostic =
        StdFile::create(diagnostic_path).map_err(|error| KepubConversionError::Prepare(error.to_string()))?;
    let mut child = Command::new(executable)
        .arg(source)
        .arg("-o")
        .arg(destination)
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
        let diagnostic = read_diagnostic(diagnostic_path);
        let suffix = if diagnostic.is_empty() {
            String::new()
        } else {
            format!(": {diagnostic}")
        };
        return Err(KepubConversionError::Exit(suffix));
    }
    if !std::fs::metadata(destination).is_ok_and(|metadata| metadata.is_file()) {
        return Err(KepubConversionError::MissingOutput);
    }
    Ok(())
}

async fn validate_output(
    source: PathBuf,
    destination: PathBuf,
    shutdown: CancellationToken,
) -> Result<(), KepubConversionError> {
    spawn_blocking(move || {
        let source = StdFile::open(source).map_err(|error| KepubConversionError::Validation(error.to_string()))?;
        let destination =
            StdFile::open(destination).map_err(|error| KepubConversionError::Validation(error.to_string()))?;
        validate_kepub_conversion(source, destination, || shutdown.is_cancelled()).map_err(|error| match error {
            komga_media::KepubConversionError::Cancelled => KepubConversionError::Cancelled,
            error => KepubConversionError::Validation(error.to_string()),
        })
    })
    .await
    .map_err(|error| KepubConversionError::Worker(error.to_string()))?
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
    use std::{fs, io::Write, os::unix::fs::PermissionsExt};

    use chrono::Local;
    use url::Url;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn write_epub(path: &Path, content: &[u8]) {
        let mut writer = ZipWriter::new(StdFile::create(path).unwrap());
        writer
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"application/epub+zip").unwrap();
        writer
            .start_file("META-INF/container.xml", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#).unwrap();
        writer.start_file("content.opf", SimpleFileOptions::default()).unwrap();
        writer.write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="chapter"/></spine></package>"#).unwrap();
        writer
            .start_file("chapter.xhtml", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content).unwrap();
        writer.finish().unwrap();
    }

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
    async fn in_process_conversion_is_available_without_configuration_and_cached() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        write_epub(&source, b"<!DOCTYPE html><html><body><p>One. Two.</p></body></html>");
        let service = KepubService::configure(None).await;
        assert!(service.is_available());
        assert_eq!(service.effective_path(), None);
        let book = book(&source);
        let shutdown = CancellationToken::new();
        let (first, second) = tokio::join!(
            service.get_or_convert(&book, &shutdown),
            service.get_or_convert(&book, &shutdown)
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let mut archive = zip::ZipArchive::new(StdFile::open(&first.path).unwrap()).unwrap();
        let mut chapter = String::new();
        archive
            .by_name("chapter.xhtml")
            .unwrap()
            .read_to_string(&mut chapter)
            .unwrap();
        assert!(chapter.contains("koboSpan"));
    }

    #[tokio::test]
    async fn source_snapshot_change_uses_a_new_cache_entry() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        write_epub(&source, b"<!DOCTYPE html><html><body><p>First.</p></body></html>");
        let service = KepubService::configure(None).await;
        let shutdown = CancellationToken::new();
        let first = service.get_or_convert(&book(&source), &shutdown).await.unwrap();

        write_epub(
            &source,
            b"<!DOCTYPE html><html><body><p>A substantially longer replacement chapter. Second.</p></body></html>",
        );
        let second = service.get_or_convert(&book(&source), &shutdown).await.unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
        let mut archive = zip::ZipArchive::new(StdFile::open(&second.path).unwrap()).unwrap();
        let mut chapter = String::new();
        archive
            .by_name("chapter.xhtml")
            .unwrap()
            .read_to_string(&mut chapter)
            .unwrap();
        assert!(chapter.contains("replacement chapter"));
    }

    #[tokio::test]
    async fn conversion_limit_has_two_permits() {
        let service = KepubService::configure(None).await;
        let first = service.conversion_limit.clone().acquire_owned().await.unwrap();
        let second = service.conversion_limit.clone().acquire_owned().await.unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                service.conversion_limit.clone().acquire_owned()
            )
            .await
            .is_err()
        );
        drop(first);
        let third = tokio::time::timeout(
            Duration::from_millis(50),
            service.conversion_limit.clone().acquire_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        drop((second, third));
    }

    #[tokio::test]
    async fn configured_external_program_does_not_override_in_process_conversion() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        let count = temporary.path().join("count");
        write_epub(&source, b"<!DOCTYPE html><html><body><p>Text.</p></body></html>");
        let executable = script(
            temporary.path(),
            &format!("echo run >> '{}'; cp \"$1\" \"$3\"", count.display()),
        );
        let service = KepubService::configure(Some(executable)).await;
        service
            .get_or_convert(&book(&source), &CancellationToken::new())
            .await
            .unwrap();
        assert!(!count.exists());
    }

    #[tokio::test]
    async fn compatible_content_failure_uses_validated_external_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        let fallback = temporary.path().join("fallback.epub");
        let count = temporary.path().join("count");
        write_epub(&source, &[0xff, 0xfe]);
        write_epub(&fallback, b"<!DOCTYPE html><html><body><p>Fallback.</p></body></html>");
        let executable = script(
            temporary.path(),
            &format!("echo run >> '{}'; cp '{}' \"$3\"", count.display(), fallback.display()),
        );
        let service = KepubService::configure(Some(executable)).await;
        service
            .get_or_convert(&book(&source), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(count).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn unsafe_input_never_starts_external_fallback() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        let count = temporary.path().join("count");
        let mut writer = ZipWriter::new(StdFile::create(&source).unwrap());
        writer.start_file("../bad", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        let executable = script(
            temporary.path(),
            &format!("echo run >> '{}'; cp \"$1\" \"$3\"", count.display()),
        );
        let service = KepubService::configure(Some(executable)).await;
        assert!(
            service
                .get_or_convert(&book(&source), &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(!count.exists());
    }

    #[tokio::test]
    async fn conversion_failures_are_not_cached() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        let count = temporary.path().join("count");
        write_epub(&source, &[0xff]);
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
    async fn external_fallback_timeout_terminates_the_process() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("Book.epub");
        write_epub(&source, &[0xff]);
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
