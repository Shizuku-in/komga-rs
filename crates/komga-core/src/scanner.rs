//! Filesystem discovery for configured libraries.

use std::{
    cmp::Ordering,
    cmp::max,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{DateTime, Local, NaiveDateTime};
use thiserror::Error;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};
use url::Url;
use walkdir::WalkDir;

use crate::{Library, new_id};

/// A series discovered in a library root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Series {
    /// Stable database identifier.
    pub id: String,
    /// Directory or one-shot filename used as the initial title.
    pub name: String,
    /// Directory URL, or the book URL for one-shot series.
    pub url: Url,
    /// Most recent relevant filesystem timestamp in local time.
    pub file_last_modified: NaiveDateTime,
    /// Owning library identifier.
    pub library_id: String,
    /// Number of books currently assigned to the series.
    pub book_count: i32,
    /// Soft-deletion timestamp, if the series is no longer on disk.
    pub deleted_date: Option<NaiveDateTime>,
    /// Whether this series represents one standalone book.
    pub oneshot: bool,
    /// Persisted creation timestamp when loaded from storage.
    pub created_date: Option<NaiveDateTime>,
    /// Persisted modification timestamp when loaded from storage.
    pub last_modified_date: Option<NaiveDateTime>,
}

/// A book file discovered in a library root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    /// Stable database identifier.
    pub id: String,
    /// Filename without its extension.
    pub name: String,
    /// File URL stored by Komga.
    pub url: Url,
    /// Most recent relevant filesystem timestamp in local time.
    pub file_last_modified: NaiveDateTime,
    /// File length in bytes.
    pub file_size: i64,
    /// Source-file hash, empty until file hashing is implemented.
    pub file_hash: String,
    /// KOReader-compatible hash, empty until hashing is implemented.
    pub file_hash_koreader: String,
    /// One-based natural-sort position inside the series.
    pub number: i32,
    /// Owning series identifier, populated while applying a scan.
    pub series_id: String,
    /// Owning library identifier.
    pub library_id: String,
    /// Soft-deletion timestamp, if the file is no longer on disk.
    pub deleted_date: Option<NaiveDateTime>,
    /// Whether this book is represented as its own one-shot series.
    pub oneshot: bool,
    /// Persisted creation timestamp when loaded from storage.
    pub created_date: Option<NaiveDateTime>,
    /// Persisted modification timestamp when loaded from storage.
    pub last_modified_date: Option<NaiveDateTime>,
}

/// One discovered series and its naturally ordered books.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedSeries {
    /// Series filesystem snapshot.
    pub series: Series,
    /// Books found directly in the series directory.
    pub books: Vec<Book>,
}

/// A supported filesystem sidecar discovered while scanning a library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedSidecar {
    /// Exact sidecar file URL.
    pub url: Url,
    /// Owning series directory URL.
    pub parent_url: Url,
    /// Filesystem modification time.
    pub last_modified_time: NaiveDateTime,
    /// Consumer category used to schedule the correct refresh task.
    pub sidecar_type: SidecarType,
    /// Entity represented by `parent_url`.
    pub source: SidecarSource,
}

/// Kind of metadata represented by a filesystem sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarType {
    /// Local artwork image.
    Artwork,
    /// Local metadata document.
    Metadata,
}

/// Entity to which a filesystem sidecar belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarSource {
    /// The sidecar belongs to one book.
    Book,
    /// The sidecar belongs to a series.
    Series,
}

/// Complete filesystem snapshot produced for one library.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanResult {
    /// Discovered series in deterministic path order.
    pub series: Vec<ScannedSeries>,
    /// Metadata and artwork sidecars found for scanned content.
    pub sidecars: Vec<ScannedSidecar>,
}

/// Errors that prevent a library filesystem scan from completing.
#[derive(Debug, Error)]
pub enum ScanError {
    /// The configured root is not an accessible directory.
    #[error("library root is not accessible: {0}")]
    RootUnavailable(PathBuf),
    /// The scan was cancelled during process shutdown.
    #[error("library scan cancelled")]
    Cancelled,
}

