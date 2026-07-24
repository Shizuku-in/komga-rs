//! Media analysis and image operations independent from HTTP and persistence.

use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    fs::File,
    io::{self, Cursor, Read, Write},
    path::Path,
    rc::Rc,
};

use chrono::NaiveDateTime;
use hayro::{
    RenderCache, RenderSettings,
    hayro_interpret::InterpreterSettings,
    hayro_syntax::{Pdf, page::Page},
    render,
    vello_cpu::color::palette::css::WHITE,
};
use hayro_write::{ChunkSettings, ExtractionQuery};
use image::{
    DynamicImage, GenericImageView, ImageFormat, ImageReader, Rgb, RgbImage, codecs::jpeg::JpegEncoder,
    imageops::FilterType,
};
use pdf_writer::Ref;
use rars::{Archive, ArchiveFamily, ArchiveReader};
use serde::Serialize;
use thiserror::Error;
use zip::ZipArchive;

use crate::Book;

const ANALYSIS_PREFIX_LIMIT: u64 = 1024 * 1024;
const PDF_MEDIA_TYPE: &str = "application/pdf";
const PDF_IMAGE_MEDIA_TYPE: &str = "image/jpeg";
const PDF_RENDER_SHORT_EDGE: f32 = 3200.0;
const PDF_JPEG_QUALITY: u8 = 75;

/// Width and height in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimension {
    /// Horizontal pixel count.
    pub width: i32,
    /// Vertical pixel count.
    pub height: i32,
}

/// One naturally ordered image page found in a book container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookPage {
    /// Exact entry name inside the container.
    pub file_name: String,
    /// Detected page media type.
    pub media_type: String,
    /// Optional dimensions controlled by the library analysis setting.
    pub dimension: Option<Dimension>,
    /// Page hash, empty until page hashing is ported.
    pub file_hash: String,
    /// Uncompressed entry size when available.
    pub file_size: Option<i64>,
}

/// A non-page file found in a book container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFile {
    /// Exact entry name inside the container.
    pub file_name: String,
    /// Detected media type, if detection succeeded.
    pub media_type: Option<String>,
    /// Kotlin EPUB subtype; absent for CBZ files.
    pub sub_type: Option<String>,
    /// Uncompressed entry size when available.
    pub file_size: Option<i64>,
}

/// Lifecycle state of analyzed media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaStatus {
    /// The book has never been analyzed.
    Unknown,
    /// Analysis failed.
    Error,
    /// Analysis completed and pages can be served.
    Ready,
    /// The detected container is not implemented.
    Unsupported,
    /// The source file changed after its previous analysis.
    Outdated,
}

impl MediaStatus {
    /// Returns the identifier stored by Komga's schema.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Error => "ERROR",
            Self::Ready => "READY",
            Self::Unsupported => "UNSUPPORTED",
            Self::Outdated => "OUTDATED",
        }
    }
}

impl TryFrom<&str> for MediaStatus {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, String> {
        match value {
            "UNKNOWN" => Ok(Self::Unknown),
            "ERROR" => Ok(Self::Error),
            "READY" => Ok(Self::Ready),
            "UNSUPPORTED" => Ok(Self::Unsupported),
            "OUTDATED" => Ok(Self::Outdated),
            _ => Err(format!("invalid media status {value:?}")),
        }
    }
}

/// Complete analyzed-media record for one book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    /// Analysis state.
    pub status: MediaStatus,
    /// Detected container media type.
    pub media_type: Option<String>,
    /// Naturally ordered image pages.
    pub pages: Vec<BookPage>,
    /// Stored count, retained separately for error and future PDF records.
    pub page_count: i32,
    /// Non-page files found in the container.
    pub files: Vec<MediaFile>,
    /// Kotlin-compatible analysis error or warning code.
    pub comment: Option<String>,
    /// Owning book identifier.
    pub book_id: String,
    /// Whether a future EPUB can be served with the Divina profile.
    pub epub_divina_compatible: bool,
    /// Whether a future EPUB is already KEPUB-formatted.
    pub epub_is_kepub: bool,
    /// Persisted creation timestamp when loaded from storage.
    pub created_date: Option<NaiveDateTime>,
    /// Persisted modification timestamp when loaded from storage.
    pub last_modified_date: Option<NaiveDateTime>,
}

