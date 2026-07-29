//! Media analysis, extraction, and image operations for Komga books.

#![warn(missing_docs)]

use std::{fs::File, io, path::Path};
use thiserror::Error;

use komga_core::{
    Book, BookPage, Dimension, EPUB_MEDIA_TYPE, JPEG_MEDIA_TYPE, MOBI_MEDIA_TYPE, Media, MediaProfile, MediaStatus,
    PDF_MEDIA_TYPE, ZIP_MEDIA_TYPE,
};

mod archive;
mod barcode;
mod comicrack;
mod epub;
mod hashing;
mod image_ops;
mod kepub;
mod metadata;
mod mobi;
mod page_removal;
mod pdf;
mod pdf_edit;
mod rar_conversion;
mod zip_edit;

pub use barcode::extract_isbn_barcode;
pub use comicrack::{ComicRackListError, parse_read_list};
pub use epub::rewrite_hashed_epub_pages;
pub use hashing::{
    ComputedPageHash, FileHashError, PDF_PAGE_HASH_ALGORITHM, PageHashError, RAW_PAGE_HASH_ALGORITHM,
    RGB_PAGE_HASH_ALGORITHM, hash_file, hash_file_koreader, hash_page, reconcile_scan_hashes,
};
pub use kepub::{KepubConversionError, convert_epub_to_kepub, validate_kepub_conversion};
pub use metadata::{
    ComicInfoContainers, ComicInfoReadList, EmbeddedMetadata, extract_comic_info_containers,
    extract_comic_info_metadata, extract_epub_metadata, extract_mylar_metadata,
};
pub use mobi::{MobiConversionError, MobiVariant, convert_mobi_to_epub, is_mobi};
pub use page_removal::{PageRemovalError, PageRemovalRewrite};
pub use pdf::PdfPageRenderer;
pub use pdf_edit::rewrite_hashed_pdf_pages;
pub use rar_conversion::{RarToCbzConversion, RarToCbzError, convert_rar_to_cbz, rewrite_hashed_rar_pages_to_cbz};
pub use zip_edit::rewrite_hashed_zip_pages;

#[cfg(test)]
use metadata::parse_comic_info_containers;

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

/// Errors produced while adding converted KEPUB positions to analyzed EPUB media.
#[derive(Debug, Error)]
pub enum KepubSpanError {
    /// Server shutdown was requested while reading converted resources.
    #[error("KEPUB span enrichment cancelled")]
    Cancelled,
    /// The converted file is not a readable KEPUB archive with matching resources.
    #[error("could not read converted KEPUB positions")]
    Invalid,
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
    /// The requested operation is not supported by this media profile.
    #[error("book media profile does not support this operation")]
    UnsupportedProfile,
    /// The requested analyzed resource does not exist.
    #[error("book resource does not exist")]
    ResourceNotFound,
    /// The source file or archive entry could not be read.
    #[error("could not read book content: {0}")]
    Read(String),
    /// The source image cannot be decoded or encoded.
    #[error("could not convert image: {0}")]
    Image(String),
}

