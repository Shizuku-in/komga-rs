use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Read, Write},
    path::Path,
    rc::Rc,
};

use komga_core::{
    BookPage, Dimension, Media, MediaFile, MediaStatus, RAR_MEDIA_TYPE, RAR4_MEDIA_TYPE, RAR5_MEDIA_TYPE,
};
use rars::{Archive, ArchiveFamily, ArchiveReader};
use zip::ZipArchive;

use crate::{MediaOperationError, image_dimension};

const ANALYSIS_PREFIX_LIMIT: u64 = 1024 * 1024;

#[derive(Debug)]
struct AnalyzedEntry {
    name: String,
    media_type: Option<String>,
    dimension: Option<Dimension>,
    file_size: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct ContainerAnalysis {
    pages: Vec<BookPage>,
    files: Vec<MediaFile>,
    comment: Option<String>,
}

#[derive(Debug)]
pub(crate) struct RarAnalysis {
    pub(crate) media_type: &'static str,
    pub(crate) contents: ContainerAnalysis,
}

#[derive(Debug)]
pub(crate) enum ZipAnalysisError {
    Cancelled,
    Archive,
}

pub(crate) fn analyze_zip(
    path: &Path,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<ContainerAnalysis, ZipAnalysisError> {
    let file = File::open(path).map_err(|_| ZipAnalysisError::Archive)?;
    let mut archive = ZipArchive::new(file).map_err(|_| ZipAnalysisError::Archive)?;
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        if cancelled() {
            return Err(ZipAnalysisError::Cancelled);
        }
        let mut entry = archive.by_index(index).map_err(|_| ZipAnalysisError::Archive)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        let file_size = i64::try_from(entry.size()).ok();
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size().min(ANALYSIS_PREFIX_LIMIT)).unwrap_or(0));
        let (media_type, dimension) = match entry.by_ref().take(ANALYSIS_PREFIX_LIMIT).read_to_end(&mut bytes) {
            Ok(_) => {
                let media_type = detect_entry_media_type(&bytes);
                let dimension = if analyze_dimensions && media_type.as_deref().is_some_and(is_image_media_type) {
                    image_dimension(&bytes)
                } else {
                    None
                };
                (media_type, dimension)
            }
            Err(_) => (None, None),
        };
        entries.push(AnalyzedEntry {
            name,
            media_type,
            dimension,
            file_size,
        });
    }
    Ok(finalize_entries(entries))
}

pub(crate) fn media_from_analysis(book_id: &str, media_type: &str, analysis: ContainerAnalysis) -> Media {
    if analysis.pages.is_empty() {
        return Media::result(
            book_id,
            MediaStatus::Error,
            Some(media_type.to_owned()),
            Some("ERR_1006"),
        );
    }
    Media {
        status: MediaStatus::Ready,
        media_type: Some(media_type.to_owned()),
        page_count: i32::try_from(analysis.pages.len()).unwrap_or(i32::MAX),
        pages: analysis.pages,
        files: analysis.files,
        comment: analysis.comment,
        extension: None,
        book_id: book_id.to_owned(),
        epub_divina_compatible: false,
        epub_is_kepub: false,
        created_date: None,
        last_modified_date: None,
    }
}

fn finalize_entries(mut entries: Vec<AnalyzedEntry>) -> ContainerAnalysis {
    entries.sort_by(|left, right| natord::compare_ignore_case(&left.name, &right.name));
    let failed_names = entries
        .iter()
        .filter(|entry| entry.media_type.is_none())
        .map(|entry| entry.name.as_str())
        .collect::<Vec<_>>();
    let comment = (!failed_names.is_empty()).then(|| format!("ERR_1007 [{}]", failed_names.join(", ")));
    let mut pages = Vec::new();
    let mut files = Vec::new();
    for entry in entries {
        if entry.media_type.as_deref().is_some_and(is_image_media_type) {
            pages.push(BookPage {
                file_name: entry.name,
                media_type: entry.media_type.expect("image entry must have a media type"),
                dimension: entry.dimension,
                file_hash: String::new(),
                file_size: entry.file_size,
            });
        } else {
            files.push(MediaFile {
                file_name: entry.name,
                media_type: entry.media_type,
                sub_type: None,
                file_size: entry.file_size,
            });
        }
    }
    ContainerAnalysis { pages, files, comment }
}

