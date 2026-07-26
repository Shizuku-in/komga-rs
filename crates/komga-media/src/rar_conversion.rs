use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use chrono::{DateTime, Local};
use komga_core::{Book, Media, MediaStatus, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE, ZIP_MEDIA_TYPE};
use rars::{ArchiveReader, Error as RarError};
use thiserror::Error;
use url::Url;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{FileHashError, MediaAnalysisError, analyze_book, archive, hash_file};

/// Validated temporary CBZ produced from one RAR-backed book.
#[derive(Debug)]
pub struct RarToCbzConversion {
    /// Source identity with filesystem fields projected from the temporary CBZ.
    pub book: Book,
    /// Reanalyzed ready ZIP media with page hashes restored.
    pub media: Media,
}

/// Failures produced while converting one RAR-backed book to CBZ.
#[derive(Debug, Error)]
pub enum RarToCbzError {
    /// Server shutdown was requested before conversion completed.
    #[error("RAR to CBZ conversion cancelled")]
    Cancelled,
    /// The source no longer matches the submitted book snapshot.
    #[error("RAR conversion source is stale: {0}")]
    Stale(String),
    /// The book or media is not a local ready RAR4/RAR5 source.
    #[error("RAR conversion is unsupported: {0}")]
    Unsupported(String),
    /// The RAR source or generated ZIP could not be decoded or encoded.
    #[error("could not convert RAR archive: {0}")]
    Archive(String),
    /// Filesystem metadata or output operations failed.
    #[error("could not write converted CBZ: {0}")]
    Io(#[from] io::Error),
    /// The generated archive did not preserve the analyzed content.
    #[error("converted CBZ failed validation: {0}")]
    Validation(String),
}

/// Streams a local ready RAR4/RAR5 book into a validated temporary CBZ.
///
/// The destination is created exclusively and removed on failure. The source
/// is never changed. Every analyzed page and ancillary file is copied with
/// the fastest supported DEFLATE level, then the destination is reanalyzed
/// before it is returned.
///
/// # Errors
///
/// Returns [`RarToCbzError::Stale`] when source metadata or bytes change,
/// [`RarToCbzError::Cancelled`] when cancellation is requested, and validation,
/// archive, or I/O errors for unsafe output.
pub fn convert_rar_to_cbz(
    book: &Book,
    media: &Media,
    destination: &Path,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<RarToCbzConversion, RarToCbzError> {
    if cancelled() {
        return Err(RarToCbzError::Cancelled);
    }
    if media.status != MediaStatus::Ready
        || !matches!(media.media_type.as_deref(), Some(RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE))
    {
        return Err(RarToCbzError::Unsupported(
            "media must be a ready RAR4 or RAR5 archive".into(),
        ));
    }
    let source = book
        .url
        .to_file_path()
        .map_err(|()| RarToCbzError::Unsupported("book URL is not a local file".into()))?;
    let initial_metadata = fs::metadata(&source)?;
    ensure_book_snapshot(book, &initial_metadata)?;
    let initial_fingerprint = source_fingerprint(&source, &cancelled)?;
    let file = OpenOptions::new().write(true).create_new(true).open(destination)?;
    let mut guard = DestinationGuard::new(destination);

    write_cbz(&source, file, media, &cancelled)?;
    fs::set_permissions(destination, initial_metadata.permissions())?;
    if cancelled() {
        return Err(RarToCbzError::Cancelled);
    }
    let current_metadata = fs::metadata(&source)?;
    ensure_same_metadata(&initial_metadata, &current_metadata, &source)?;
    let current_fingerprint = source_fingerprint(&source, &cancelled)?;
    if current_fingerprint != initial_fingerprint {
        return Err(RarToCbzError::Stale("source bytes changed during conversion".into()));
    }

    let output_metadata = fs::metadata(destination)?;
    let mut converted_book = book.clone();
    converted_book.url = Url::from_file_path(destination)
        .map_err(|()| RarToCbzError::Unsupported("destination cannot be represented as a file URL".into()))?;
    converted_book.file_size = i64::try_from(output_metadata.len()).unwrap_or(i64::MAX);
    converted_book.file_last_modified = scan_timestamp(&output_metadata);
    converted_book.file_hash.clear();
    converted_book.file_hash_koreader.clear();
    let mut converted_media =
        analyze_book(&converted_book, analyze_dimensions, &cancelled).map_err(|error| match error {
            MediaAnalysisError::Cancelled => RarToCbzError::Cancelled,
        })?;
    validate_converted_media(&converted_media, media)?;
    for page in &mut converted_media.pages {
        let previous = media
            .pages
            .iter()
            .find(|candidate| {
                candidate.file_name == page.file_name
                    && candidate.media_type == page.media_type
                    && candidate.file_size == page.file_size
            })
            .ok_or_else(|| RarToCbzError::Validation(format!("unexpected page {}", page.file_name)))?;
        page.file_hash.clone_from(&previous.file_hash);
    }
    guard.disarm();
    Ok(RarToCbzConversion {
        book: converted_book,
        media: converted_media,
    })
}

fn write_cbz(
    source: &Path,
    destination: File,
    media: &Media,
    cancelled: &impl Fn() -> bool,
) -> Result<(), RarToCbzError> {
    let archive = ArchiveReader::read_path(source).map_err(|error| RarToCbzError::Archive(error.to_string()))?;
    let expected = media
        .pages
        .iter()
        .map(|page| page.file_name.clone())
        .chain(media.files.iter().map(|file| file.file_name.clone()))
        .collect::<BTreeSet<_>>();
    let written = Rc::new(RefCell::new(BTreeSet::new()));
    let zip_error = Rc::new(RefCell::new(None));
    let writer = Rc::new(RefCell::new(Some(ZipWriter::new(destination))));
    archive
        .extract_to(None, |meta| {
            if cancelled() {
                return Err(RarError::Cancelled);
            }
            let name = meta.name_lossy();
            if meta.is_directory || !expected.contains(&name) || !written.borrow_mut().insert(name.clone()) {
                return Ok(Box::new(io::sink()));
            }
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(1));
            let result = writer
                .borrow_mut()
                .as_mut()
                .expect("ZIP writer must exist during extraction")
                .start_file(name, options);
            if let Err(error) = result {
                *zip_error.borrow_mut() = Some(error.to_string());
                return Err(RarError::Cancelled);
            }
            Ok(Box::new(ZipEntryWriter {
                writer: Rc::clone(&writer),
            }))
        })
        .map_err(|error| {
            if let Some(error) = zip_error.borrow_mut().take() {
                RarToCbzError::Archive(error)
            } else if archive::rar_error_is_cancelled(&error) && cancelled() {
                RarToCbzError::Cancelled
            } else {
                RarToCbzError::Archive(error.to_string())
            }
        })?;
    if *written.borrow() != expected {
        let missing = expected.difference(&written.borrow()).cloned().collect::<Vec<_>>();
        return Err(RarToCbzError::Validation(format!(
            "source archive did not yield registered entries: {}",
            missing.join(", ")
        )));
    }
    let mut writer = writer
        .borrow_mut()
        .take()
        .expect("ZIP writer must exist after extraction");
    writer.flush()?;
    let file = writer
        .finish()
        .map_err(|error| RarToCbzError::Archive(error.to_string()))?;
    file.sync_all()?;
    Ok(())
}

struct ZipEntryWriter {
    writer: Rc<RefCell<Option<ZipWriter<File>>>>,
}

impl Write for ZipEntryWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer
            .borrow_mut()
            .as_mut()
            .expect("ZIP writer must exist while an entry is open")
            .write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .borrow_mut()
            .as_mut()
            .expect("ZIP writer must exist while an entry is open")
            .flush()
    }
}