/// Detects and analyzes a book using Kotlin-compatible media state transitions.
///
/// ZIP/CBZ, RAR/CBR, PDF, EPUB, and unencrypted MOBI books are analyzed without external
/// commands.
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
    let mut detected = match infer::get_from_path(&path) {
        Ok(Some(kind)) => archive::normalize_media_type(kind.mime_type()).to_owned(),
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
        .is_some_and(|value| value.eq_ignore_ascii_case("mobi"))
    {
        if !mobi::is_mobi(&path) {
            return Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(MOBI_MEDIA_TYPE.to_owned()),
                Some("ERR_1041"),
            ));
        }
        let materialized = match tempfile::NamedTempFile::new() {
            Ok(materialized) => materialized,
            Err(_) => {
                return Ok(Media::result(
                    &book.id,
                    MediaStatus::Error,
                    Some(MOBI_MEDIA_TYPE.to_owned()),
                    Some("ERR_1041"),
                ));
            }
        };
        return match mobi::convert_mobi_to_epub(&path, materialized.path(), &cancelled) {
            Ok(_) => analyze_materialized_mobi(materialized.path(), &book.id, analyze_dimensions, cancelled),
            Err(mobi::MobiConversionError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(mobi::MobiConversionError::Drm) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(MOBI_MEDIA_TYPE.to_owned()),
                Some("ERR_1040"),
            )),
            Err(_) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(MOBI_MEDIA_TYPE.to_owned()),
                Some("ERR_1041"),
            )),
        };
    }

    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("epub"))
        && detected != EPUB_MEDIA_TYPE
    {
        if epub::is_epub(&path) {
            detected = EPUB_MEDIA_TYPE.to_owned();
        } else {
            return Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(detected),
                Some("ERR_1032"),
            ));
        }
    }

    if detected == EPUB_MEDIA_TYPE {
        return match epub::analyze(&path, &book.id, analyze_dimensions, cancelled) {
            Ok(media) => Ok(media),
            Err(epub::AnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(epub::AnalysisError::Invalid) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(EPUB_MEDIA_TYPE.to_owned()),
                Some("ERR_1005"),
            )),
        };
    }

    if detected == PDF_MEDIA_TYPE {
        return match pdf::analyze(&path, &book.id, analyze_dimensions, cancelled) {
            Ok(media) => Ok(media),
            Err(pdf::AnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(pdf::AnalysisError::Io(kind)) => {
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
            Err(pdf::AnalysisError::Invalid) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(PDF_MEDIA_TYPE.to_owned()),
                Some("ERR_1005"),
            )),
        };
    }

    if detected == ZIP_MEDIA_TYPE {
        return match archive::analyze_zip(&path, analyze_dimensions, cancelled) {
            Ok(analysis) => Ok(archive::media_from_analysis(&book.id, ZIP_MEDIA_TYPE, analysis)),
            Err(archive::ZipAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(archive::ZipAnalysisError::Archive) => Ok(Media::result(
                &book.id,
                MediaStatus::Error,
                Some(ZIP_MEDIA_TYPE.to_owned()),
                Some("ERR_1008"),
            )),
        };
    }

    if archive::is_rar_media_type(&detected) {
        return match archive::analyze_rar(&path, analyze_dimensions, cancelled) {
            Ok(analysis) => Ok(archive::media_from_analysis(
                &book.id,
                analysis.media_type,
                analysis.contents,
            )),
            Err(archive::RarAnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
            Err(archive::RarAnalysisError::Encrypted) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(detected),
                Some("ERR_1002"),
            )),
            Err(archive::RarAnalysisError::MultiVolume) => Ok(Media::result(
                &book.id,
                MediaStatus::Unsupported,
                Some(detected),
                Some("ERR_1004"),
            )),
            Err(archive::RarAnalysisError::Archive) => Ok(Media::result(
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

/// Analyzes a materialized EPUB while preserving MOBI as the persisted source type.
///
/// This is used by the server-side MOBI cache after conversion. Generated
/// resources, navigation, and positions are retained, while fixed-layout EPUB
/// behavior and KEPUB-specific flags remain disabled for MOBI sources.
///
/// # Errors
///
/// Returns [`MediaAnalysisError::Cancelled`] when `cancelled` requests stop.
pub fn analyze_materialized_mobi(
    epub_path: &Path,
    book_id: &str,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Media, MediaAnalysisError> {
    match epub::analyze(epub_path, book_id, analyze_dimensions, cancelled) {
        Ok(mut media) => {
            media.media_type = Some(MOBI_MEDIA_TYPE.to_owned());
            media.pages.clear();
            media.epub_divina_compatible = false;
            media.epub_is_kepub = false;
            if let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_mut() {
                extension.is_fixed_layout = false;
            }
            Ok(media)
        }
        Err(epub::AnalysisError::Cancelled) => Err(MediaAnalysisError::Cancelled),
        Err(epub::AnalysisError::Invalid) => Ok(Media::result(
            book_id,
            MediaStatus::Error,
            Some(MOBI_MEDIA_TYPE.to_owned()),
            Some("ERR_1041"),
        )),
    }
}

/// Adds Kobo span identifiers from a converted KEPUB to an ordinary reflowable EPUB analysis.
///
/// Fixed-layout publications, native KEPUB files, non-EPUB media, and unsuccessful
/// analyses are left unchanged. The converted archive is read only and all other
/// metadata continues to come from the original EPUB.
///
/// # Errors
///
/// Returns [`KepubSpanError::Cancelled`] when the callback requests stop, or
/// [`KepubSpanError::Invalid`] when the converted archive or a spine resource
/// cannot be read.
pub fn enrich_kobo_spans(
    media: &mut Media,
    kepub_path: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<(), KepubSpanError> {
    if media.status != MediaStatus::Ready || media.media_type.as_deref() != Some(EPUB_MEDIA_TYPE) || media.epub_is_kepub
    {
        return Ok(());
    }
    let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_mut() else {
        return Ok(());
    };
    if extension.is_fixed_layout {
        return Ok(());
    }
    if cancelled() {
        return Err(KepubSpanError::Cancelled);
    }
    let file = File::open(kepub_path).map_err(|_| KepubSpanError::Invalid)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|_| KepubSpanError::Invalid)?;
    epub::positions::enrich_from_kepub(&mut archive, &media.files, &mut extension.positions, &cancelled).map_err(
        |error| match error {
            epub::positions::PositionError::Cancelled => KepubSpanError::Cancelled,
            epub::positions::PositionError::Invalid => KepubSpanError::Invalid,
        },
    )
}

/// Returns the media type produced when a stored page is delivered.
///
/// PDF pages are persisted without an image media type and render as JPEG.
pub fn page_output_media_type<'a>(media: &Media, page: &'a BookPage) -> &'a str {
    if media.profile() == Some(MediaProfile::Pdf) {
        JPEG_MEDIA_TYPE
    } else {
        &page.media_type
    }
}

/// Returns the dimensions produced when a stored page is delivered.
///
/// PDF crop-box dimensions are scaled with the same policy as the renderer.
pub fn page_output_dimension(media: &Media, page: &BookPage) -> Option<Dimension> {
    if media.profile() == Some(MediaProfile::Pdf) {
        page.dimension.and_then(pdf::output_dimension)
    } else {
        page.dimension
    }
}

/// Extracts one rasterized page from a ready ZIP-, RAR-, PDF-, or fixed-layout
/// EPUB-backed record.
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
    match media.profile() {
        Some(MediaProfile::Pdf) => {
            return pdf::render_page(&path, usize::try_from(page_number - 1).unwrap_or(usize::MAX));
        }
        Some(MediaProfile::Epub) if media.epub_divina_compatible => {
            return epub::extract(&path, &page.file_name)
                .map(|bytes| TypedBytes {
                    bytes,
                    media_type: page.media_type.clone(),
                })
                .map_err(MediaOperationError::Read);
        }
        Some(MediaProfile::Epub) => return Err(MediaOperationError::UnsupportedProfile),
        _ => {}
    }
    let bytes = match media.media_type.as_deref() {
        Some(ZIP_MEDIA_TYPE) => archive::extract_zip_entry(&path, &page.file_name)?,
        Some(media_type) if archive::is_rar_media_type(media_type) => {
            archive::extract_rar_entry(&path, &page.file_name)?
        }
        _ => return Err(MediaOperationError::NotReady),
    };
    Ok(TypedBytes {
        bytes,
        media_type: page.media_type.clone(),
    })
}

/// Extracts one declared resource from a ready EPUB.
///
/// Resource names must exactly match an analyzed `MEDIA_FILE` entry. This
/// prevents callers from using the API as an unrestricted ZIP reader.
///
/// # Errors
///
/// Returns an error when the media is not ready, is not EPUB, the resource was
/// not declared during analysis, or the archive entry cannot be read.
pub fn extract_resource(book: &Book, media: &Media, resource: &str) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Err(MediaOperationError::NotReady);
    }
    if media.profile() != Some(MediaProfile::Epub) {
        return Err(MediaOperationError::UnsupportedProfile);
    }
    let file = media
        .files
        .iter()
        .find(|file| file.file_name == resource)
        .ok_or(MediaOperationError::ResourceNotFound)?;
    let path = book
        .url
        .to_file_path()
        .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
    let bytes = epub::extract(&path, resource).map_err(MediaOperationError::Read)?;
    Ok(TypedBytes {
        bytes,
        media_type: file
            .media_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_owned()),
    })
}

