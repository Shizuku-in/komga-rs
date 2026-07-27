use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use image::{ImageFormat, ImageReader, RgbImage};
use md5::{Digest, Md5};
use thiserror::Error;
use xxhash_rust::xxh3::Xxh3;

use komga_core::{Book, Dimension, ScanResult};

const BUFFER_SIZE: usize = 8_192;
const KOREADER_SAMPLE_SIZE: usize = 1_024;
const KOREADER_SAMPLE_OFFSETS: [u64; 12] = [
    0,
    1_024,
    4_096,
    16_384,
    65_536,
    262_144,
    1_048_576,
    4_194_304,
    16_777_216,
    67_108_864,
    268_435_456,
    1_073_741_824,
];
const RGB_PAGE_HASH_HEADER: &[u8] = b"KOMGA_PAGE_RGB24_V1\0";

/// Algorithm marker persisted beside raw page-content hashes.
pub const RAW_PAGE_HASH_ALGORITHM: &str = "RAW_XXH3_128_V1";

/// Algorithm marker persisted beside canonical JPEG pixel hashes.
pub const RGB_PAGE_HASH_ALGORITHM: &str = "RGB24_XXH3_128_V1";

/// A page hash paired with the algorithm needed to interpret it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputedPageHash {
    /// Seeded XXH3-128 digest as 32 lowercase hexadecimal characters.
    pub hash: String,
    /// Stable algorithm marker persisted with the digest.
    pub algorithm: &'static str,
}

/// Errors that prevent page content from being normalized for hashing.
#[derive(Debug, Error)]
pub enum PageHashError {
    /// JPEG content could not be decoded into canonical RGB pixels.
    #[error("could not decode JPEG page: {0}")]
    InvalidJpeg(String),
}

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