impl Media {
    fn result(book_id: &str, status: MediaStatus, media_type: Option<String>, comment: Option<&str>) -> Self {
        Self {
            status,
            media_type,
            pages: Vec::new(),
            page_count: 0,
            files: Vec::new(),
            comment: comment.map(str::to_owned),
            book_id: book_id.to_owned(),
            epub_divina_compatible: false,
            epub_is_kepub: false,
            created_date: None,
            last_modified_date: None,
        }
    }
}

/// Generated or selected book poster stored in `THUMBNAIL_BOOK`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailBook {
    /// Stable thumbnail identifier.
    pub id: String,
    /// Inline thumbnail bytes, if database-backed.
    pub thumbnail: Option<Vec<u8>>,
    /// Sidecar file URL, if filesystem-backed.
    pub url: Option<String>,
    /// Whether clients should display this thumbnail.
    pub selected: bool,
    /// Kotlin thumbnail type such as `GENERATED`.
    pub thumbnail_type: String,
    /// Encoded image media type.
    pub media_type: String,
    /// Encoded byte length.
    pub file_size: i64,
    /// Encoded image dimensions.
    pub dimension: Dimension,
    /// Owning book identifier.
    pub book_id: String,
}

/// Page representation returned by Komga's REST API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageDto {
    /// One-based page number.
    pub number: i32,
    /// Exact container entry name.
    pub file_name: String,
    /// Detected image media type.
    pub media_type: String,
    /// Optional width in pixels.
    pub width: Option<i32>,
    /// Optional height in pixels.
    pub height: Option<i32>,
    /// Optional uncompressed byte count.
    pub size_bytes: Option<i64>,
    /// Human-readable IEC byte count.
    pub size: String,
}

impl PageDto {
    /// Converts a stored page and zero-based index into an API representation.
    pub fn from_page(index: usize, page: &BookPage) -> Self {
        Self {
            number: i32::try_from(index + 1).unwrap_or(i32::MAX),
            file_name: page.file_name.clone(),
            media_type: page.media_type.clone(),
            width: page.dimension.map(|value| value.width),
            height: page.dimension.map(|value| value.height),
            size_bytes: page.file_size,
            size: page.file_size.map(binary_size).unwrap_or_default(),
        }
    }
}

/// Converts all persisted pages into the representation exposed by the REST API.
///
/// PDF pages store their crop-box dimensions and no image media type. Their API
/// representation describes the JPEG raster that page delivery produces.
pub fn media_page_dtos(media: &Media) -> Vec<PageDto> {
    let pdf = media.media_type.as_deref() == Some(PDF_MEDIA_TYPE);
    media
        .pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            let mut dto = PageDto::from_page(index, page);
            if pdf {
                dto.media_type = PDF_IMAGE_MEDIA_TYPE.to_owned();
                let dimension = page.dimension.and_then(pdf_output_dimension);
                dto.width = dimension.map(|value| value.width);
                dto.height = dimension.map(|value| value.height);
            }
            dto
        })
        .collect()
}

/// Binary content paired with the media type used by HTTP responses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedBytes {
    /// Encoded content.
    pub bytes: Vec<u8>,
    /// IANA media type for the encoded content.
    pub media_type: String,
}

/// Supported image output encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOutput {
    /// JPEG with a white background for transparent input.
    Jpeg,
    /// PNG preserving transparency.
    Png,
}