#[derive(Debug)]
pub(crate) enum RarAnalysisError {
    Cancelled,
    Encrypted,
    MultiVolume,
    Archive,
}

struct RarEntryWriter {
    name: String,
    file_size: Option<i64>,
    analyze_dimensions: bool,
    prefix: Vec<u8>,
    entries: Rc<RefCell<Vec<AnalyzedEntry>>>,
}

impl Write for RarEntryWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let limit = usize::try_from(ANALYSIS_PREFIX_LIMIT).unwrap_or(usize::MAX);
        let remaining = limit.saturating_sub(self.prefix.len());
        self.prefix.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for RarEntryWriter {
    fn drop(&mut self) {
        let media_type = detect_entry_media_type(&self.prefix);
        let dimension = if self.analyze_dimensions && media_type.as_deref().is_some_and(is_image_media_type) {
            image_dimension(&self.prefix)
        } else {
            None
        };
        self.entries.borrow_mut().push(AnalyzedEntry {
            name: std::mem::take(&mut self.name),
            media_type,
            dimension,
            file_size: self.file_size,
        });
    }
}

pub(crate) fn analyze_rar(
    path: &Path,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<RarAnalysis, RarAnalysisError> {
    let archive = ArchiveReader::read_path(path).map_err(classify_rar_error)?;
    let media_type = rar_media_type(&archive).ok_or(RarAnalysisError::Archive)?;
    let members = archive.members().collect::<Vec<_>>();
    if members.iter().any(|member| member.meta.is_encrypted) {
        return Err(RarAnalysisError::Encrypted);
    }
    if rar_is_volume(&archive)
        || members
            .iter()
            .any(|member| member.meta.is_split_before || member.meta.is_split_after)
    {
        return Err(RarAnalysisError::MultiVolume);
    }

    let mut sizes = HashMap::<Vec<u8>, VecDeque<Option<i64>>>::new();
    for member in members.iter().filter(|member| !member.meta.is_directory) {
        sizes
            .entry(member.meta.name.clone())
            .or_default()
            .push_back(i64::try_from(member.meta.unpacked_size).ok());
    }
    let entries = Rc::new(RefCell::new(Vec::new()));
    archive
        .extract_to(None, |meta| {
            if cancelled() {
                return Err(rars::Error::Cancelled);
            }
            if meta.is_directory {
                return Ok(Box::new(io::sink()));
            }
            let file_size = sizes.get_mut(meta.name_bytes()).and_then(VecDeque::pop_front).flatten();
            Ok(Box::new(RarEntryWriter {
                name: meta.name_lossy(),
                file_size,
                analyze_dimensions,
                prefix: Vec::new(),
                entries: Rc::clone(&entries),
            }))
        })
        .map_err(classify_rar_error)?;
    if cancelled() {
        return Err(RarAnalysisError::Cancelled);
    }
    let entries = Rc::try_unwrap(entries)
        .map_err(|_| RarAnalysisError::Archive)?
        .into_inner();
    Ok(RarAnalysis {
        media_type,
        contents: finalize_entries(entries),
    })
}

pub(crate) fn extract_zip_entry(path: &Path, entry_name: &str) -> Result<Vec<u8>, MediaOperationError> {
    let file = File::open(path).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let mut archive = ZipArchive::new(file).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size().min(16 * 1024 * 1024)).unwrap_or(0));
    entry
        .read_to_end(&mut bytes)
        .map_err(|error| MediaOperationError::Read(error.to_string()))?;
    Ok(bytes)
}

struct SharedBytesWriter(Rc<RefCell<Vec<u8>>>);