/// Discovers series and book files using the configured library scan policy.
///
/// Inaccessible descendants are skipped, matching Kotlin's file visitor. The
/// root itself is validated before traversal. `cancelled` is checked between
/// entries so a server shutdown can retain the task for the next process.
///
/// # Errors
///
/// Returns [`ScanError::RootUnavailable`] when the root cannot be listed and
/// [`ScanError::Cancelled`] when the cancellation callback requests a stop.
pub fn scan_library_files(library: &Library, cancelled: impl Fn() -> bool) -> Result<ScanResult, ScanError> {
    let root = library
        .root_path()
        .filter(|path| path.is_dir())
        .ok_or_else(|| ScanError::RootUnavailable(PathBuf::from(library.root.path())))?;
    fs::read_dir(&root).map_err(|_| ScanError::RootUnavailable(root.clone()))?;

    let exclusions = library
        .scan_directory_exclusions
        .iter()
        .map(|value| value.to_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let mut books_by_directory = BTreeMap::<PathBuf, Vec<Book>>::new();
    let walker = WalkDir::new(&root)
        .follow_links(true)
        .into_iter()
        .filter_entry(|entry| !entry.file_type().is_dir() || should_visit(entry.path(), &exclusions));

    for entry in walker {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() || is_hidden(entry.path()) || !is_supported(entry.path(), library) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let Ok(url) = Url::from_file_path(entry.path()) else {
            continue;
        };
        let name = entry
            .path()
            .file_stem()
            .unwrap_or(entry.file_name())
            .to_string_lossy()
            .into_owned();
        books_by_directory.entry(parent.to_path_buf()).or_default().push(Book {
            id: new_id(),
            name,
            url,
            file_last_modified: updated_time(&metadata),
            file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            file_hash: String::new(),
            file_hash_koreader: String::new(),
            number: 0,
            series_id: String::new(),
            library_id: library.id.clone(),
            deleted_date: None,
            oneshot: false,
            created_date: None,
            last_modified_date: None,
        });
    }

    let oneshots = library
        .oneshots_directory
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase);
    let mut scanned = Vec::new();
    for (directory, mut books) in books_by_directory {
        books.sort_by(|left, right| compare_book_names(&left.name, &right.name));
        for (index, book) in books.iter_mut().enumerate() {
            book.number = i32::try_from(index + 1).unwrap_or(i32::MAX);
        }
        let is_oneshot = oneshots
            .as_ref()
            .is_some_and(|value| directory.to_string_lossy().to_lowercase().contains(value));
        if is_oneshot {
            for mut book in books {
                book.oneshot = true;
                let series = Series {
                    id: new_id(),
                    name: book.name.clone(),
                    url: book.url.clone(),
                    file_last_modified: book.file_last_modified,
                    library_id: library.id.clone(),
                    book_count: 1,
                    deleted_date: None,
                    oneshot: true,
                    created_date: None,
                    last_modified_date: None,
                };
                book.number = 1;
                scanned.push(ScannedSeries {
                    series,
                    books: vec![book],
                });
            }
        } else {
            let directory_time = fs::metadata(&directory)
                .map(|metadata| updated_time(&metadata))
                .unwrap_or_else(|_| books.iter().map(|book| book.file_last_modified).max().unwrap());
            let file_last_modified = if library.scan_force_modified_time {
                max(
                    directory_time,
                    books.iter().map(|book| book.file_last_modified).max().unwrap(),
                )
            } else {
                directory_time
            };
            let name = directory
                .file_name()
                .filter(|name| !name.is_empty())
                .unwrap_or(directory.as_os_str())
                .to_string_lossy()
                .into_owned();
            scanned.push(ScannedSeries {
                series: Series {
                    id: new_id(),
                    name,
                    url: Url::from_directory_path(&directory)
                        .expect("walked absolute library directory must convert to a file URL"),
                    file_last_modified,
                    library_id: library.id.clone(),
                    book_count: i32::try_from(books.len()).unwrap_or(i32::MAX),
                    deleted_date: None,
                    oneshot: false,
                    created_date: None,
                    last_modified_date: None,
                },
                books,
            });
        }
    }
    scanned.sort_by(|left, right| left.series.url.as_str().cmp(right.series.url.as_str()));
    let mut sidecars = scanned
        .iter()
        .filter(|entry| !entry.series.oneshot)
        .filter_map(|entry| {
            let directory = entry.series.url.to_file_path().ok()?;
            let path = directory.join("series.json");
            let metadata = fs::metadata(&path).ok().filter(|m| m.is_file())?;
            Some(ScannedSidecar {
                url: Url::from_file_path(path).ok()?,
                parent_url: entry.series.url.clone(),
                last_modified_time: updated_time(&metadata),
                sidecar_type: SidecarType::Metadata,
                source: SidecarSource::Series,
            })
        })
        .collect::<Vec<_>>();
    for entry in &scanned {
        if !entry.series.oneshot
            && let Ok(directory) = entry.series.url.to_file_path()
            && let Ok(paths) = scan_series_sidecars(&directory)
        {
            for path in paths {
                if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                    continue;
                }
                let Ok(metadata) = fs::metadata(&path) else {
                    continue;
                };
                let Ok(url) = Url::from_file_path(path) else {
                    continue;
                };
                sidecars.push(ScannedSidecar {
                    url,
                    parent_url: entry.series.url.clone(),
                    last_modified_time: updated_time(&metadata),
                    sidecar_type: SidecarType::Artwork,
                    source: SidecarSource::Series,
                });
            }
        }
        for book in &entry.books {
            let Ok(path) = book.url.to_file_path() else {
                continue;
            };
            let Ok(paths) = scan_book_sidecars(&path) else {
                continue;
            };
            for path in paths {
                if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
                    continue;
                }
                let Ok(metadata) = fs::metadata(&path) else {
                    continue;
                };
                let Ok(url) = Url::from_file_path(path) else {
                    continue;
                };
                sidecars.push(ScannedSidecar {
                    url,
                    parent_url: book.url.clone(),
                    last_modified_time: updated_time(&metadata),
                    sidecar_type: SidecarType::Artwork,
                    source: SidecarSource::Book,
                });
            }
        }
    }
    sidecars.sort_by(|left, right| left.url.as_str().cmp(right.url.as_str()));
    Ok(ScanResult {
        series: scanned,
        sidecars,
    })
}