impl ImageOutput {
    /// Parses the `convert` query parameter accepted by Komga.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

/// Errors that interrupt analysis without producing a persisted media result.
#[derive(Debug, Error)]
pub enum MediaAnalysisError {
    /// Server shutdown was requested between archive entries.
    #[error("media analysis cancelled")]
    Cancelled,
}

/// Errors produced while reading or converting an already analyzed page.
#[derive(Debug, Error)]
pub enum MediaOperationError {
    /// The media row is not ready for page access.
    #[error("book media is not ready")]
    NotReady,
    /// The requested one-based page number is outside the book.
    #[error("page number does not exist")]
    PageOutOfBounds,
    /// The source file or archive entry could not be read.
    #[error("could not read book content: {0}")]
    Read(String),
    /// The source image cannot be decoded or encoded.
    #[error("could not convert image: {0}")]
    Image(String),
}

#[derive(Debug)]
struct AnalyzedEntry {
    name: String,
    media_type: Option<String>,
    dimension: Option<Dimension>,
    file_size: Option<i64>,
}

#[derive(Debug)]
struct ContainerAnalysis {
    pages: Vec<BookPage>,
    files: Vec<MediaFile>,
    comment: Option<String>,
}

#[derive(Debug)]
struct RarAnalysis {
    media_type: &'static str,
    contents: ContainerAnalysis,
}

/// Detects and analyzes a book using Kotlin-compatible media state transitions.
///
/// ZIP/CBZ, RAR/CBR, and PDF books are analyzed without external commands.
/// Recognized EPUB and other formats remain unsupported until their extractors
/// are introduced.
///
/// # Errors
///
/// Returns [`MediaAnalysisError::Cancelled`] when the callback requests stop.
pub fn analyze_book(
    book: &Book,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Media, MediaAnalysisError> {
    if cancelled() {
        return Err(MediaAnalysisError::Cancelled);
    }
    let path = match book.url.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            return Ok(Media::result(&book.id, MediaStatus::Error, None, Some("ERR_1005")));
        }
    };
    let detected = match infer::get_from_path(&path) {
        Ok(Some(kind)) => normalize_media_type(kind.mime_type()).to_owned(),
        Ok(None) => "application/octet-stream".to_owned(),
        Err(error) => {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => "ERR_1018",
                std::io::ErrorKind::PermissionDenied => "ERR_1000",
                _ => "ERR_1005",
            };
            return Ok(Media::result(&book.id, MediaStatus::Error, None, Some(code)));
        }
    };

    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("epub"))
    {
        return Ok(if detected == "application/epub+zip" {
            Media::result(&book.id, MediaStatus::Unsupported, Some(detected), Some("ERR_1001"))
        } else {
            Media::result(&book.id, MediaStatus::Error, Some(detected), Some("ERR_1032"))
        });
    }

    if detected == PDF_MEDIA_TYPE {
        return match analyze_pdf(&path, &book.id, analyze_dimensions, cancelled) {
            Ok(media) => Ok(media),
            Err(PdfAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(PdfAnalysisError::Io(kind)) => {
                let code = match kind {
                    io::ErrorKind::NotFound => "ERR_1018",
                    io::ErrorKind::PermissionDenied => "ERR_1000",
                    _ => "ERR_1005",
                };
                Ok(Media::result(
                    &book.id,
                    MediaStatus::Error,
                    Some(PDF_MEDIA_TYPE.to_owned()),
                    Some(code),
                ))
            }
            Err(PdfAnalysisError::Invalid) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(PDF_MEDIA_TYPE.to_owned()),
                Some("ERR_1005"),
            )),
        };
    }

    if detected == "application/zip" {
        return match analyze_zip(&path, analyze_dimensions, cancelled) {
            Ok(analysis) => Ok(media_from_analysis(&book.id, "application/zip", analysis)),
            Err(ZipAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(ZipAnalysisError::Archive) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some("application/zip".to_owned()),
                Some("ERR_1008"),
            )),
        };
    }

    if is_rar_media_type(&detected) {
        return match analyze_rar(&path, analyze_dimensions, cancelled) {
            Ok(analysis) => Ok(media_from_analysis(&book.id, analysis.media_type, analysis.contents)),
            Err(RarAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(RarAnalysisError::Encrypted) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(detected),
                Some("ERR_1002"),
            )),
            Err(RarAnalysisError::MultiVolume) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(detected),
                Some("ERR_1004"),
            )),
            Err(RarAnalysisError::Archive) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(detected),
                Some("ERR_1008"),
            )),
        };
    }

    Ok(Media::result(
        &book.id,
        MediaStatus::Unsupported,
        Some(detected),
        Some("ERR_1001"),
    ))
}

#[derive(Debug)]
enum PdfAnalysisError {
    Cancelled,
    Io(io::ErrorKind),
    Invalid,
}