/// Computes KOReader's partial-MD5 document identifier.
///
/// At most 1 KiB is read at the beginning of the file and at eleven
/// exponentially increasing offsets through 1 GiB. Samples beyond EOF are
/// ignored, matching KOReader and Kotlin Komga.
///
/// # Errors
///
/// Returns [`FileHashError::Cancelled`] when `cancelled` requests a stop, or
/// [`FileHashError::Io`] when the file cannot be opened, sought, or read.
pub fn hash_file_koreader(path: &Path, cancelled: impl Fn() -> bool) -> Result<String, FileHashError> {
    if cancelled() {
        return Err(FileHashError::Cancelled);
    }
    let mut file = File::open(path)?;
    let mut digest = Md5::new();
    let mut buffer = [0_u8; KOREADER_SAMPLE_SIZE];
    for offset in KOREADER_SAMPLE_OFFSETS {
        if cancelled() {
            return Err(FileHashError::Cancelled);
        }
        file.seek(SeekFrom::Start(offset))?;
        let count = file.read(&mut buffer)?;
        if count > 0 {
            digest.update(&buffer[..count]);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Computes a deterministic page-content hash.
///
/// JPEG input is decoded to row-major RGB24 pixels and hashed with a versioned
/// header plus its little-endian width and height. This removes metadata while
/// avoiding encoder-dependent JPEG bytes. Other media types retain Komga's raw
/// byte hashing behavior.
///
/// # Errors
///
/// Returns [`PageHashError::InvalidJpeg`] when `media_type` is exactly
/// `image/jpeg` and the content cannot be decoded as JPEG.
pub fn hash_page(content: &[u8], media_type: &str) -> Result<ComputedPageHash, PageHashError> {
    let mut hasher = Xxh3::with_seed(0);
    let algorithm = if media_type == "image/jpeg" {
        let image = ImageReader::with_format(std::io::Cursor::new(content), ImageFormat::Jpeg)
            .decode()
            .map_err(|error| PageHashError::InvalidJpeg(error.to_string()))?
            .to_rgb8();
        return Ok(hash_rgb_page(&image).0);
    } else {
        hasher.update(content);
        RAW_PAGE_HASH_ALGORITHM
    };
    Ok(ComputedPageHash {
        hash: format!("{:032x}", hasher.digest128()),
        algorithm,
    })
}

pub(crate) fn hash_jpeg_page(content: &[u8]) -> Result<(ComputedPageHash, Dimension), PageHashError> {
    let image = ImageReader::with_format(std::io::Cursor::new(content), ImageFormat::Jpeg)
        .decode()
        .map_err(|error| PageHashError::InvalidJpeg(error.to_string()))?
        .to_rgb8();
    Ok(hash_rgb_page(&image))
}

fn hash_rgb_page(image: &RgbImage) -> (ComputedPageHash, Dimension) {
    let mut hasher = Xxh3::with_seed(0);
    hasher.update(RGB_PAGE_HASH_HEADER);
    hasher.update(&image.width().to_le_bytes());
    hasher.update(&image.height().to_le_bytes());
    hasher.update(image.as_raw());
    (
        ComputedPageHash {
            hash: format!("{:032x}", hasher.digest128()),
            algorithm: RGB_PAGE_HASH_ALGORITHM,
        },
        Dimension {
            width: i32::try_from(image.width()).unwrap_or(i32::MAX),
            height: i32::try_from(image.height()).unwrap_or(i32::MAX),
        },
    )
}

pub(crate) fn hash_bytes(content: &[u8]) -> String {
    format!("{:032x}", xxhash_rust::xxh3::xxh3_128_with_seed(content, 0))
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
    use std::{cell::Cell, fs, path::PathBuf};

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
    fn koreader_hashes_match_partial_md5_vectors() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.epub");

        fs::write(&path, []).unwrap();
        assert_eq!(
            hash_file_koreader(&path, || false).unwrap(),
            "d41d8cd98f00b204e9800998ecf8427e"
        );

        fs::write(&path, b"hello").unwrap();
        assert_eq!(
            hash_file_koreader(&path, || false).unwrap(),
            "5d41402abc4b2a76b9719d911017c592"
        );
    }

    #[test]
    fn koreader_hash_reads_only_the_defined_samples() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        let content = (0..5_000).map(|index| (index % 251) as u8).collect::<Vec<_>>();
        fs::write(&path, &content).unwrap();

        let mut expected = Md5::new();
        expected.update(&content[0..1_024]);
        expected.update(&content[1_024..2_048]);
        expected.update(&content[4_096..]);
        assert_eq!(
            hash_file_koreader(&path, || false).unwrap(),
            format!("{:x}", expected.finalize())
        );
    }

    #[test]
    fn koreader_hash_honors_cancellation_and_reports_io_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.pdf");
        fs::write(&path, vec![1_u8; KOREADER_SAMPLE_SIZE * 2]).unwrap();
        let checks = Cell::new(0);

        let error = hash_file_koreader(&path, || {
            checks.set(checks.get() + 1);
            checks.get() > 2
        })
        .unwrap_err();
        assert!(matches!(error, FileHashError::Cancelled));
        assert!(matches!(
            hash_file_koreader(&temporary.path().join("missing.epub"), || false),
            Err(FileHashError::Io(_))
        ));
    }

    #[test]
    fn jpeg_page_hashes_ignore_metadata_and_match_canonical_vectors() {
        let fixture = |directory: &str, file: &str| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../komga/komga/src/test/resources/hashpage")
                .join(directory)
                .join(file)
        };
        let vectors = [
            ("e-sou.jpeg", "b0b432764c6e797f13db4e22424bc172"),
            ("e-z.jpeg", "51e3c31e4edfc7236cb49d38dfb45bac"),
            ("m-d.jpeg", "3b2a789232cb4b8e8a49c66afb5eab4d"),
        ];

        for (directory, expected) in vectors {
            let first_content = fs::read(fixture(directory, "1.jpg")).unwrap();
            let first = hash_page(&first_content, "image/jpeg").unwrap();
            let second = hash_page(&fs::read(fixture(directory, "2.jpg")).unwrap(), "image/jpeg").unwrap();
            let (detailed, dimension) = hash_jpeg_page(&first_content).unwrap();
            assert_eq!(first, second);
            assert_eq!(detailed, first);
            let decoded = image::load_from_memory(&first_content).unwrap();
            assert_eq!(dimension.width, i32::try_from(decoded.width()).unwrap());
            assert_eq!(dimension.height, i32::try_from(decoded.height()).unwrap());
            assert_eq!(first.hash, expected);
            assert_eq!(first.algorithm, RGB_PAGE_HASH_ALGORITHM);
        }
    }

    #[test]
    fn non_jpeg_pages_hash_raw_bytes_and_invalid_jpegs_fail() {
        let raw = hash_page(b"hello", "image/png").unwrap();
        assert_eq!(raw.hash, "b5e9c1ad071b3e7fc779cfaa5e523818");
        assert_eq!(raw.algorithm, RAW_PAGE_HASH_ALGORITHM);

        assert!(matches!(
            hash_page(b"not a jpeg", "image/jpeg"),
            Err(PageHashError::InvalidJpeg(_))
        ));
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