/// Projects one existing regular file into a new book identity.
///
/// Unlike a library scan this intentionally does not filter by extension,
/// matching Komga's import workflow. Returns `None` when the path cannot be
/// represented as a local regular file.
#[must_use]
pub fn scan_book_file(path: &Path, library_id: &str, oneshot: bool) -> Option<Book> {
    let metadata = fs::metadata(path).ok().filter(|value| value.is_file())?;
    let url = Url::from_file_path(path).ok()?;
    Some(Book {
        id: new_id(),
        name: path.file_stem()?.to_string_lossy().into_owned(),
        url,
        file_last_modified: updated_time(&metadata),
        file_size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
        file_hash: String::new(),
        file_hash_koreader: String::new(),
        number: 0,
        series_id: String::new(),
        library_id: library_id.to_owned(),
        deleted_date: None,
        oneshot,
        created_date: None,
        last_modified_date: None,
    })
}

/// Finds artwork files adjacent to one book using Komga's basename policy.
///
/// # Errors
///
/// Returns an I/O error when the book's parent directory cannot be listed.
pub fn scan_book_sidecars(book_path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let Some(parent) = book_path.parent() else {
        return Ok(Vec::new());
    };
    let Some(base) = book_path.file_stem().and_then(|value| value.to_str()) else {
        return Ok(Vec::new());
    };
    let base = base.to_ascii_lowercase();
    let mut result = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if (!file_type.is_file() && !file_type.is_symlink()) || !is_artwork_extension(&path) {
            continue;
        }
        let Some(candidate) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let candidate = candidate.to_ascii_lowercase();
        let suffix = candidate.strip_prefix(&base);
        if suffix.is_some_and(|value| value.is_empty() || is_numbered_suffix(value)) {
            result.push(path);
        }
    }
    result.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    Ok(result)
}