fn analyze_pdf(
    path: &Path,
    book_id: &str,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Media, PdfAnalysisError> {
    let bytes = std::fs::read(path).map_err(|error| PdfAnalysisError::Io(error.kind()))?;
    if cancelled() {
        return Err(PdfAnalysisError::Cancelled);
    }
    let pdf = Pdf::new(bytes).map_err(|_| PdfAnalysisError::Invalid)?;
    let mut pages = Vec::with_capacity(pdf.pages().len());
    for (index, page) in pdf.pages().iter().enumerate() {
        if cancelled() {
            return Err(PdfAnalysisError::Cancelled);
        }
        pages.push(BookPage {
            file_name: (index + 1).to_string(),
            media_type: String::new(),
            dimension: if analyze_dimensions {
                dimension_from_pdf_page(page)
            } else {
                None
            },
            file_hash: String::new(),
            file_size: None,
        });
    }
    Ok(Media {
        status: MediaStatus::Ready,
        media_type: Some(PDF_MEDIA_TYPE.to_owned()),
        page_count: i32::try_from(pages.len()).unwrap_or(i32::MAX),
        pages,
        files: Vec::new(),
        comment: None,
        book_id: book_id.to_owned(),
        epub_divina_compatible: false,
        epub_is_kepub: false,
        created_date: None,
        last_modified_date: None,
    })
}

fn dimension_from_pdf_page(page: &Page<'_>) -> Option<Dimension> {
    let crop_box = page.crop_box();
    Some(Dimension {
        width: rounded_i32(crop_box.width())?,
        height: rounded_i32(crop_box.height())?,
    })
}

fn rounded_i32(value: f64) -> Option<i32> {
    let rounded = value.round();
    (rounded.is_finite() && rounded >= f64::from(i32::MIN) && rounded <= f64::from(i32::MAX)).then_some(rounded as i32)
}

#[derive(Debug)]
enum ZipAnalysisError {
    Cancelled,
    Archive,
}

fn analyze_zip(
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

fn media_from_analysis(book_id: &str, media_type: &str, analysis: ContainerAnalysis) -> Media {
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
        book_id: book_id.to_owned(),
        epub_divina_compatible: false,
        epub_is_kepub: false,
        created_date: None,
        last_modified_date: None,
    }
}

#[derive(Debug)]
enum RarAnalysisError {
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

fn analyze_rar(
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

fn classify_rar_error(error: rars::Error) -> RarAnalysisError {
    if rar_error_is_cancelled(&error) {
        RarAnalysisError::Cancelled
    } else if rar_error_is_encrypted(&error) {
        RarAnalysisError::Encrypted
    } else {
        RarAnalysisError::Archive
    }
}

fn rar_error_is_cancelled(error: &rars::Error) -> bool {
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
        ArchiveFamily::Rar15To40 => Some("application/x-rar-compressed; version=4"),
        ArchiveFamily::Rar50Plus => Some("application/x-rar-compressed; version=5"),
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

/// Extracts one rasterized page from a ready ZIP-, RAR-, or PDF-backed record.
///
/// # Errors
///
/// Returns an error for non-ready media, invalid page numbers, missing files,
/// encrypted entries, unsupported compression methods, or corrupt documents.
pub fn extract_page(book: &Book, media: &Media, page_number: i32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Err(MediaOperationError::NotReady);
    }
    if page_number <= 0 {
        return Err(MediaOperationError::PageOutOfBounds);
    }
    let page = media
        .pages
        .get(usize::try_from(page_number - 1).map_err(|_| MediaOperationError::PageOutOfBounds)?)
        .ok_or(MediaOperationError::PageOutOfBounds)?;
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    if media.media_type.as_deref() == Some(PDF_MEDIA_TYPE) {
        return render_pdf_page(&path, usize::try_from(page_number - 1).unwrap_or(usize::MAX));
    }
    let bytes = match media.media_type.as_deref() {
        Some("application/zip") => extract_zip_entry(&path, &page.file_name)?,
        Some(media_type) if is_rar_media_type(media_type) => extract_rar_entry(&path, &page.file_name)?,
        _ => return Err(MediaOperationError::NotReady),
    };
    Ok(TypedBytes {
        bytes,
        media_type: page.media_type.clone(),
    })
}

/// Extracts one PDF page as a standalone PDF document.
///
/// # Errors
///
/// Returns an error when media is not a ready PDF, the page is out of bounds,
/// or the source document cannot be read or rewritten.
pub fn extract_raw_page(book: &Book, media: &Media, page_number: i32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready || media.media_type.as_deref() != Some(PDF_MEDIA_TYPE) {
        return Err(MediaOperationError::NotReady);
    }
    if page_number <= 0 {
        return Err(MediaOperationError::PageOutOfBounds);
    }
    let index = usize::try_from(page_number - 1).map_err(|_| MediaOperationError::PageOutOfBounds)?;
    media.pages.get(index).ok_or(MediaOperationError::PageOutOfBounds)?;
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    let pdf = load_pdf(&path)?;
    let bytes = write_pdf_pages(&pdf, &[index])?;
    Ok(TypedBytes {
        bytes,
        media_type: PDF_MEDIA_TYPE.to_owned(),
    })
}

fn load_pdf(path: &Path) -> Result<Pdf, MediaOperationError> {
    let bytes = std::fs::read(path).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    Pdf::new(bytes).map_err(|error| MediaOperationError::Read(format!("invalid PDF: {error:?}")))
}

fn render_pdf_page(path: &Path, index: usize) -> Result<TypedBytes, MediaOperationError> {
    let pdf = load_pdf(path)?;
    let page = pdf.pages().get(index).ok_or(MediaOperationError::PageOutOfBounds)?;
    let scale = pdf_render_scale(page)
        .ok_or_else(|| MediaOperationError::Image("PDF page has invalid or unsupported dimensions".to_owned()))?;
    let pixmap = render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
            ..Default::default()
        },
    );
    let width = u32::from(pixmap.width());
    let height = u32::from(pixmap.height());
    let rgba = pixmap.take_unpremultiplied();
    let mut rgb = Vec::with_capacity(rgba.len().saturating_mul(3));
    for pixel in rgba {
        rgb.extend_from_slice(&[pixel.r, pixel.g, pixel.b]);
    }
    let image = RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| MediaOperationError::Image("PDF renderer returned invalid pixel data".to_owned()))?;
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, PDF_JPEG_QUALITY)
        .encode_image(&image)
        .map_err(|error| MediaOperationError::Image(error.to_string()))?;
    Ok(TypedBytes {
        bytes,
        media_type: PDF_IMAGE_MEDIA_TYPE.to_owned(),
    })
}