/// Extracts one PDF page as a standalone PDF document.
///
/// # Errors
///
/// Returns an error when media is not a ready PDF, the page is out of bounds,
/// or the source document cannot be read or rewritten.
pub fn extract_raw_page(book: &Book, media: &Media, page_number: i32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready || media.profile() != Some(MediaProfile::Pdf) {
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
    pdf::extract_page(&path, index)
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
    image_ops::convert(source, output, max_edge)
}

/// Reports whether the linked image pipeline can decode a media type for
/// conversion to JPEG or PNG.
pub fn can_convert_image_media_type(media_type: &str) -> bool {
    image_ops::can_decode_media_type(media_type)
}

/// Generates a JPEG poster from an EPUB package cover or the first page.
///
/// # Errors
///
/// Returns page extraction or image conversion errors unchanged.
pub fn generate_thumbnail(book: &Book, media: &Media, max_edge: u32) -> Result<TypedBytes, MediaOperationError> {
    if media.status != MediaStatus::Ready {
        return Err(MediaOperationError::NotReady);
    }
    let cover = if media.profile() == Some(MediaProfile::Epub) {
        let path = book
            .url
            .to_file_path()
            .map_err(|()| MediaOperationError::Read("book URL is not a file URL".to_owned()))?;
        epub::cover(&path)
            .map_err(MediaOperationError::Read)?
            .map_or_else(|| extract_page(book, media, 1), Ok)?
    } else {
        extract_page(book, media, 1)?
    };
    convert_image(&cover.bytes, ImageOutput::Jpeg, Some(max_edge))
}

/// Reads the first image frame's dimensions without decoding all pixels.
pub fn image_dimension(bytes: &[u8]) -> Option<Dimension> {
    image_ops::dimension(bytes)
}

/// Builds a two-by-two JPEG poster from up to four source images.
pub fn collage_thumbnail(sources: &[Vec<u8>], size: u32) -> Result<Vec<u8>, MediaOperationError> {
    image_ops::collage(sources, size)
}

/// Detects an image media type from encoded bytes.
pub fn detect_image_media_type(bytes: &[u8]) -> Option<String> {
    infer::get(bytes)
        .filter(|kind| kind.mime_type().starts_with("image/"))
        .map(|kind| kind.mime_type().to_owned())
        .or_else(|| image_ops::detected_media_type(bytes).map(str::to_owned))
        .or_else(|| is_svg_document(bytes).then(|| "image/svg+xml".to_owned()))
}

fn is_svg_document(bytes: &[u8]) -> bool {
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(quick_xml::events::Event::Start(start) | quick_xml::events::Event::Empty(start)) => {
                let name = start.name();
                return name
                    .as_ref()
                    .rsplit(|byte| *byte == b':')
                    .next()
                    .is_some_and(|local_name| local_name.eq_ignore_ascii_case(b"svg"));
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => return false,
            Ok(_) => {}
        }
        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        fs::File,
        io::{Cursor, Write},
        path::{Path, PathBuf},
    };

    use hayro::hayro_syntax::Pdf;
    use image::{DynamicImage, GenericImageView, ImageFormat};
    use pdf_writer::Ref;
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

    fn kotlin_epub(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../komga/komga/src/test/resources/epub")
            .join(name)
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

    fn write_epub(path: &Path, opf: &str, entries: &[(&str, &[u8])]) {
        write_epub_documents(
            path,
            br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            opf.as_bytes(),
            entries,
        );
    }

    fn write_epub_documents(path: &Path, container: &[u8], opf: &[u8], entries: &[(&str, &[u8])]) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        writer.start_file("mimetype", options).unwrap();
        writer.write_all(EPUB_MEDIA_TYPE.as_bytes()).unwrap();
        writer.start_file("META-INF/container.xml", options).unwrap();
        writer.write_all(container).unwrap();
        writer.start_file("content.opf", options).unwrap();
        writer.write_all(opf).unwrap();
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    fn utf16_xml(value: &str, little_endian: bool) -> Vec<u8> {
        let mut bytes = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for code_unit in value.encode_utf16() {
            let encoded = if little_endian {
                code_unit.to_le_bytes()
            } else {
                code_unit.to_be_bytes()
            };
            bytes.extend_from_slice(&encoded);
        }
        bytes
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
            assert_eq!(media.media_type.as_deref(), Some(ZIP_MEDIA_TYPE), "{name}");
            assert_eq!(media.page_count, 1, "{name}");
            assert_eq!(media.pages[0].file_name, "komga.png", "{name}");
            assert_eq!(media.pages[0].media_type, "image/png", "{name}");
            assert_eq!(media.pages[0].dimension, None, "{name}");
            assert_eq!(media.pages[0].file_size, Some(3108), "{name}");
            assert_eq!(media.comment, None, "{name}");
        }

        let encrypted = analyze_book(&book(&kotlin_archive("zip-encrypted.zip")), false, || false).unwrap();
        assert_eq!(encrypted.status, MediaStatus::Error);
        assert_eq!(encrypted.media_type.as_deref(), Some(ZIP_MEDIA_TYPE));
        assert_eq!(encrypted.comment.as_deref(), Some("ERR_1006"));
        assert!(encrypted.pages.is_empty());
    }

    #[test]
    fn rejects_kotlin_7zip_fixtures_with_the_unsupported_format_contract() {
        for name in ["7zip.7z", "7zip-encrypted.7z"] {
            let media = analyze_book(&book(&kotlin_archive(name)), false, || false).unwrap();
            assert_eq!(media.status, MediaStatus::Unsupported, "{name}");
            assert_eq!(
                media.media_type.as_deref(),
                Some("application/x-7z-compressed"),
                "{name}"
            );
            assert_eq!(media.comment.as_deref(), Some("ERR_1001"), "{name}");
            assert!(media.pages.is_empty(), "{name}");
        }
    }

    #[test]
    fn detects_xml_declared_svg_pages_and_normalizes_xml_files_like_tika() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("vector.cbz");
        let mut writer = ZipWriter::new(File::create(&path).unwrap());
        let options = SimpleFileOptions::default();
        writer.start_file("vector.svg", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"><rect width="10" height="20"/></svg>"#,
            )
            .unwrap();
        writer.start_file("metadata.xml", options).unwrap();
        writer.write_all(br#"<?xml version="1.0"?><metadata/>"#).unwrap();
        writer.finish().unwrap();

        let media = analyze_book(&book(&path), true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.page_count, 1);
        assert_eq!(media.pages[0].file_name, "vector.svg");
        assert_eq!(media.pages[0].media_type, "image/svg+xml");
        assert_eq!(media.pages[0].dimension, None);
        assert_eq!(media.files[0].file_name, "metadata.xml");
        assert_eq!(media.files[0].media_type.as_deref(), Some("application/xml"));
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

        assert_eq!(page_output_media_type(&media, &media.pages[0]), JPEG_MEDIA_TYPE);
        assert_eq!(
            page_output_dimension(&media, &media.pages[0]),
            Some(Dimension {
                width: 3200,
                height: 3200
            })
        );

        let rendered = extract_page(&source, &media, 1).unwrap();
        assert_eq!(rendered.media_type, JPEG_MEDIA_TYPE);
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
    fn analyzes_fixed_layout_epub_and_extracts_pages_resources_and_cover() {
        let source = book(&kotlin_archive("epub3.epub"));
        let media = analyze_book(&source, true, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(EPUB_MEDIA_TYPE));
        assert_eq!(media.page_count, 2);
        assert!(media.epub_divina_compatible);
        assert!(!media.epub_is_kepub);
        assert_eq!(media.pages[0].file_name, "cover.jpeg");
        assert_eq!(media.pages[1].file_name, "0_0.png");
        assert_eq!(media.pages[1].dimension, Some(Dimension { width: 38, height: 38 }));
        assert_eq!(media.comment, None);

        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_ref() else {
            panic!("expected EPUB extension");
        };
        assert!(extension.is_fixed_layout);
        assert_eq!(extension.positions.len(), 2);
        assert_eq!(extension.positions[0].href, "titlepage.xhtml");
        assert_eq!(extension.positions[1].href, "page_1.xhtml");

        let first_page = extract_page(&source, &media, 1).unwrap();
        assert_eq!(first_page.media_type, "image/jpeg");
        assert!(first_page.bytes.starts_with(&[0xff, 0xd8]));
        let resource = extract_resource(&source, &media, "nav.xhtml").unwrap();
        assert_eq!(resource.media_type, "application/xhtml+xml");
        assert!(resource.bytes.windows(10).any(|bytes| bytes == b"Navigation"));
        assert!(matches!(
            extract_resource(&source, &media, "META-INF/container.xml"),
            Err(MediaOperationError::ResourceNotFound)
        ));

        let thumbnail = generate_thumbnail(&source, &media, 300).unwrap();
        assert_eq!(thumbnail.media_type, JPEG_MEDIA_TYPE);
        assert_eq!(
            image::load_from_memory(&thumbnail.bytes).unwrap().dimensions(),
            (48, 48)
        );
    }

    #[test]
    fn analyzes_reflowable_epub_navigation_positions_and_invalid_extension() {
        let source = book(&kotlin_epub("The Incomplete Theft - Ralph Burke.epub"));
        let media = analyze_book(&source, false, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(EPUB_MEDIA_TYPE));
        assert!(!media.epub_divina_compatible);
        assert!(media.pages.is_empty());
        assert_eq!(
            media
                .files
                .iter()
                .filter(|file| file.sub_type.as_deref() == Some("EPUB_PAGE"))
                .count(),
            4
        );
        assert_eq!(media.comment, None);
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_ref() else {
            panic!("expected EPUB extension");
        };
        assert!(!extension.is_fixed_layout);
        assert_eq!(extension.toc[0].title, "The Incomplete Theft");
        assert_eq!(
            extension.toc[0].href.as_deref(),
            Some("OEBPS/@public@vhost@g@gutenberg@html@files@65659@65659-h@65659-h-0.htm_split_001.html")
        );
        assert!(!extension.positions.is_empty());
        assert!(extension.positions.iter().all(|position| position.kobo_span.is_none()));
        assert!(matches!(
            extract_page(&source, &media, 1),
            Err(MediaOperationError::PageOutOfBounds)
        ));

        let invalid = analyze_book(&book(&kotlin_archive("zip-as-epub.epub")), false, || false).unwrap();
        assert_eq!(invalid.status, MediaStatus::Error);
        assert_eq!(invalid.media_type.as_deref(), Some(ZIP_MEDIA_TYPE));
        assert_eq!(invalid.comment.as_deref(), Some("ERR_1032"));
    }

    #[test]
    fn analyzes_epub_with_incomplete_manifest_items_as_resource_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("incomplete-manifest.epub");
        write_epub(
            &path,
            r#"<package><manifest>
                <item id="page" href="page.xhtml" media-type="application/xhtml+xml"/>
                <item href="missing-id.css" media-type="text/css"/>
                <item id="missing-href" media-type="image/jpeg"/>
                <item id="missing-type" href="missing-type.bin"/>
              </manifest><spine><itemref idref="page"/></spine></package>"#,
            &[("page.xhtml", b"<html><body>Readable publication</body></html>")],
        );

        let media = analyze_book(&book(&path), true, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(
            media.comment.as_deref(),
            Some("ERR_1033 [missing-id.css, , missing-type.bin]")
        );
        assert_eq!(media.files.len(), 1);
        assert_eq!(media.files[0].file_name, "page.xhtml");
    }

    #[test]
    fn analyzes_utf16_container_and_doctype_opf_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("utf16.epub");
        let container = utf16_xml(
            r#"<?xml version="1.0" encoding="UTF-16"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            true,
        );
        let opf = utf16_xml(
            r#"<?xml version="1.0" encoding="UTF-16"?>
                <!DOCTYPE package>
                <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
                  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Encoded title</dc:title></metadata>
                  <manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/></manifest>
                  <spine><itemref idref="page"/></spine>
                </package>"#,
            false,
        );
        let page = utf16_xml(
            r#"<?xml version="1.0" encoding="UTF-16"?><html><body>Readable text.</body></html>"#,
            true,
        );
        write_epub_documents(&path, &container, &opf, &[("page.xhtml", page.as_slice())]);
        let source = book(&path);

        let media = analyze_book(&source, false, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.comment, None);
        assert_eq!(media.page_count, 1);
        let metadata = extract_epub_metadata(&source, &media).unwrap().unwrap();
        assert_eq!(metadata.book.title.as_deref(), Some("Encoded title"));
    }

    #[test]
    fn detects_native_kepub_positions_and_reports_missing_manifest_resources() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("native-kepub.epub");
        let opf = r#"<?xml version="1.0"?>
            <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
              <metadata/>
              <manifest>
                <item id="page" href="page.xhtml" media-type="application/xhtml+xml"/>
                <item id="missing" href="missing.css" media-type="text/css"/>
              </manifest>
              <spine><itemref idref="page"/></spine>
            </package>"#;
        let page = utf16_xml(
            &format!(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?><!DOCTYPE html><html><body><span class=\"koboSpan\" id=\"kobo.1.1\">start</span>{}<span class=\"koboSpan\" id=\"kobo.1.2\">end</span></body></html>",
                "x".repeat(1200)
            ),
            true,
        );
        write_epub(&path, opf, &[("page.xhtml", page.as_slice())]);

        let media = analyze_book(&book(&path), false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert!(media.epub_is_kepub);
        assert!(!media.epub_divina_compatible);
        assert_eq!(media.comment.as_deref(), Some("ERR_1033 [missing.css]"));
        assert!(media.files.iter().all(|file| file.file_name != "missing.css"));
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension else {
            panic!("expected EPUB extension");
        };
        assert!(extension.positions.len() >= 2);
        assert_eq!(extension.positions[0].kobo_span.as_deref(), Some("kobo.1.1"));
        assert!(
            extension.positions[1..]
                .iter()
                .any(|position| position.kobo_span.is_some())
        );
    }

    #[test]
    fn counts_epub_divina_text_as_kotlin_utf16_code_units() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("utf16-threshold.epub");
        let opf = r#"<?xml version="1.0"?>
            <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
              <metadata/>
              <manifest>
                <item id="page" href="page.xhtml" media-type="application/xhtml+xml"/>
                <item id="image" href="page.png" media-type="image/png"/>
              </manifest>
              <spine><itemref idref="page"/></spine>
            </package>"#;
        let page = format!(
            "<html><body>{}<img src=\"page.png\"/></body></html>",
            "\u{1f600}".repeat(8)
        );
        let image = png();
        write_epub(
            &path,
            opf,
            &[("page.xhtml", page.as_bytes()), ("page.png", image.as_slice())],
        );

        let media = analyze_book(&book(&path), false, || false).unwrap();

        assert_eq!(media.status, MediaStatus::Ready);
        assert!(!media.epub_divina_compatible);
        assert!(media.pages.is_empty());
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension else {
            panic!("expected EPUB extension");
        };
        assert!(!extension.is_fixed_layout);
    }

    #[test]
    fn enriches_ordinary_epub_positions_from_a_converted_kepub() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("ordinary.epub");
        let converted_path = temporary.path().join("ordinary.kepub.epub");
        let opf = r#"<?xml version="1.0"?>
            <package xmlns="http://www.idpf.org/2007/opf" version="3.0">
              <metadata/>
              <manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/></manifest>
              <spine><itemref idref="page"/></spine>
            </package>"#;
        let ordinary = format!("<html><body>{}</body></html>", "x".repeat(1600));
        let converted = format!(
            "<?xml version=\"1.0\"?><!DOCTYPE html><html><body><span class=\"koboSpan\" id=\"kobo.1.1\">start</span>{}<span class=\"koboSpan\" id=\"kobo.1.2\">end</span></body></html>",
            "x".repeat(1400)
        );
        write_epub(&source_path, opf, &[("page.xhtml", ordinary.as_bytes())]);
        write_epub(&converted_path, opf, &[("page.xhtml", converted.as_bytes())]);

        let mut media = analyze_book(&book(&source_path), false, || false).unwrap();
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_ref() else {
            panic!("expected EPUB extension");
        };
        assert!(extension.positions.len() >= 2);
        assert!(extension.positions.iter().all(|position| position.kobo_span.is_none()));

        enrich_kobo_spans(&mut media, &converted_path, || false).unwrap();
        let Some(komga_core::MediaExtension::Epub(extension)) = media.extension.as_ref() else {
            panic!("expected EPUB extension");
        };
        assert_eq!(extension.positions[0].kobo_span.as_deref(), Some("kobo.1.1"));
        assert!(
            extension.positions[1..]
                .iter()
                .any(|position| position.kobo_span.is_some())
        );
    }

    #[test]
    fn skips_kepub_enrichment_for_fixed_layout_media() {
        let source = book(&kotlin_archive("epub3.epub"));
        let mut media = analyze_book(&source, false, || false).unwrap();
        let before = media.clone();

        enrich_kobo_spans(&mut media, Path::new("missing.kepub.epub"), || false).unwrap();

        assert_eq!(media, before);
    }

    #[test]
    fn pdf_dimensions_can_be_skipped_and_invalid_documents_report_kotlin_error() {
        let source = book(&kotlin_pdf());
        let media = analyze_book(&source, false, || false).unwrap();
        assert_eq!(media.status, MediaStatus::Ready);
        assert_eq!(media.pages[0].dimension, None);
        assert_eq!(page_output_dimension(&media, &media.pages[0]), None);

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

    #[test]
    fn image_conversion_capabilities_match_enabled_decoders() {
        for media_type in [
            "image/jpeg",
            "image/png",
            "image/gif",
            "image/webp",
            "image/bmp",
            "image/tiff",
            "image/x-icon",
            "image/vnd.microsoft.icon; charset=binary",
            "image/jxl",
            "image/jp2",
            "image/j2c",
            "image/x-tga",
            "image/vnd-ms.dds",
            "image/vnd.radiance",
            "image/x-exr",
            "image/x-portable-anymap",
            "image/x-farbfeld",
            "image/x-qoi",
        ] {
            assert!(can_convert_image_media_type(media_type), "{media_type}");
        }
        for media_type in ["image/svg+xml", "application/pdf", "application/octet-stream"] {
            assert!(!can_convert_image_media_type(media_type), "{media_type}");
        }
        #[cfg(not(feature = "heif-native"))]
        for media_type in ["image/avif", "image/heif", "image/heic"] {
            assert!(!can_convert_image_media_type(media_type), "{media_type}");
        }
        #[cfg(feature = "heif-native")]
        for media_type in ["image/avif", "image/heif", "image/heic"] {
            assert!(can_convert_image_media_type(media_type), "{media_type}");
        }
    }

    #[test]
    fn parses_only_comic_info_container_fields_with_kotlin_pairing() {
        let parsed = parse_comic_info_containers(
            br#"<ComicInfo><SeriesGroup> Alpha, ,Beta,Alpha </SeriesGroup><AlternateSeries>Event</AlternateSeries><AlternateNumber>7</AlternateNumber><StoryArc>Arc A, Arc B, Arc C</StoryArc><StoryArcNumber>1,nope,3</StoryArcNumber><Unknown>ignored</Unknown></ComicInfo>"#,
        )
        .unwrap();
        assert_eq!(parsed.collections, ["Alpha", "Beta"]);
        assert_eq!(
            parsed.read_lists,
            [
                ComicInfoReadList {
                    name: "Event".into(),
                    number: Some(7),
                },
                ComicInfoReadList {
                    name: "Arc A".into(),
                    number: Some(1),
                },
                ComicInfoReadList {
                    name: "Arc C".into(),
                    number: Some(3),
                },
            ]
        );
    }

    #[test]
    fn comic_info_without_arc_numbers_keeps_names_and_rejects_dtd() {
        let parsed = parse_comic_info_containers(
            b"<ComicInfo><StoryArc> First, Second </StoryArc><AlternateNumber>invalid</AlternateNumber></ComicInfo>",
        )
        .unwrap();
        assert_eq!(
            parsed.read_lists,
            [
                ComicInfoReadList {
                    name: "First".into(),
                    number: None,
                },
                ComicInfoReadList {
                    name: "Second".into(),
                    number: None,
                },
            ]
        );
        assert!(parse_comic_info_containers(b"<!DOCTYPE x><ComicInfo />").is_err());

        let paired = parse_comic_info_containers(
            b"<ComicInfo><StoryArc>one, , three</StoryArc><StoryArcNumber>6,7,8</StoryArcNumber></ComicInfo>",
        )
        .unwrap();
        assert_eq!(
            paired.read_lists,
            [
                ComicInfoReadList {
                    name: "one".into(),
                    number: Some(6),
                },
                ComicInfoReadList {
                    name: "three".into(),
                    number: Some(8),
                },
            ]
        );
    }
}