/// Finds artwork files in one series directory using Komga's filename policy.
///
/// # Errors
///
/// Returns an I/O error when the series directory cannot be listed.
pub fn scan_series_sidecars(series_path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(series_path)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if (!file_type.is_file() && !file_type.is_symlink()) || !is_artwork_extension(&path) {
            continue;
        }
        let Some(candidate) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if matches!(
            candidate.to_ascii_lowercase().as_str(),
            "cover" | "default" | "folder" | "poster" | "series"
        ) {
            result.push(path);
        }
    }
    result.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    Ok(result)
}

fn is_numbered_suffix(value: &str) -> bool {
    value
        .strip_prefix('-')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_artwork_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpeg" | "jpg" | "tbn" | "webp" | "gif"
    )
}

fn should_visit(path: &Path, exclusions: &[String]) -> bool {
    if is_hidden(path) {
        return false;
    }
    let candidate = path.to_string_lossy().to_lowercase();
    !exclusions.iter().any(|exclusion| candidate.contains(exclusion))
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn is_supported(path: &Path, library: &Library) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    (library.scan_cbx && matches!(extension.as_str(), "cbz" | "zip" | "cbr" | "rar"))
        || (library.scan_pdf && extension == "pdf")
        || (library.scan_epub && extension == "epub")
}

fn updated_time(metadata: &fs::Metadata) -> NaiveDateTime {
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    DateTime::<Local>::from(max(modified, created)).naive_local()
}