fn pdf_render_scale(page: &Page<'_>) -> Option<f32> {
    let crop_box = page.crop_box();
    let crop_width = crop_box.width() as f32;
    let crop_height = crop_box.height() as f32;
    let (render_width, render_height) = page.render_dimensions();
    pdf_scale(crop_width, crop_height, render_width, render_height)
}

fn pdf_scale(crop_width: f32, crop_height: f32, render_width: f32, render_height: f32) -> Option<f32> {
    let values = [crop_width, crop_height, render_width, render_height];
    if values.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        return None;
    }
    let requested = PDF_RENDER_SHORT_EDGE / crop_width.min(crop_height);
    let viewport_limit = f32::from(u16::MAX) / render_width.max(render_height);
    Some(requested.min(viewport_limit))
}

fn pdf_output_dimension(dimension: Dimension) -> Option<Dimension> {
    let width = dimension.width as f32;
    let height = dimension.height as f32;
    let scale = pdf_scale(width, height, width, height)?;
    Some(Dimension {
        width: rounded_i32(f64::from(width * scale))?,
        height: rounded_i32(f64::from(height * scale))?,
    })
}

fn write_pdf_pages(pdf: &Pdf, page_indices: &[usize]) -> Result<Vec<u8>, MediaOperationError> {
    let mut output = pdf_writer::Pdf::new();
    let mut next_ref = Ref::new(1);
    let catalog_ref = next_ref.bump();
    let queries = page_indices
        .iter()
        .copied()
        .map(ExtractionQuery::new_page)
        .collect::<Vec<_>>();
    let extracted = hayro_write::extract(
        pdf,
        Box::new(|| next_ref.bump()),
        ChunkSettings::default(),
        |_| unreachable!("page extraction does not write XObjects"),
        &queries,
    )
    .map_err(|error| MediaOperationError::Read(format!("could not extract PDF page: {error:?}")))?;
    let page_refs = extracted
        .root_refs
        .iter()
        .copied()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MediaOperationError::Read(format!("could not extract PDF page: {error:?}")))?;
    output.catalog(catalog_ref).pages(extracted.page_tree_parent_ref);
    output
        .pages(extracted.page_tree_parent_ref)
        .kids(page_refs.iter().copied())
        .count(i32::try_from(page_refs.len()).unwrap_or(i32::MAX));
    output.extend(&extracted.chunk);
    Ok(output.finish())
}

