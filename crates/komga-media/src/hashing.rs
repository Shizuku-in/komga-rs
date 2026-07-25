use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
};

use thiserror::Error;
use xxhash_rust::xxh3::Xxh3;

use komga_core::{Book, ScanResult};

const BUFFER_SIZE: usize = 8_192;

/// Errors that prevent a source file hash from being computed.
#[derive(Debug, Error)]
pub enum FileHashError {
    /// The hash operation was cancelled between input chunks.
    #[error("file hashing cancelled")]
    Cancelled,
    /// The source file could not be opened or read.
    #[error("could not read source file: {0}")]
    Io(#[from] io::Error),
    /// The source URL is not a local file.
    #[error("source URL is not a local file: {0}")]
    InvalidSource(String),
    /// The source changed while its hash was being computed.
    #[error("source file changed while hashing: {0}")]
    SourceChanged(String),
}

/// Computes Komga's seeded XXH3-128 source-file hash.
///
/// The result is a 32-character lowercase hexadecimal string compatible with
/// Kotlin's `Algorithm.XXH3_128.Seeded(0)` implementation.
///
/// # Errors
///
/// Returns [`FileHashError::Cancelled`] when `cancelled` requests a stop, or
/// [`FileHashError::Io`] when the file cannot be opened or read.
pub fn hash_file(path: &Path, cancelled: impl Fn() -> bool) -> Result<String, FileHashError> {
    if cancelled() {
        return Err(FileHashError::Cancelled);
    }
    let mut reader = BufReader::with_capacity(BUFFER_SIZE, File::open(path)?);
    let mut hasher = Xxh3::with_seed(0);
    let mut buffer = [0_u8; BUFFER_SIZE];
    loop {
        if cancelled() {
            return Err(FileHashError::Cancelled);
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:032x}", hasher.digest128()))
}

/// Recomputes hashes needed to distinguish timestamp-only scan changes.
///
/// Only same-sized files with an existing hash are read. The resulting hash is
/// attached to the scan snapshot so persistence can preserve ready media when
/// the bytes are unchanged.
///
/// # Errors
///
/// Returns an error when hashing is cancelled, a candidate cannot be read, its
/// URL is not local, or its metadata changes during hashing.
pub fn reconcile_scan_hashes(
    scan: &mut ScanResult,
    existing_books: &[Book],
    cancelled: impl Fn() -> bool,
) -> Result<(), FileHashError> {
    let existing = existing_books
        .iter()
        .filter(|book| book.deleted_date.is_none())
        .map(|book| (book.url.as_str(), book))
        .collect::<HashMap<_, _>>();
    for scanned in &mut scan.series {
        for book in &mut scanned.books {
            let Some(previous) = existing.get(book.url.as_str()) else {
                continue;
            };
            if previous.file_hash.is_empty()
                || previous.file_size != book.file_size
                || previous.file_last_modified == book.file_last_modified
            {
                continue;
            }
            let path = book
                .url
                .to_file_path()
                .map_err(|()| FileHashError::InvalidSource(book.url.to_string()))?;
            let hash = hash_file(&path, &cancelled)?;
            let metadata = path.metadata()?;
            let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
            let modified = metadata
                .modified()
                .map(chrono::DateTime::<chrono::Local>::from)
                .map(|value| value.naive_local())?;
            if size != book.file_size || modified != book.file_last_modified {
                return Err(FileHashError::SourceChanged(path.display().to_string()));
            }
            book.file_hash = hash;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs};

    use super::*;

    #[test]
    fn hashes_match_kotlin_xxh3_128_vectors() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");

        fs::write(&path, []).unwrap();
        assert_eq!(hash_file(&path, || false).unwrap(), "99aa06d3014798d86001c324468d497f");

        fs::write(&path, b"hello").unwrap();
        assert_eq!(hash_file(&path, || false).unwrap(), "b5e9c1ad071b3e7fc779cfaa5e523818");
    }

    #[test]
    fn streams_files_larger_than_the_buffer() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        let content = (0..BUFFER_SIZE * 3 + 17)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        fs::write(&path, &content).unwrap();

        let expected = format!("{:032x}", xxhash_rust::xxh3::xxh3_128_with_seed(&content, 0));
        assert_eq!(hash_file(&path, || false).unwrap(), expected);
    }

    #[test]
    fn stops_when_cancelled_between_chunks() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        fs::write(&path, vec![1_u8; BUFFER_SIZE * 2]).unwrap();
        let checks = Cell::new(0);

        let error = hash_file(&path, || {
            checks.set(checks.get() + 1);
            checks.get() > 2
        })
        .unwrap_err();

        assert!(matches!(error, FileHashError::Cancelled));
    }

    #[test]
    fn reports_source_file_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let error = hash_file(&temporary.path().join("missing.cbz"), || false).unwrap_err();
        assert!(matches!(error, FileHashError::Io(_)));
    }

    #[test]
    fn scan_reconciliation_hashes_only_changed_same_sized_sources() {
        use chrono::Duration;
        use komga_core::{Library, scan_library_files};
        use url::Url;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Book.cbz"), b"hello").unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        let mut scan = scan_library_files(&library, || false).unwrap();
        let mut existing = scan.series[0].books[0].clone();
        existing.file_last_modified -= Duration::seconds(1);
        existing.file_hash = "previous".into();

        reconcile_scan_hashes(&mut scan, &[existing], || false).unwrap();

        assert_eq!(scan.series[0].books[0].file_hash, "b5e9c1ad071b3e7fc779cfaa5e523818");
    }
}