fn sort_key(value: &str) -> String {
    value
        .nfd()
        .filter(|character| !is_combining_mark(*character))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compares book names using Komga's accent-insensitive natural ordering.
pub fn compare_book_names(left: &str, right: &str) -> Ordering {
    natord::compare_ignore_case(&sort_key(left), &sort_key(right))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn library(root: &Path) -> Library {
        Library::new("Scan".into(), Url::from_directory_path(root).unwrap())
    }

    #[test]
    fn discovers_supported_books_and_applies_exclusions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series/.hidden")).unwrap();
        fs::create_dir_all(root.join("skip-this")).unwrap();
        fs::write(root.join("Series/Volume 10.cbz"), []).unwrap();
        fs::write(root.join("Series/Volume 2.CBZ"), []).unwrap();
        fs::write(root.join("Series/skip-this.cbz"), []).unwrap();
        fs::write(root.join("Series/book.txt"), []).unwrap();
        fs::write(root.join("Series/.hidden/hidden.cbz"), []).unwrap();
        fs::write(root.join("skip-this/skipped.pdf"), []).unwrap();
        let mut library = library(&root);
        library.scan_directory_exclusions.insert("SKIP-THIS".into());

        let result = scan_library_files(&library, || false).unwrap();

        assert_eq!(result.series.len(), 1);
        assert_eq!(result.series[0].series.name, "Series");
        assert_eq!(
            result.series[0]
                .books
                .iter()
                .map(|book| (book.name.as_str(), book.number))
                .collect::<Vec<_>>(),
            [("skip-this", 1), ("Volume 2", 2), ("Volume 10", 3)]
        );
    }

    #[test]
    fn creates_one_series_per_book_in_oneshot_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("One Shots")).unwrap();
        fs::write(root.join("One Shots/A.cbz"), []).unwrap();
        fs::write(root.join("One Shots/B.pdf"), []).unwrap();
        let mut library = library(&root);
        library.oneshots_directory = Some("one shots".into());

        let result = scan_library_files(&library, || false).unwrap();

        assert_eq!(result.series.len(), 2);
        assert!(result.series.iter().all(|entry| entry.series.oneshot));
        assert!(result.series.iter().all(|entry| entry.books[0].oneshot));
    }

    #[test]
    fn observes_cancellation_before_walking_entries() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("book.cbz"), []).unwrap();

        assert!(matches!(
            scan_library_files(&library(&root), || true),
            Err(ScanError::Cancelled)
        ));
    }

    #[test]
    fn discovers_book_artwork_sidecars_with_kotlin_basename_rules() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let book = series.join("Book 2.cbz");
        fs::write(&book, b"book").unwrap();
        fs::write(series.join("Book 2.jpg"), b"art").unwrap();
        fs::write(series.join("BOOK 2-1.webp"), b"art").unwrap();
        fs::write(series.join("Book 2-extra.jpg"), b"ignored").unwrap();
        fs::write(series.join("Other.jpg"), b"ignored").unwrap();

        let paths = scan_book_sidecars(&book).unwrap();
        assert_eq!(paths.len(), 2);
        let result = scan_library_files(&library(&root), || false).unwrap();
        let artwork = result
            .sidecars
            .iter()
            .filter(|sidecar| sidecar.sidecar_type == SidecarType::Artwork)
            .collect::<Vec<_>>();
        assert_eq!(artwork.len(), 2);
        assert!(artwork.iter().all(|sidecar| sidecar.source == SidecarSource::Book));
        assert!(
            artwork
                .iter()
                .all(|sidecar| sidecar.parent_url.as_str().ends_with("Book%202.cbz"))
        );
    }

    #[test]
    fn discovers_series_artwork_filename_matrix_case_insensitively_in_stable_order() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("Volume.cbz"), b"book").unwrap();
        for basename in ["cover", "default", "folder", "poster", "series"] {
            for extension in ["png", "jpeg", "jpg", "tbn", "webp", "gif"] {
                fs::write(series.join(format!("{basename}.{extension}")), b"art").unwrap();
            }
        }
        fs::write(series.join("COVER.JPG"), b"art").unwrap();
        for invalid in ["banner.jpg", "cover.bmp", "cover-1.jpg", "cover.jpg.txt"] {
            fs::write(series.join(invalid), b"ignored").unwrap();
        }

        let paths = scan_series_sidecars(&series).unwrap();
        assert_eq!(paths.len(), 31);
        assert!(
            paths
                .windows(2)
                .all(|paths| paths[0].as_os_str() <= paths[1].as_os_str())
        );

        let result = scan_library_files(&library(&root), || false).unwrap();
        let artwork = result
            .sidecars
            .iter()
            .filter(|sidecar| sidecar.source == SidecarSource::Series && sidecar.sidecar_type == SidecarType::Artwork)
            .collect::<Vec<_>>();
        assert_eq!(artwork.len(), 31);
        assert!(
            artwork
                .iter()
                .all(|sidecar| sidecar.parent_url == result.series[0].series.url)
        );
        assert!(
            artwork
                .windows(2)
                .all(|sidecars| sidecars[0].url.as_str() <= sidecars[1].url.as_str())
        );
    }

    #[test]
    fn excludes_series_artwork_for_oneshots_and_symbolic_links() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("One Shots");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("Volume.cbz"), b"book").unwrap();
        fs::write(series.join("cover.jpg"), b"art").unwrap();
        let mut config = library(&root);
        config.oneshots_directory = Some("one shots".into());

        let result = scan_library_files(&config, || false).unwrap();
        assert!(
            result
                .sidecars
                .iter()
                .all(|sidecar| sidecar.source != SidecarSource::Series)
        );

        #[cfg(unix)]
        {
            let regular = series.join("poster.jpg");
            let symlink = series.join("folder.jpg");
            fs::write(&regular, b"art").unwrap();
            std::os::unix::fs::symlink(&regular, &symlink).unwrap();
            config.oneshots_directory = None;
            let result = scan_library_files(&config, || false).unwrap();
            let series_artwork = result
                .sidecars
                .iter()
                .filter(|sidecar| {
                    sidecar.source == SidecarSource::Series && sidecar.sidecar_type == SidecarType::Artwork
                })
                .collect::<Vec<_>>();
            assert_eq!(series_artwork.len(), 2);
            assert!(
                series_artwork
                    .iter()
                    .all(|sidecar| !sidecar.url.path().ends_with("folder.jpg"))
            );
        }
    }
}