fn extract_zip_entry(path: &Path, entry_name: &str) -> Result<Vec<u8>, MediaOperationError> {
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

fn extract_rar_entry(path: &Path, entry_name: &str) -> Result<Vec<u8>, MediaOperationError> {
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

/// Converts and optionally downsizes encoded image bytes.
///
/// `max_edge` preserves aspect ratio and never enlarges the source image.
///
/// # Errors
///
/// Returns [`MediaOperationError::Image`] when decoding or encoding fails.
pub fn convert_image(
    source: &[u8],
    output: ImageOutput,
    max_edge: Option<u32>,
) -> Result<TypedBytes, MediaOperationError> {
    let reader = ImageReader::new(Cursor::new(source))
        .with_guessed_format()
        .map_err(|error| MediaOperationError::Image(error.to_string()))?;
    let mut image = reader
        .decode()
        .map_err(|error| MediaOperationError::Image(error.to_string()))?;
    if let Some(max_edge) = max_edge {
        let (width, height) = image.dimensions();
        if width.max(height) > max_edge {
            image = image.resize(max_edge, max_edge, FilterType::Lanczos3);
        }
    }

    let (image, format, media_type) = match output {
        ImageOutput::Jpeg => (
            DynamicImage::ImageRgb8(flatten_alpha(&image)),
            ImageFormat::Jpeg,
            "image/jpeg",
        ),
        ImageOutput::Png => (image, ImageFormat::Png, "image/png"),
    };
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, format)
        .map_err(|error| MediaOperationError::Image(error.to_string()))?;
    Ok(TypedBytes {
        bytes: bytes.into_inner(),
        media_type: media_type.to_owned(),
    })
}

/// Generates a JPEG poster from the first page of a ready book.
///
/// # Errors
///
/// Returns page extraction or image conversion errors unchanged.
pub fn generate_thumbnail(book: &Book, media: &Media, max_edge: u32) -> Result<TypedBytes, MediaOperationError> {
    let cover = extract_page(book, media, 1)?;
    convert_image(&cover.bytes, ImageOutput::Jpeg, Some(max_edge))
}

fn flatten_alpha(image: &DynamicImage) -> RgbImage {
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let pixel = rgba.get_pixel(x, y).0;
        let alpha = u16::from(pixel[3]);
        let blend = |channel: u8| -> u8 {
            u8::try_from((u16::from(channel) * alpha + 255 * (255 - alpha)) / 255).unwrap_or(255)
        };
        Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])])
    })
}

/// Reads the first image frame's dimensions without decoding all pixels.
pub fn image_dimension(bytes: &[u8]) -> Option<Dimension> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(Dimension {
        width: i32::try_from(width).ok()?,
        height: i32::try_from(height).ok()?,
    })
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

fn normalize_media_type(value: &str) -> &str {
    match value {
        "application/vnd.rar" => "application/x-rar-compressed",
        "image/x-png" => "image/png",
        "image/jpg" => "image/jpeg",
        _ => value,
    }
}

fn is_rar_media_type(value: &str) -> bool {
    matches!(
        value,
        "application/x-rar-compressed"
            | "application/x-rar-compressed; version=4"
            | "application/x-rar-compressed; version=5"
            | "application/vnd.rar"
    )
}