fn validate_converted_media(converted: &Media, previous: &Media) -> Result<(), RarToCbzError> {
    if converted.status != MediaStatus::Ready || converted.media_type.as_deref() != Some(ZIP_MEDIA_TYPE) {
        return Err(RarToCbzError::Validation("temporary archive is not a ready ZIP".into()));
    }
    let pages_match = converted.pages.len() == previous.pages.len()
        && converted.pages.iter().zip(&previous.pages).all(|(left, right)| {
            left.file_name == right.file_name
                && left.media_type == right.media_type
                && left.file_size == right.file_size
        });
    if !pages_match {
        return Err(RarToCbzError::Validation(
            "temporary archive does not contain exactly the original pages".into(),
        ));
    }
    let files_match = converted.files.len() == previous.files.len()
        && converted.files.iter().zip(&previous.files).all(|(left, right)| {
            left.file_name == right.file_name
                && left.media_type == right.media_type
                && left.file_size == right.file_size
        });
    if !files_match {
        return Err(RarToCbzError::Validation(
            "temporary archive does not contain exactly the original ancillary files".into(),
        ));
    }
    Ok(())
}

fn source_fingerprint(path: &Path, cancelled: &impl Fn() -> bool) -> Result<String, RarToCbzError> {
    hash_file(path, cancelled).map_err(|error| match error {
        FileHashError::Cancelled => RarToCbzError::Cancelled,
        FileHashError::Io(error) => RarToCbzError::Io(error),
        FileHashError::InvalidSource(source) | FileHashError::SourceChanged(source) => RarToCbzError::Stale(source),
    })
}