impl Write for SharedBytesWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn extract_rar_entry(path: &Path, entry_name: &str) -> Result<Vec<u8>, MediaOperationError> {
    let archive = ArchiveReader::read_path(path).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    let bytes = Rc::new(RefCell::new(Vec::new()));
    let mut found = false;
    archive
        .extract_to(None, |meta| {
            if !found && !meta.is_directory && meta.name_lossy() == entry_name {
                found = true;
                Ok(Box::new(SharedBytesWriter(Rc::clone(&bytes))))
            } else {
                Ok(Box::new(io::sink()))
            }
        })
        .map_err(|error| MediaOperationError::Read(error.to_string()))?;
    if !found {
        return Err(MediaOperationError::Read(format!(
            "archive entry {entry_name:?} does not exist"
        )));
    }
    Rc::try_unwrap(bytes)
        .map_err(|_| MediaOperationError::Read("archive entry remained borrowed".to_owned()))
        .map(RefCell::into_inner)
}

pub(crate) fn normalize_media_type(value: &str) -> &str {
    match value {
        "application/vnd.rar" => RAR_MEDIA_TYPE,
        "image/x-png" => "image/png",
        "image/jpg" => "image/jpeg",
        _ => value,
    }
}

pub(crate) fn is_rar_media_type(value: &str) -> bool {
    matches!(
        value,
        RAR_MEDIA_TYPE | RAR4_MEDIA_TYPE | RAR5_MEDIA_TYPE | "application/vnd.rar"
    )
}

fn detect_entry_media_type(bytes: &[u8]) -> Option<String> {
    infer::get(bytes)
        .map(|kind| normalize_media_type(kind.mime_type()).to_owned())
        .or_else(|| {
            image::guess_format(bytes)
                .ok()
                .map(|format| format.to_mime_type().to_owned())
        })
        .or_else(|| {
            let text = std::str::from_utf8(bytes).ok()?;
            (!text.contains('\0')).then(|| {
                if text.trim_start().starts_with("<svg") {
                    "image/svg+xml".to_owned()
                } else {
                    "text/plain".to_owned()
                }
            })
        })
}

fn is_image_media_type(value: &str) -> bool {
    value.starts_with("image/")
}

fn classify_rar_error(error: rars::Error) -> RarAnalysisError {
    if rar_error_is_cancelled(&error) {
        RarAnalysisError::Cancelled
    } else if rar_error_is_encrypted(&error) {
        RarAnalysisError::Encrypted
    } else {
        RarAnalysisError::Archive
    }
}

pub(crate) fn rar_error_is_cancelled(error: &rars::Error) -> bool {
    match error {
        rars::Error::Cancelled => true,
        rars::Error::AtArchiveOffset { source, .. } | rars::Error::AtEntry { source, .. } => {
            rar_error_is_cancelled(source)
        }
        _ => false,
    }
}

fn rar_error_is_encrypted(error: &rars::Error) -> bool {
    match error {
        rars::Error::NeedPassword
        | rars::Error::WrongPasswordOrCorruptData
        | rars::Error::UnsupportedEncryption { .. } => true,
        rars::Error::AtArchiveOffset { source, .. } | rars::Error::AtEntry { source, .. } => {
            rar_error_is_encrypted(source)
        }
        _ => false,
    }
}

fn rar_media_type(archive: &Archive) -> Option<&'static str> {
    match archive.family() {
        ArchiveFamily::Rar15To40 => Some(RAR4_MEDIA_TYPE),
        ArchiveFamily::Rar50Plus => Some(RAR5_MEDIA_TYPE),
        ArchiveFamily::Rar13 => None,
        _ => None,
    }
}

fn rar_is_volume(archive: &Archive) -> bool {
    match archive {
        Archive::Rar15To40(archive) => archive.main.is_volume(),
        Archive::Rar50Plus(archive) => archive.main.is_volume(),
        Archive::Rar13(_) => false,
        _ => false,
    }
}