fn is_image_media_type(value: &str) -> bool {
    value.starts_with("image/")
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
        format!("{bytes} {}", UNITS[unit])
    } else {
        let rounded = (value * 10.0).round() / 10.0;
        if rounded.fract() == 0.0 {
            format!("{} {}", rounded as i64, UNITS[unit])
        } else {
            format!("{rounded:.1} {}", UNITS[unit])
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write, path::PathBuf};

    use rars::{
        ArchiveVersion, FeatureSet,
        rar15_40::{StoredEntry, WriterOptions, write_stored_volumes},
    };
    use url::Url;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn png() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(48, 32, image::Rgba([10, 20, 30, 128])));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    fn book(path: &Path) -> Book {
        Book {
            id: "book".into(),
            name: "Volume".into(),
            url: Url::from_file_path(path).unwrap(),
            file_last_modified: chrono::Local::now().naive_local(),
            file_size: 0,
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

    fn kotlin_archive(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/archives")
            .join(name)
    }

    fn kotlin_pdf() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/pdf/komga.pdf")
    }

    fn blank_pdf(page_sizes: &[(f32, f32)]) -> Vec<u8> {
        let mut pdf = pdf_writer::Pdf::new();
        let catalog_ref = Ref::new(1);
        let pages_ref = Ref::new(2);
        let page_refs = (0..page_sizes.len())
            .map(|index| Ref::new(i32::try_from(index + 3).unwrap()))
            .collect::<Vec<_>>();
        pdf.catalog(catalog_ref).pages(pages_ref);
        pdf.pages(pages_ref)
            .kids(page_refs.iter().copied())
            .count(i32::try_from(page_refs.len()).unwrap());
        for (page_ref, (width, height)) in page_refs.into_iter().zip(page_sizes.iter().copied()) {
            let mut page = pdf.page(page_ref);
            page.parent(pages_ref);
            page.media_box(pdf_writer::Rect::new(0.0, 0.0, width, height));
            page.resources();
        }
        pdf.finish()
    }

    #[test]
    fn analyzes_naturally_sorted_zip_pages_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default();
        writer.start_file("10.png", options).unwrap();
        writer.write_all(&png()).unwrap();
        writer.start_file("notes.txt", options).unwrap();
        writer.write_all(b"notes").unwrap();
        writer.start_file("2.png", options).unwrap();
        writer.write_all(&png()).unwrap();
        writer.finish().unwrap();

        let media = analyze_book(&book(&path), true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.page_count, 2);
        assert_eq!(media.pages[0].file_name, "2.png");
        assert_eq!(media.pages[0].dimension, Some(Dimension { width: 48, height: 32 }));
        assert_eq!(media.pages[1].file_name, "10.png");
        assert_eq!(media.files[0].file_name, "notes.txt");
    }

    #[test]
    fn reports_empty_and_missing_books_with_kotlin_codes() {
        let temporary = tempfile::tempdir().unwrap();
        let empty_path = temporary.path().join("empty.cbz");
        let mut writer = ZipWriter::new(File::create(&empty_path).unwrap());
        writer.start_file("notes.txt", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"notes").unwrap();
        writer.finish().unwrap();
        let empty = analyze_book(&book(&empty_path), true, || false).unwrap();
        assert_eq!(empty.status, MediaStatus::Error);
        assert_eq!(empty.comment.as_deref(), Some("ERR_1006"));

        let missing = temporary.path().join("missing.cbz");
        let missing = analyze_book(&book(&missing), true, || false).unwrap();
        assert_eq!(missing.status, MediaStatus::Error);
        assert_eq!(missing.comment.as_deref(), Some("ERR_1018"));
    }

    #[test]
    fn converts_transparent_pages_without_upscaling() {
        let converted = convert_image(&png(), ImageOutput::Jpeg, Some(300)).unwrap();
        let decoded = image::load_from_memory(&converted.bytes).unwrap();
        assert_eq!(converted.media_type, "image/jpeg");
        assert_eq!(decoded.dimensions(), (48, 32));
        assert!(!decoded.color().has_alpha());
    }

    #[test]
    fn cancellation_stops_before_opening_the_archive() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        fs::write(&path, []).unwrap();
        assert!(matches!(
            analyze_book(&book(&path), true, || true),
            Err(MediaAnalysisError::Cancelled)
        ));
    }

    #[test]
    fn reads_kotlin_zip_compression_fixtures() {
        for name in [
            "zip.zip",
            "zip-bzip2.zip",
            "zip-copy.zip",
            "zip-deflate64.zip",
            "zip-lzma.zip",
            "zip-ppmd.zip",
        ] {
            let media = analyze_book(&book(&kotlin_archive(name)), false, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Ready, "{name}");
            assert_eq!(media.page_count, 1, "{name}");
            assert_eq!(media.pages[0].dimension, None, "{name}");
        }

        let encrypted = analyze_book(&book(&kotlin_archive("zip-encrypted.zip")), false, || false).unwrap();
        assert_eq!(encrypted.status, MediaStatus::Error);
    }

    #[test]
    fn reads_kotlin_rar_and_solid_fixtures() {
        for (name, media_type, page_count) in [
            ("rar4.rar", "application/x-rar-compressed; version=4", 3),
            ("rar5.rar", "application/x-rar-compressed; version=5", 1),
            ("rar4-solid.rar", "application/x-rar-compressed; version=4", 3),
            ("rar5-solid.rar", "application/x-rar-compressed; version=5", 3),
        ] {
            let source = book(&kotlin_archive(name));
            let media = analyze_book(&source, true, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Ready, "{name}");
            assert_eq!(media.media_type.as_deref(), Some(media_type), "{name}");
            assert_eq!(media.page_count, page_count, "{name}");
            assert!(
                media
                    .pages
                    .iter()
                    .all(|page| page.dimension == Some(Dimension { width: 48, height: 48 })),
                "{name}"
            );
            let extracted = extract_page(&source, &media, page_count).unwrap();
            assert_eq!(extracted.media_type, "image/png", "{name}");
            assert!(extracted.bytes.starts_with(b"\x89PNG"), "{name}");
        }

        let source = book(&kotlin_archive("rar4.rar"));
        let media = analyze_book(&source, false, || false).unwrap();
        assert!(media.pages.iter().all(|page| page.dimension.is_none()));
        assert_eq!(media.pages[0].file_name, "komga-1.png");
        assert_eq!(media.pages[0].file_size, Some(3108));
    }

    #[test]
    fn analyzes_and_renders_kotlin_pdf_fixture() {
        let source = book(&kotlin_pdf());
        let media = analyze_book(&source, true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(PDF_MEDIA_TYPE));
        assert_eq!(media.page_count, 1);
        assert_eq!(media.pages[0].file_name, "1");
        assert_eq!(media.pages[0].media_type, "");
        assert_eq!(
            media.pages[0].dimension,
            Some(Dimension {
                width: 512,
                height: 512
            })
        );
        assert_eq!(media.pages[0].file_size, None);

        let dtos = media_page_dtos(&media);
        assert_eq!(dtos[0].media_type, PDF_IMAGE_MEDIA_TYPE);
        assert_eq!((dtos[0].width, dtos[0].height), (Some(3200), Some(3200)));
        assert_eq!(dtos[0].size_bytes, None);
        assert_eq!(dtos[0].size, "");

        let rendered = extract_page(&source, &media, 1).unwrap();
        assert_eq!(rendered.media_type, PDF_IMAGE_MEDIA_TYPE);
        assert!(rendered.bytes.starts_with(&[0xff, 0xd8]));
        assert_eq!(
            image::load_from_memory(&rendered.bytes).unwrap().dimensions(),
            (3200, 3200)
        );

        let raw = extract_raw_page(&source, &media, 1).unwrap();
        assert_eq!(raw.media_type, PDF_MEDIA_TYPE);
        assert!(raw.bytes.starts_with(b"%PDF-"));
        assert_eq!(Pdf::new(raw.bytes).unwrap().pages().len(), 1);
    }

    #[test]
    fn pdf_dimensions_can_be_skipped_and_invalid_documents_report_kotlin_error() {
        let source = book(&kotlin_pdf());
        let media = analyze_book(&source, false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.pages[0].dimension, None);
        assert_eq!(media_page_dtos(&media)[0].width, None);

        let temporary = tempfile::tempdir().unwrap();
        let invalid_path = temporary.path().join("invalid.pdf");
        fs::write(&invalid_path, b"%PDF-1.7\ninvalid").unwrap();
        let invalid = analyze_book(&book(&invalid_path), true, || false).unwrap();
        assert_eq!(invalid.status, MediaStatus::Error);
        assert_eq!(invalid.media_type.as_deref(), Some(PDF_MEDIA_TYPE));
        assert_eq!(invalid.comment.as_deref(), Some("ERR_1005"));
    }

    #[test]
    fn extracts_the_requested_page_as_a_standalone_pdf() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("pages.pdf");
        fs::write(&path, blank_pdf(&[(100.0, 200.0), (300.0, 400.0)])).unwrap();
        let source = book(&path);
        let media = analyze_book(&source, true, || false).unwrap();
        assert_eq!(media.page_count, 2);

        let raw = extract_raw_page(&source, &media, 2).unwrap();
        let extracted = Pdf::new(raw.bytes).unwrap();
        assert_eq!(extracted.pages().len(), 1);
        let crop_box = extracted.pages()[0].crop_box();
        assert_eq!((crop_box.width(), crop_box.height()), (300.0, 400.0));
        assert!(matches!(
            extract_raw_page(&source, &media, 3),
            Err(MediaOperationError::PageOutOfBounds)
        ));
    }

    #[test]
    fn rejects_encrypted_and_multivolume_rar_with_kotlin_codes() {
        for name in ["rar4-encrypted.rar", "rar5-encrypted.rar"] {
            let media = analyze_book(&book(&kotlin_archive(name)), false, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Unsupported, "{name}");
            assert_eq!(media.comment.as_deref(), Some("ERR_1002"), "{name}");
            assert!(
                media
                    .media_type
                    .as_deref()
                    .is_some_and(|value| value.starts_with("application/x-rar-compressed")),
                "{name}"
            );
        }

        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("multi-volume.cbr");
        let image = png();
        let volumes = write_stored_volumes(
            StoredEntry {
                name: b"page.png",
                data: &image,
                file_time: 0,
                file_attr: 0x20,
                host_os: 3,
                password: None,
                file_comment: None,
            },
            WriterOptions::new(ArchiveVersion::Rar40, FeatureSet::store_only()),
            64,
        )
        .unwrap();
        assert!(volumes.len() > 1);
        fs::write(&path, &volumes[0]).unwrap();

        let media = analyze_book(&book(&path), false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Unsupported);
        assert_eq!(media.comment.as_deref(), Some("ERR_1004"));
    }
}