fn ensure_book_snapshot(book: &Book, metadata: &Metadata) -> Result<(), RarToCbzError> {
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    if size != book.file_size || scan_timestamp(metadata) != book.file_last_modified {
        return Err(RarToCbzError::Stale(
            "book filesystem snapshot changed before conversion".into(),
        ));
    }
    Ok(())
}

fn ensure_same_metadata(initial: &Metadata, current: &Metadata, path: &Path) -> Result<(), RarToCbzError> {
    if initial.len() != current.len() || initial.modified()? != current.modified()? {
        return Err(RarToCbzError::Stale(format!(
            "source metadata changed during conversion: {}",
            path.display()
        )));
    }
    Ok(())
}

fn scan_timestamp(metadata: &Metadata) -> chrono::NaiveDateTime {
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    DateTime::<Local>::from(modified.max(created)).naive_local()
}

struct DestinationGuard {
    path: PathBuf,
    armed: bool,
}

impl DestinationGuard {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DestinationGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use komga_core::scan_book_file;
    use zip::ZipArchive;

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/archives")
            .join(name)
    }

    #[test]
    fn converts_rar4_rar5_and_solid_archives_to_valid_cbz() {
        for name in ["rar4.rar", "rar4-solid.rar", "rar5.rar", "rar5-solid.rar"] {
            let source = fixture(name);
            let book = scan_book_file(&source, "library", false).unwrap();
            let mut media = analyze_book(&book, true, || false).unwrap();
            for (index, page) in media.pages.iter_mut().enumerate() {
                page.file_hash = format!("hash-{index}");
            }
            let temporary = tempfile::tempdir().unwrap();
            let destination = temporary.path().join("converted.cbz");
            let converted = convert_rar_to_cbz(&book, &media, &destination, true, || false).unwrap();
            assert_eq!(converted.media.status, MediaStatus::Ready, "{name}");
            assert_eq!(converted.media.media_type.as_deref(), Some(ZIP_MEDIA_TYPE), "{name}");
            assert_eq!(
                converted
                    .media
                    .pages
                    .iter()
                    .map(|page| page.file_hash.as_str())
                    .collect::<Vec<_>>(),
                media
                    .pages
                    .iter()
                    .map(|page| page.file_hash.as_str())
                    .collect::<Vec<_>>(),
                "{name}"
            );
            let mut archive = ZipArchive::new(File::open(&destination).unwrap()).unwrap();
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index).unwrap();
                assert_eq!(entry.compression(), CompressionMethod::Deflated, "{name}");
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                assert!(!bytes.is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn cancellation_and_unsupported_media_leave_no_destination() {
        let source = fixture("rar4.rar");
        let book = scan_book_file(&source, "library", false).unwrap();
        let media = analyze_book(&book, false, || false).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("cancelled.cbz");
        assert!(matches!(
            convert_rar_to_cbz(&book, &media, &destination, false, || true),
            Err(RarToCbzError::Cancelled)
        ));
        assert!(!destination.exists());

        let mut unsupported = media;
        unsupported.status = MediaStatus::Unsupported;
        assert!(matches!(
            convert_rar_to_cbz(&book, &unsupported, &destination, false, || false),
            Err(RarToCbzError::Unsupported(_))
        ));
        assert!(!destination.exists());
    }
}
