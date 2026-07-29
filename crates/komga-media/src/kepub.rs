//! In-process EPUB to KEPUB conversion.
//!
//! Sentence segmentation, XHTML transformation, OPF rewriting, and dummy
//! title-page behavior are compatibility translations of kepubify v4.0.4
//! (<https://github.com/pgaskin/kepubify/releases/tag/v4.0.4>).
//!
//! ```text
//! MIT License
//!
//! Copyright (c) 2017-2022 Patrick Gaskin
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in
//! all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//! FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.
//! ```

use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Seek, SeekFrom, Write},
};

use ego_tree::NodeId;
use html5ever::{Attribute, LocalName, Namespace, QualName};
use percent_encoding::percent_decode_str;
use quick_xml::{
    Reader, Writer, XmlVersion,
    events::{BytesEnd, BytesStart, Event},
};
use roxmltree::Node as XmlNode;
use scraper::{Html, Node};
use thiserror::Error;
use zip::{CompressionMethod, ZipArchive, ZipWriter, result::ZipError, write::SimpleFileOptions};

use crate::epub::package as epub_package;

const EPUB_MIMETYPE: &str = "application/epub+zip";
const CONTAINER_PATH: &str = "META-INF/container.xml";
const OPF_MEDIA_TYPE: &str = "application/oebps-package+xml";
const MAX_ENTRIES: usize = 65_535;
const MAX_DOCUMENT_SIZE: u64 = 16 * 1024 * 1024;
const MAX_DOCUMENTS_SIZE: u64 = 256 * 1024 * 1024;
const MAX_UNCOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;
const XHTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const MATHML_NAMESPACE: &str = "http://www.w3.org/1998/Math/MathML";
const KOBO_CSS: &str = "div#book-inner { margin-top: 0; margin-bottom: 0;}";
const DUMMY_ID: &str = "kepubify-titlepage-dummy";
const DUMMY_HREF: &str = "kepubify-titlepage-dummy.xhtml";
const DUMMY_CONTENT: &str = r#"<!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml" lang="en"><head><title></title></head><body><p style="text-align: center; margin: 4em 0; font-size: .7em; font-style: italic;">Page intentionally left blank by kepubify.</p></body></html>"#;

/// Errors produced while converting an EPUB to KEPUB.
#[derive(Debug, Error)]
pub enum KepubConversionError {
    /// Cancellation was requested between conversion stages.
    #[error("KEPUB conversion cancelled")]
    Cancelled,
    /// Reading or writing the source or destination failed.
    #[error("KEPUB conversion I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The input is not a structurally valid EPUB.
    #[error("invalid EPUB: {0}")]
    InvalidEpub(String),
    /// The ZIP uses a path or feature which is unsafe to process.
    #[error("unsafe EPUB archive: {0}")]
    UnsafeArchive(String),
    /// The archive exceeds a conversion resource boundary.
    #[error("EPUB conversion resource limit exceeded: {0}")]
    ResourceLimit(String),
    /// A package or content document could not be transformed compatibly.
    #[error("EPUB content conversion failed: {0}")]
    ContentConversion(String),
}

impl KepubConversionError {
    /// Returns whether an already safety-checked source may be handed to the
    /// configured compatibility converter.
    pub fn allows_external_fallback(&self) -> bool {
        matches!(self, Self::InvalidEpub(_) | Self::ContentConversion(_))
    }
}

#[derive(Debug)]
struct ArchiveIndex {
    names: Vec<String>,
    by_name: HashMap<String, usize>,
    by_lowercase: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
struct ManifestItem {
    id: String,
    href: String,
    media_type: String,
}

#[derive(Debug)]
struct Package {
    path: String,
    items: Vec<ManifestItem>,
    content_paths: Vec<String>,
    first_linear_id: Option<String>,
    cover_id: String,
}

/// Converts one EPUB stream to a Kobo-compatible KEPUB stream.
///
/// The output keeps unchanged entries as raw compressed data, emits `mimetype`
/// first without compression, and applies kepubify v4.0.4's default content,
/// OPF, dummy-title-page, and filtering behavior. The callback is checked
/// between archive and document stages.
///
/// # Errors
///
/// Returns [`KepubConversionError::UnsafeArchive`] for hostile ZIP features,
/// [`KepubConversionError::ResourceLimit`] for bounded-input violations, and
/// the other variants for cancellation, I/O, package, or content failures.
pub fn convert_epub_to_kepub<R, W>(
    mut source: R,
    destination: W,
    cancelled: impl Fn() -> bool,
) -> Result<(), KepubConversionError>
where
    R: Read + Seek,
    W: Write + Seek,
{
    check_cancelled(&cancelled)?;
    validate_central_directory(&mut source)?;
    source.seek(SeekFrom::Start(0))?;
    let mut archive = ZipArchive::new(source).map_err(invalid_zip)?;
    let index = inspect_archive(&mut archive, &cancelled)?;
    let package = read_package(&mut archive, &index, &cancelled)?;

    let add_dummy = dummy_titlepage_required(&mut archive, &index, &package, &cancelled)?;
    let mut converted = HashMap::<usize, Vec<u8>>::new();
    let mut converted_total = 0_u64;

    let opf_index = resolve_exact(&index, &package.path)
        .ok_or_else(|| KepubConversionError::InvalidEpub("package document is missing".into()))?;
    let opf = read_document(&mut archive, opf_index, &cancelled)?;
    add_document_size(&mut converted_total, opf.len())?;
    converted.insert(opf_index, transform_opf(&opf, &package.cover_id, add_dummy)?);

    let mut transformed_content = HashSet::new();
    for path in &package.content_paths {
        check_cancelled(&cancelled)?;
        let Some(entry_index) = resolve_with_case_fallback(&index, path)? else {
            continue;
        };
        if !transformed_content.insert(entry_index) {
            continue;
        }
        let content = read_document(&mut archive, entry_index, &cancelled)?;
        add_document_size(&mut converted_total, content.len())?;
        converted.insert(entry_index, transform_content(&content, &cancelled)?);
    }

    let dummy_path = add_dummy.then(|| join_archive_path(parent_path(&package.path), DUMMY_HREF));
    let dummy = add_dummy
        .then(|| transform_content(DUMMY_CONTENT.as_bytes(), &cancelled))
        .transpose()?;
    if let Some(bytes) = &dummy {
        add_document_size(&mut converted_total, bytes.len())?;
    }

    check_cancelled(&cancelled)?;
    let comment = archive.comment().to_vec();
    let mut writer = ZipWriter::new(destination);
    writer.set_raw_comment(comment.into_boxed_slice()).map_err(write_zip)?;
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .map_err(write_zip)?;
    writer.write_all(EPUB_MIMETYPE.as_bytes())?;

    for entry_index in 0..archive.len() {
        check_cancelled(&cancelled)?;
        let name = &index.names[entry_index];
        if name == "mimetype" || should_filter(name) || is_directory_name(name) {
            continue;
        }
        let entry = archive.by_index(entry_index).map_err(invalid_zip)?;
        if let Some(bytes) = converted.get(&entry_index) {
            let mut options = entry
                .options()
                .into_full_options()
                .compression_method(CompressionMethod::Deflated)
                .large_file(bytes.len() > u32::MAX as usize);
            if !entry.comment().is_empty() {
                options = options.with_file_comment(entry.comment());
            }
            writer.start_file(name, options).map_err(write_zip)?;
            writer.write_all(bytes)?;
        } else {
            writer.raw_copy_file(entry).map_err(write_zip)?;
        }
        if entry_index == opf_index
            && let (Some(path), Some(bytes)) = (dummy_path.as_deref(), dummy.as_deref())
        {
            writer
                .start_file(
                    path,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .unix_permissions(0o666),
                )
                .map_err(write_zip)?;
            writer.write_all(bytes)?;
        }
    }
    writer.finish().map_err(write_zip)?;
    Ok(())
}

/// Validates a generated KEPUB against its source EPUB.
///
/// Both archives receive the same path, feature, and size checks used by the
/// converter. The generated archive must begin with an uncompressed, correct
/// `mimetype`, contain a valid container and OPF, and retain every source
/// content document.
///
/// # Errors
///
/// Returns the corresponding conversion error when either archive is unsafe,
/// over limits, unreadable, or when the generated package is incomplete.
pub fn validate_kepub_conversion<R, K>(
    mut source: R,
    mut converted: K,
    cancelled: impl Fn() -> bool,
) -> Result<(), KepubConversionError>
where
    R: Read + Seek,
    K: Read + Seek,
{
    validate_central_directory(&mut source)?;
    source.seek(SeekFrom::Start(0))?;
    let mut source = ZipArchive::new(source).map_err(invalid_zip)?;
    let source_index = inspect_archive(&mut source, &cancelled)?;
    let source_package = read_package(&mut source, &source_index, &cancelled)?;

    validate_central_directory(&mut converted)?;
    converted.seek(SeekFrom::Start(0))?;
    let mut converted = ZipArchive::new(converted).map_err(invalid_zip)?;
    let converted_index = inspect_archive(&mut converted, &cancelled)?;
    let first = converted.by_index(0).map_err(invalid_zip)?;
    if first.name() != "mimetype" || first.compression() != CompressionMethod::Stored {
        return Err(KepubConversionError::InvalidEpub(
            "mimetype is not the first stored entry".into(),
        ));
    }
    drop(first);
    let mimetype_index = resolve_exact(&converted_index, "mimetype")
        .ok_or_else(|| KepubConversionError::InvalidEpub("mimetype is missing".into()))?;
    let mimetype = read_document(&mut converted, mimetype_index, &cancelled)?;
    if mimetype != EPUB_MIMETYPE.as_bytes() {
        return Err(KepubConversionError::InvalidEpub("mimetype is incorrect".into()));
    }
    let converted_package = read_package(&mut converted, &converted_index, &cancelled)?;
    let converted_names = converted_index.names.iter().collect::<HashSet<_>>();
    for path in source_package.content_paths {
        let Some(source_entry) = resolve_with_case_fallback(&source_index, &path)? else {
            continue;
        };
        let source_name = &source_index.names[source_entry];
        if !converted_names.contains(source_name) {
            return Err(KepubConversionError::InvalidEpub(format!(
                "generated package dropped content document {source_name:?}"
            )));
        }
    }
    if converted_package.content_paths.is_empty() {
        return Err(KepubConversionError::InvalidEpub(
            "generated package has no content documents".into(),
        ));
    }
    Ok(())
}

fn validate_central_directory<R: Read + Seek>(source: &mut R) -> Result<(), KepubConversionError> {
    const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    const ZIP64_EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
    const CENTRAL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];

    let file_len = source.seek(SeekFrom::End(0))?;
    let tail_len = usize::try_from(file_len.min(65_557)).unwrap_or(65_557);
    source.seek(SeekFrom::End(-i64::try_from(tail_len).unwrap_or(i64::MAX)))?;
    let mut tail = vec![0; tail_len];
    read_zip_structure(source, &mut tail, "ZIP tail is truncated")?;
    let eocd_index = tail
        .windows(EOCD_SIGNATURE.len())
        .rposition(|value| value == EOCD_SIGNATURE)
        .ok_or_else(|| KepubConversionError::InvalidEpub("end-of-central-directory record is missing".into()))?;
    if tail.len().saturating_sub(eocd_index) < 22 {
        return Err(KepubConversionError::InvalidEpub(
            "end-of-central-directory record is truncated".into(),
        ));
    }
    let eocd = &tail[eocd_index..];
    let mut entry_count = u64::from(u16::from_le_bytes([eocd[10], eocd[11]]));
    let mut central_offset = u64::from(u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]));
    if entry_count == u64::from(u16::MAX) || central_offset == u64::from(u32::MAX) {
        let eocd_absolute = file_len - u64::try_from(tail.len() - eocd_index).unwrap_or(0);
        if eocd_absolute < 20 {
            return Err(KepubConversionError::InvalidEpub("ZIP64 locator is missing".into()));
        }
        source.seek(SeekFrom::Start(eocd_absolute - 20))?;
        let mut locator = [0; 20];
        read_zip_structure(source, &mut locator, "ZIP64 locator is truncated")?;
        if locator[..4] != ZIP64_LOCATOR_SIGNATURE {
            return Err(KepubConversionError::InvalidEpub("ZIP64 locator is invalid".into()));
        }
        let zip64_offset = u64::from_le_bytes(locator[8..16].try_into().expect("fixed slice"));
        source.seek(SeekFrom::Start(zip64_offset))?;
        let mut zip64 = [0; 56];
        read_zip_structure(source, &mut zip64, "ZIP64 end record is truncated")?;
        if zip64[..4] != ZIP64_EOCD_SIGNATURE {
            return Err(KepubConversionError::InvalidEpub(
                "ZIP64 end-of-central-directory record is invalid".into(),
            ));
        }
        entry_count = u64::from_le_bytes(zip64[32..40].try_into().expect("fixed slice"));
        central_offset = u64::from_le_bytes(zip64[48..56].try_into().expect("fixed slice"));
    }
    if entry_count > MAX_ENTRIES as u64 {
        return Err(KepubConversionError::ResourceLimit(format!(
            "archive has more than {MAX_ENTRIES} entries"
        )));
    }

    source.seek(SeekFrom::Start(central_offset))?;
    let mut names = HashSet::<Vec<u8>>::new();
    for _ in 0..entry_count {
        let mut header = [0; 46];
        read_zip_structure(source, &mut header, "central-directory entry is truncated")?;
        if header[..4] != CENTRAL_SIGNATURE {
            return Err(KepubConversionError::InvalidEpub(
                "central-directory entry is invalid".into(),
            ));
        }
        let name_len = usize::from(u16::from_le_bytes([header[28], header[29]]));
        let extra_len = u64::from(u16::from_le_bytes([header[30], header[31]]));
        let comment_len = u64::from(u16::from_le_bytes([header[32], header[33]]));
        let mut name = vec![0; name_len];
        read_zip_structure(source, &mut name, "central-directory entry name is truncated")?;
        if !names.insert(name.clone()) {
            return Err(KepubConversionError::UnsafeArchive(format!(
                "duplicate entry name {:?}",
                String::from_utf8_lossy(&name)
            )));
        }
        source.seek(SeekFrom::Current(i64::try_from(extra_len + comment_len).map_err(
            |_| KepubConversionError::InvalidEpub("central-directory entry is too large".into()),
        )?))?;
    }
    Ok(())
}

fn read_zip_structure(source: &mut impl Read, bytes: &mut [u8], truncated: &str) -> Result<(), KepubConversionError> {
    source.read_exact(bytes).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            KepubConversionError::InvalidEpub(truncated.to_owned())
        } else {
            KepubConversionError::Io(error)
        }
    })
}

fn inspect_archive<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    cancelled: &impl Fn() -> bool,
) -> Result<ArchiveIndex, KepubConversionError> {
    if archive.len() > MAX_ENTRIES {
        return Err(KepubConversionError::ResourceLimit(format!(
            "archive has more than {MAX_ENTRIES} entries"
        )));
    }
    let mut names = Vec::with_capacity(archive.len());
    let mut by_name = HashMap::with_capacity(archive.len());
    let mut by_lowercase = HashMap::with_capacity(archive.len());
    let mut declared_size = 0_u64;
    for index in 0..archive.len() {
        check_cancelled(cancelled)?;
        let entry = archive.by_index(index).map_err(invalid_zip)?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| KepubConversionError::UnsafeArchive("entry name is not UTF-8".into()))?
            .to_owned();
        validate_entry_name(&name)?;
        if entry.encrypted() {
            return Err(KepubConversionError::UnsafeArchive(format!("encrypted entry {name:?}")));
        }
        if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(KepubConversionError::UnsafeArchive(format!(
                "symbolic-link entry {name:?}"
            )));
        }
        if by_name.insert(name.clone(), index).is_some() {
            return Err(KepubConversionError::UnsafeArchive(format!(
                "duplicate entry name {name:?}"
            )));
        }
        let lower = name.to_lowercase();
        if let Some(previous) = by_lowercase.insert(lower, index)
            && names.get(previous) != Some(&name)
        {
            return Err(KepubConversionError::UnsafeArchive(format!(
                "case-insensitive entry ambiguity between {:?} and {name:?}",
                names[previous]
            )));
        }
        declared_size = declared_size
            .checked_add(entry.size())
            .ok_or_else(|| KepubConversionError::ResourceLimit("declared uncompressed size overflowed".into()))?;
        if declared_size > MAX_UNCOMPRESSED_SIZE {
            return Err(KepubConversionError::ResourceLimit(format!(
                "declared uncompressed size exceeds {MAX_UNCOMPRESSED_SIZE} bytes"
            )));
        }
        names.push(name);
    }
    Ok(ArchiveIndex {
        names,
        by_name,
        by_lowercase,
    })
}

fn validate_entry_name(name: &str) -> Result<(), KepubConversionError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\\')
        || name.contains('\0')
        || name.split('/').any(|component| component == "..")
    {
        return Err(KepubConversionError::UnsafeArchive(format!(
            "invalid entry name {name:?}"
        )));
    }
    Ok(())
}

fn read_package<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    cancelled: &impl Fn() -> bool,
) -> Result<Package, KepubConversionError> {
    let container_index = resolve_exact(index, CONTAINER_PATH)
        .ok_or_else(|| KepubConversionError::InvalidEpub("OCF container is missing".into()))?;
    let container = read_document(archive, container_index, cancelled)?;
    let container = epub_package::decode_xml_text(&container)
        .map_err(|error| KepubConversionError::InvalidEpub(format!("invalid container encoding: {error}")))?;
    let document = epub_package::parse_xml(&container)
        .map_err(|error| KepubConversionError::InvalidEpub(format!("invalid OCF container: {error}")))?;
    let root = document.root_element();
    if !local_name(root, "container") || root.attribute("version") != Some("1.0") {
        return Err(KepubConversionError::InvalidEpub(
            "OCF container does not declare version 1.0".into(),
        ));
    }
    let package_path = document
        .descendants()
        .find(|node| local_name(*node, "rootfile") && node.attribute("media-type") == Some(OPF_MEDIA_TYPE))
        .and_then(|node| node.attribute("full-path"))
        .ok_or_else(|| KepubConversionError::InvalidEpub("container has no OPF rootfile".into()))?;
    let package_path = normalize_archive_href("", package_path).map_err(KepubConversionError::InvalidEpub)?;
    let package_index = resolve_exact(index, &package_path)
        .ok_or_else(|| KepubConversionError::InvalidEpub("OPF rootfile is missing".into()))?;
    let opf = read_document(archive, package_index, cancelled)?;
    let opf = epub_package::decode_xml_text(&opf)
        .map_err(|error| KepubConversionError::InvalidEpub(format!("invalid OPF encoding: {error}")))?;
    let document = epub_package::parse_xml(&opf)
        .map_err(|error| KepubConversionError::InvalidEpub(format!("invalid OPF: {error}")))?;
    if !local_name(document.root_element(), "package") {
        return Err(KepubConversionError::InvalidEpub(
            "OPF root element is not package".into(),
        ));
    }
    let manifest = document
        .descendants()
        .find(|node| local_name(*node, "manifest"))
        .ok_or_else(|| KepubConversionError::InvalidEpub("OPF manifest is missing".into()))?;
    let mut items = Vec::new();
    let mut content_paths = Vec::new();
    for item in manifest.children().filter(|node| local_name(*node, "item")) {
        let Some(id) = item.attribute("id").filter(|value| !value.is_empty()) else {
            return Err(KepubConversionError::InvalidEpub("manifest item has no id".into()));
        };
        let Some(href) = item.attribute("href").filter(|value| !value.is_empty()) else {
            return Err(KepubConversionError::InvalidEpub("manifest item has no href".into()));
        };
        let media_type = item.attribute("media-type").unwrap_or_default();
        let path =
            normalize_archive_href(parent_path(&package_path), href).map_err(KepubConversionError::InvalidEpub)?;
        if is_content_document(media_type, href) {
            content_paths.push(path.clone());
        }
        items.push(ManifestItem {
            id: id.to_owned(),
            href: href.to_owned(),
            media_type: media_type.to_owned(),
        });
    }
    let first_linear_id = document
        .descendants()
        .find(|node| local_name(*node, "spine"))
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| local_name(*node, "itemref"))
        .find(|node| node.attribute("linear") != Some("no"))
        .and_then(|node| node.attribute("idref"))
        .map(str::to_owned);
    let cover_id = document
        .descendants()
        .find(|node| local_name(*node, "meta") && node.attribute("name") == Some("cover"))
        .and_then(|node| node.attribute("content"))
        .unwrap_or("cover")
        .to_owned();
    Ok(Package {
        path: package_path,
        items,
        content_paths,
        first_linear_id,
        cover_id,
    })
}

fn dummy_titlepage_required<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: &ArchiveIndex,
    package: &Package,
    cancelled: &impl Fn() -> bool,
) -> Result<bool, KepubConversionError> {
    let Some(idref) = package.first_linear_id.as_deref() else {
        return Ok(false);
    };
    let Some(item) = package.items.iter().find(|item| item.id == idref) else {
        return Ok(false);
    };
    if !is_content_document(&item.media_type, &item.href) {
        return Ok(false);
    }
    let basename = item.href.rsplit('/').next().unwrap_or(&item.href).to_ascii_lowercase();
    if basename.contains("cover") || basename.contains("title") {
        return Ok(false);
    }
    let path =
        normalize_archive_href(parent_path(&package.path), &item.href).map_err(KepubConversionError::InvalidEpub)?;
    let Some(entry_index) = resolve_exact(index, &path) else {
        return Ok(false);
    };
    let content = read_document(archive, entry_index, cancelled)?;
    let content = std::str::from_utf8(strip_bom(&content))
        .map_err(|error| KepubConversionError::ContentConversion(format!("title page is not UTF-8: {error}")))?;
    let html = Html::parse_document(content);
    let Some(body) = find_element(&html, "body") else {
        return Ok(false);
    };
    let mut paragraphs = 0_usize;
    let mut images = 0_usize;
    let mut words = 0_usize;
    collect_titlepage_counts(&html, body, &mut paragraphs, &mut images, &mut words);
    Ok(paragraphs > 4 || words > 20 || (images == 0 && words < 5) || images > 4)
}

fn collect_titlepage_counts(
    html: &Html,
    node_id: NodeId,
    paragraphs: &mut usize,
    images: &mut usize,
    words: &mut usize,
) {
    let Some(node) = html.tree.get(node_id) else {
        return;
    };
    match node.value() {
        Node::Text(text) => {
            *words += text.split_whitespace().filter(|word| word.len() > 3).count();
        }
        Node::Element(element) => {
            let name = element.name();
            if name == "p" {
                *paragraphs += 1;
            }
            if matches!(name, "img" | "svg") {
                *images += 1;
            }
            if matches!(
                name,
                "img" | "svg" | "script" | "style" | "pre" | "audio" | "video" | "math"
            ) {
                return;
            }
        }
        _ => {}
    }
    for child in node.children() {
        collect_titlepage_counts(html, child.id(), paragraphs, images, words);
    }
}

fn transform_opf(bytes: &[u8], cover_id: &str, add_dummy: bool) -> Result<Vec<u8>, KepubConversionError> {
    let mut reader = Reader::from_reader(strip_bom(bytes));
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::with_capacity(bytes.len() + 512));
    let mut buffer = Vec::new();
    let mut skip_depth = 0_usize;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| KepubConversionError::ContentConversion(format!("could not parse OPF: {error}")))?;
        match event {
            Event::Start(start) if should_remove_opf_element(&reader, &start)? => {
                skip_depth = 1;
            }
            Event::Start(_) if skip_depth > 0 => skip_depth += 1,
            Event::End(_) if skip_depth > 0 => skip_depth -= 1,
            Event::Empty(start) if skip_depth == 0 && should_remove_opf_element(&reader, &start)? => {}
            Event::Start(start) if skip_depth == 0 => {
                let start = transform_cover_item(&reader, start, cover_id)?;
                let is_spine = local_event_name_from_start(&start) == "spine";
                let start_name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
                writer
                    .write_event(Event::Start(start))
                    .map_err(KepubConversionError::Io)?;
                if add_dummy && is_spine {
                    let mut itemref = BytesStart::new(sibling_name(&start_name, "itemref"));
                    itemref.push_attribute(("idref", DUMMY_ID));
                    writer
                        .write_event(Event::Empty(itemref))
                        .map_err(KepubConversionError::Io)?;
                }
            }
            Event::Empty(start) if skip_depth == 0 => {
                let start = transform_cover_item(&reader, start, cover_id)?;
                writer
                    .write_event(Event::Empty(start))
                    .map_err(KepubConversionError::Io)?;
            }
            Event::End(end) if skip_depth == 0 => {
                if add_dummy && local_event_name_from_end(&end) == "manifest" {
                    let manifest_name = String::from_utf8_lossy(end.name().as_ref()).into_owned();
                    let mut item = BytesStart::new(sibling_name(&manifest_name, "item"));
                    item.push_attribute(("id", DUMMY_ID));
                    item.push_attribute(("href", DUMMY_HREF));
                    item.push_attribute(("media-type", "application/xhtml+xml"));
                    writer
                        .write_event(Event::Empty(item))
                        .map_err(KepubConversionError::Io)?;
                }
                writer.write_event(Event::End(end)).map_err(KepubConversionError::Io)?;
            }
            Event::Eof => break,
            event if skip_depth == 0 => writer.write_event(event).map_err(KepubConversionError::Io)?,
            _ => {}
        }
        buffer.clear();
    }
    Ok(writer.into_inner())
}

fn should_remove_opf_element(reader: &Reader<&[u8]>, start: &BytesStart<'_>) -> Result<bool, KepubConversionError> {
    let name = local_event_name_from_start(start);
    if name == "meta" {
        return Ok(attribute_value(reader, start, "name")?.as_deref() == Some("calibre:timestamp"));
    }
    Ok(name == "contributor" && attribute_value(reader, start, "role")?.as_deref() == Some("bkp"))
}

fn transform_cover_item(
    reader: &Reader<&[u8]>,
    start: BytesStart<'_>,
    cover_id: &str,
) -> Result<BytesStart<'static>, KepubConversionError> {
    if local_event_name_from_start(&start) != "item"
        || attribute_value(reader, &start, "id")?.as_deref() != Some(cover_id)
    {
        return Ok(start.into_owned());
    }
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mut transformed = BytesStart::new(name);
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute
            .map_err(|error| KepubConversionError::ContentConversion(format!("invalid OPF attribute: {error}")))?;
        if local_bytes_name(attribute.key.as_ref()) == b"properties" {
            continue;
        }
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|error| KepubConversionError::ContentConversion(format!("invalid OPF attribute: {error}")))?;
        let escaped = quick_xml::escape::escape(value);
        transformed.push_attribute((key.as_str(), escaped.as_ref()));
    }
    transformed.push_attribute(("properties", "cover-image"));
    Ok(transformed.into_owned())
}

fn attribute_value(
    reader: &Reader<&[u8]>,
    start: &BytesStart<'_>,
    name: &str,
) -> Result<Option<String>, KepubConversionError> {
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute
            .map_err(|error| KepubConversionError::ContentConversion(format!("invalid XML attribute: {error}")))?;
        if local_bytes_name(attribute.key.as_ref()) == name.as_bytes() {
            return attribute
                .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(|error| {
                    KepubConversionError::ContentConversion(format!("invalid XML attribute value: {error}"))
                });
        }
    }
    Ok(None)
}

fn transform_content(bytes: &[u8], cancelled: &impl Fn() -> bool) -> Result<Vec<u8>, KepubConversionError> {
    check_cancelled(cancelled)?;
    let content = std::str::from_utf8(strip_bom(bytes))
        .map_err(|error| KepubConversionError::ContentConversion(format!("content is not UTF-8: {error}")))?;
    let declaration = leading_xml_declaration(content);
    let parsed = declaration.map_or(content, |value| &content[value.len()..]);
    let mut html = Html::parse_document(parsed);
    update_charset(&mut html);
    add_kobo_style(&mut html)?;
    add_kobo_wrappers(&mut html)?;
    add_kobo_spans(&mut html, cancelled)?;
    clean_content(&mut html);
    check_cancelled(cancelled)?;
    let mut output = Vec::with_capacity(bytes.len() + bytes.len() / 3);
    if let Some(declaration) = declaration {
        output.extend_from_slice(declaration.as_bytes());
    }
    serialize_node(&html, html.tree.root().id(), None, &mut output)?;
    Ok(output)
}

fn update_charset(html: &mut Html) {
    let ids = descendant_ids(html, html.tree.root().id());
    for id in ids {
        let Some(mut node) = html.tree.get_mut(id) else {
            continue;
        };
        let Node::Element(element) = node.value() else {
            continue;
        };
        if element.name() != "meta" {
            continue;
        }
        let http_equiv =
            element_attr(element, "http-equiv").is_some_and(|value| value.eq_ignore_ascii_case("content-type"));
        for (name, value) in &mut element.attrs {
            if name.local.as_ref() == "charset" {
                *value = "UTF-8".into();
            } else if http_equiv && name.local.as_ref() == "content" {
                let updated = update_content_type_charset(value);
                *value = updated.into();
            }
        }
    }
}

fn update_content_type_charset(value: &str) -> String {
    let mut changed = false;
    let parts = value
        .split(';')
        .map(|part| {
            let trimmed = part.trim();
            if trimmed
                .split_once('=')
                .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("charset"))
            {
                changed = true;
                "charset=utf-8".to_owned()
            } else {
                trimmed.to_owned()
            }
        })
        .collect::<Vec<_>>();
    if changed { parts.join("; ") } else { value.to_owned() }
}

fn add_kobo_style(html: &mut Html) -> Result<(), KepubConversionError> {
    let head = find_element(html, "head")
        .ok_or_else(|| KepubConversionError::ContentConversion("HTML head is missing".into()))?;
    let style_id = html
        .tree
        .orphan(element_node(
            "style",
            XHTML_NAMESPACE,
            &[("type", "text/css"), ("class", "kobostylehacks")],
        ))
        .id();
    html.tree
        .get_mut(style_id)
        .expect("new style node exists")
        .append(text_node(KOBO_CSS));
    html.tree.get_mut(head).expect("head node exists").append_id(style_id);
    Ok(())
}

fn add_kobo_wrappers(html: &mut Html) -> Result<(), KepubConversionError> {
    let body = find_element(html, "body")
        .ok_or_else(|| KepubConversionError::ContentConversion("HTML body is missing".into()))?;
    let already_wrapped = html
        .tree
        .get(body)
        .into_iter()
        .flat_map(|node| node.children())
        .any(|child| {
            child
                .value()
                .as_element()
                .is_some_and(|element| element_attr(element, "id") == Some("book-columns"))
                && child.children().any(|inner| {
                    inner
                        .value()
                        .as_element()
                        .is_some_and(|element| element_attr(element, "id") == Some("book-inner"))
                })
        });
    if already_wrapped {
        return Ok(());
    }
    let inner = html
        .tree
        .orphan(element_node("div", XHTML_NAMESPACE, &[("id", "book-inner")]))
        .id();
    html.tree
        .get_mut(inner)
        .expect("new inner node exists")
        .reparent_from_id_append(body);
    let outer = html
        .tree
        .orphan(element_node("div", XHTML_NAMESPACE, &[("id", "book-columns")]))
        .id();
    html.tree
        .get_mut(outer)
        .expect("new outer node exists")
        .append_id(inner);
    html.tree.get_mut(body).expect("body node exists").append_id(outer);
    Ok(())
}

#[derive(Debug)]
enum SpanAction {
    Paragraph,
    Text {
        node: NodeId,
        parent_is_p: bool,
        value: String,
    },
    Image(NodeId),
}

fn add_kobo_spans(html: &mut Html, cancelled: &impl Fn() -> bool) -> Result<(), KepubConversionError> {
    let body = find_element(html, "body")
        .ok_or_else(|| KepubConversionError::ContentConversion("HTML body is missing".into()))?;
    if descendant_ids(html, body).into_iter().any(|id| {
        html.tree
            .get(id)
            .and_then(|node| node.value().as_element())
            .and_then(|element| element_attr(element, "class"))
            .is_some_and(|classes| classes.split_whitespace().any(|class| class == "koboSpan"))
    }) {
        return Ok(());
    }
    let mut actions = Vec::new();
    collect_span_actions(html, body, false, &mut actions);
    let mut paragraph = 0_usize;
    let mut segment = 0_usize;
    let mut increment_paragraph = false;
    for action in actions {
        check_cancelled(cancelled)?;
        match action {
            SpanAction::Paragraph => increment_paragraph = true,
            SpanAction::Image(image) => {
                paragraph += 1;
                segment = 1;
                increment_paragraph = false;
                let span = html.tree.orphan(kobo_span(paragraph, segment)).id();
                html.tree
                    .get_mut(image)
                    .expect("collected image exists")
                    .insert_id_before(span);
                html.tree.get_mut(span).expect("new span exists").append_id(image);
            }
            SpanAction::Text {
                node,
                parent_is_p,
                value,
            } => {
                for sentence in split_sentences(&value) {
                    if sentence.chars().all(char::is_whitespace) && !parent_is_p {
                        html.tree
                            .get_mut(node)
                            .expect("collected text exists")
                            .insert_before(text_node(sentence));
                        continue;
                    }
                    if increment_paragraph {
                        paragraph += 1;
                        segment = 0;
                        increment_paragraph = false;
                    }
                    segment += 1;
                    let span = html.tree.orphan(kobo_span(paragraph, segment)).id();
                    html.tree
                        .get_mut(span)
                        .expect("new span exists")
                        .append(text_node(sentence));
                    html.tree
                        .get_mut(node)
                        .expect("collected text exists")
                        .insert_id_before(span);
                }
                html.tree.get_mut(node).expect("collected text exists").detach();
            }
        }
    }
    Ok(())
}

fn collect_span_actions(html: &Html, node_id: NodeId, parent_is_p: bool, actions: &mut Vec<SpanAction>) {
    let Some(node) = html.tree.get(node_id) else {
        return;
    };
    match node.value() {
        Node::Text(text) => actions.push(SpanAction::Text {
            node: node_id,
            parent_is_p,
            value: text.to_string(),
        }),
        Node::Element(element) => {
            let name = element.name();
            if name == "img" {
                actions.push(SpanAction::Image(node_id));
                return;
            }
            if matches!(name, "script" | "style" | "pre" | "audio" | "video" | "svg" | "math") {
                return;
            }
            if matches!(
                name,
                "p" | "ol" | "ul" | "table" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            ) {
                actions.push(SpanAction::Paragraph);
            }
            let is_p = name == "p";
            for child in node.children() {
                collect_span_actions(html, child.id(), is_p, actions);
            }
        }
        _ => {
            for child in node.children() {
                collect_span_actions(html, child.id(), parent_is_p, actions);
            }
        }
    }
}

fn split_sentences(value: &str) -> Vec<&str> {
    #[derive(Clone, Copy)]
    enum State {
        Default,
        AfterPunctuation,
        AfterExtra,
        AfterSpace,
    }
    let mut sentences = Vec::with_capacity(4);
    let mut start = 0_usize;
    let mut state = State::Default;
    let mut iterator = value.char_indices().peekable();
    while let Some((index, character)) = iterator.next() {
        let punctuation = matches!(character, '.' | '!' | '?');
        let extra = matches!(character, '\'' | '"' | '”' | '’' | '“' | '…');
        let space = matches!(character, '\t' | '\n' | '\u{000c}' | '\r' | ' ');
        match state {
            State::Default => {
                if punctuation {
                    state = State::AfterPunctuation;
                }
            }
            State::AfterPunctuation => {
                state = if punctuation {
                    State::AfterPunctuation
                } else if extra {
                    State::AfterExtra
                } else if space {
                    State::AfterSpace
                } else {
                    State::Default
                };
            }
            State::AfterExtra => {
                state = if punctuation {
                    State::AfterPunctuation
                } else if space {
                    State::AfterSpace
                } else {
                    State::Default
                };
            }
            State::AfterSpace => {
                if !space {
                    sentences.push(&value[start..index]);
                    start = index;
                    state = if punctuation {
                        State::AfterPunctuation
                    } else {
                        State::Default
                    };
                }
            }
        }
        if iterator.peek().is_none() {
            sentences.push(&value[start..]);
        }
    }
    if value.is_empty() {
        sentences.push(value);
    }
    sentences
}

fn clean_content(html: &mut Html) {
    let ids = descendant_ids(html, html.tree.root().id());
    let mut remove = Vec::new();
    for id in ids {
        let Some(mut node) = html.tree.get_mut(id) else {
            continue;
        };
        match node.value() {
            Node::Text(text) if text.contains('�') => {
                text.text = text.replace('�', "").into();
            }
            Node::Element(element) => {
                let name = element.name();
                let adept = name == "meta"
                    && element_attr(element, "name")
                        .is_some_and(|value| matches!(value, "Adept.expected.resource" | "Adept.resource"));
                let office = (name == "o:p" || name.starts_with("st1:")) && element_is_empty(&html.tree, id);
                if adept || office {
                    remove.push(id);
                }
            }
            _ => {}
        }
    }
    for id in remove {
        if let Some(mut node) = html.tree.get_mut(id) {
            node.detach();
        }
    }
}

fn element_is_empty(tree: &ego_tree::Tree<Node>, id: NodeId) -> bool {
    tree.get(id).is_none_or(|node| {
        node.children().all(|child| match child.value() {
            Node::Comment(_) => true,
            Node::Text(text) => text.chars().all(char::is_whitespace),
            _ => false,
        })
    })
}

fn serialize_node(
    html: &Html,
    node_id: NodeId,
    parent: Option<&str>,
    output: &mut Vec<u8>,
) -> Result<(), KepubConversionError> {
    let node = html
        .tree
        .get(node_id)
        .ok_or_else(|| KepubConversionError::ContentConversion("HTML tree node disappeared".into()))?;
    match node.value() {
        Node::Document | Node::Fragment => {
            for child in node.children() {
                serialize_node(html, child.id(), None, output)?;
            }
        }
        Node::Doctype(doctype) => {
            output.extend_from_slice(b"<!DOCTYPE ");
            output.extend_from_slice(doctype.name().as_bytes());
            if !doctype.public_id().is_empty() {
                output.extend_from_slice(b" PUBLIC \"");
                escape_attribute(doctype.public_id(), output);
                output.extend_from_slice(b"\"");
            }
            if !doctype.system_id().is_empty() {
                if doctype.public_id().is_empty() {
                    output.extend_from_slice(b" SYSTEM");
                }
                output.extend_from_slice(b" \"");
                escape_attribute(doctype.system_id(), output);
                output.extend_from_slice(b"\"");
            }
            output.push(b'>');
        }
        Node::Comment(comment) => {
            if comment.starts_with('?') && comment.ends_with('?') {
                output.push(b'<');
                output.extend_from_slice(comment.as_bytes());
                output.push(b'>');
            } else {
                output.extend_from_slice(b"<!--");
                output.extend_from_slice(comment.as_bytes());
                output.extend_from_slice(b"-->");
            }
        }
        Node::ProcessingInstruction(instruction) => {
            output.extend_from_slice(b"<?");
            output.extend_from_slice(instruction.target.as_bytes());
            if !instruction.data.is_empty() {
                output.push(b' ');
                output.extend_from_slice(instruction.data.as_bytes());
            }
            output.extend_from_slice(b"?>");
        }
        Node::Text(text) => {
            if matches!(parent, Some("script" | "style")) {
                output.extend_from_slice(text.as_bytes());
            } else {
                escape_text(text, output);
            }
        }
        Node::Element(element) => {
            let name = element.name();
            output.push(b'<');
            write_qualified_name(&element.name, output);
            let has_default_xmlns = element.attrs.iter().any(|(name, _)| {
                name.local.as_ref() == "xmlns" && name.prefix.as_ref().is_none_or(|prefix| prefix.as_ref().is_empty())
            });
            let has_xlink_xmlns = element.attrs.iter().any(|(name, _)| {
                name.local.as_ref() == "xlink" && name.prefix.as_ref().is_some_and(|prefix| prefix.as_ref() == "xmlns")
            });
            for (attribute, value) in &element.attrs {
                output.push(b' ');
                write_qualified_name(attribute, output);
                output.extend_from_slice(b"=\"");
                escape_attribute(value, output);
                output.push(b'"');
            }
            if !has_default_xmlns {
                let namespace = element.name.ns.as_ref();
                let required = match name {
                    "html" => Some(XHTML_NAMESPACE),
                    "svg" => Some(SVG_NAMESPACE),
                    "math" => Some(MATHML_NAMESPACE),
                    _ if namespace == SVG_NAMESPACE => None,
                    _ if namespace == MATHML_NAMESPACE => None,
                    _ => None,
                };
                if let Some(namespace) = required {
                    output.extend_from_slice(b" xmlns=\"");
                    output.extend_from_slice(namespace.as_bytes());
                    output.push(b'"');
                }
            }
            if name == "svg" && !has_xlink_xmlns {
                output.extend_from_slice(b" xmlns:xlink=\"http://www.w3.org/1999/xlink\"");
            }
            if matches!(name, "script" | "style") && element_attr(element, "type").is_none() {
                let media_type = if name == "style" { "text/css" } else { "text/javascript" };
                output.extend_from_slice(b" type=\"");
                output.extend_from_slice(media_type.as_bytes());
                output.push(b'"');
            }
            if is_void_element(name) {
                output.extend_from_slice(b" />");
                return Ok(());
            }
            output.push(b'>');
            for child in node.children() {
                serialize_node(html, child.id(), Some(name), output)?;
            }
            output.extend_from_slice(b"</");
            write_qualified_name(&element.name, output);
            output.push(b'>');
        }
    }
    Ok(())
}

fn escape_text(value: &str, output: &mut Vec<u8>) {
    for character in value.chars() {
        match character {
            '&' => output.extend_from_slice(b"&amp;"),
            '<' => output.extend_from_slice(b"&lt;"),
            '>' => output.extend_from_slice(b"&gt;"),
            '\u{00a0}' => output.extend_from_slice(b"&#160;"),
            _ => {
                let mut buffer = [0; 4];
                output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
}

fn escape_attribute(value: &str, output: &mut Vec<u8>) {
    for character in value.chars() {
        match character {
            '&' => output.extend_from_slice(b"&amp;"),
            '<' => output.extend_from_slice(b"&lt;"),
            '"' => output.extend_from_slice(b"&quot;"),
            '\u{00a0}' => output.extend_from_slice(b"&#160;"),
            _ => {
                let mut buffer = [0; 4];
                output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
}

fn write_qualified_name(name: &QualName, output: &mut Vec<u8>) {
    if let Some(prefix) = &name.prefix
        && !prefix.as_ref().is_empty()
    {
        output.extend_from_slice(prefix.as_bytes());
        output.push(b':');
    }
    output.extend_from_slice(name.local.as_bytes());
}

fn element_node(name: &str, namespace: &str, attrs: &[(&str, &str)]) -> Node {
    let name = QualName::new(None, Namespace::from(namespace), LocalName::from(name));
    let attrs = attrs
        .iter()
        .map(|(name, value)| Attribute {
            name: QualName::new(None, Namespace::from(""), LocalName::from(*name)),
            value: (*value).into(),
        })
        .collect();
    Node::Element(scraper::node::Element::new(name, attrs))
}

fn text_node(value: &str) -> Node {
    Node::Text(scraper::node::Text { text: value.into() })
}

fn kobo_span(paragraph: usize, segment: usize) -> Node {
    element_node(
        "span",
        XHTML_NAMESPACE,
        &[("class", "koboSpan"), ("id", &format!("kobo.{paragraph}.{segment}"))],
    )
}

fn descendant_ids(html: &Html, root: NodeId) -> Vec<NodeId> {
    html.tree
        .get(root)
        .into_iter()
        .flat_map(|node| std::iter::once(node).chain(node.descendants()))
        .map(|node| node.id())
        .collect()
}

fn find_element(html: &Html, name: &str) -> Option<NodeId> {
    descendant_ids(html, html.tree.root().id()).into_iter().find(|id| {
        html.tree
            .get(*id)
            .and_then(|node| node.value().as_element())
            .is_some_and(|element| element.name().eq_ignore_ascii_case(name))
    })
}

fn element_attr<'a>(element: &'a scraper::node::Element, name: &str) -> Option<&'a str> {
    element
        .attrs
        .iter()
        .find(|(attribute, _)| attribute.local.as_ref() == name)
        .map(|(_, value)| value.as_ref())
}

fn leading_xml_declaration(content: &str) -> Option<&str> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    content
        .strip_prefix("<?xml")
        .and_then(|rest| rest.find("?>").map(|end| &content[..5 + end + 2]))
}

fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_content_document(media_type: &str, href: &str) -> bool {
    matches!(media_type, "application/xhtml+xml" | "text/html")
        || href
            .rsplit_once('.')
            .is_some_and(|(_, extension)| matches!(extension.to_ascii_lowercase().as_str(), "htm" | "html" | "xhtml"))
}

fn should_filter(name: &str) -> bool {
    let basename = name.rsplit('/').next().unwrap_or(name);
    matches!(
        basename,
        "calibre_bookmarks.txt"
            | "iTunesMetadata.plist"
            | "iTunesArtwork.plist"
            | ".DS_STORE"
            | "thumbs.db"
            | "Thumbs.db"
    ) || name.starts_with("__MACOSX/")
}

fn is_directory_name(name: &str) -> bool {
    name.ends_with('/')
}

fn normalize_archive_href(base: &str, href: &str) -> Result<String, String> {
    if href.contains("://") || href.starts_with("data:") {
        return Err(format!("external manifest href is unsupported: {href:?}"));
    }
    let plus_decoded = href.replace('+', " ");
    let decoded = percent_decode_str(&plus_decoded)
        .decode_utf8()
        .map_err(|error| error.to_string())?;
    let path = decoded.split_once('#').map_or(decoded.as_ref(), |(path, _)| path);
    if path.starts_with('/') || path.contains('\\') {
        return Err(format!("unsafe manifest href: {href:?}"));
    }
    let mut components = Vec::new();
    for component in base
        .split('/')
        .chain(path.split('/'))
        .filter(|component| !component.is_empty() && *component != ".")
    {
        if component == ".." {
            components
                .pop()
                .ok_or_else(|| format!("manifest href escapes archive root: {href:?}"))?;
        } else {
            components.push(component);
        }
    }
    Ok(components.join("/"))
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

fn join_archive_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn resolve_exact(index: &ArchiveIndex, name: &str) -> Option<usize> {
    index.by_name.get(name).copied()
}

fn resolve_with_case_fallback(index: &ArchiveIndex, name: &str) -> Result<Option<usize>, KepubConversionError> {
    if let Some(index) = resolve_exact(index, name) {
        return Ok(Some(index));
    }
    Ok(index.by_lowercase.get(&name.to_lowercase()).copied())
}

fn read_document<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<u8>, KepubConversionError> {
    check_cancelled(cancelled)?;
    let mut entry = archive.by_index(index).map_err(invalid_zip)?;
    if entry.size() > MAX_DOCUMENT_SIZE {
        return Err(KepubConversionError::ResourceLimit(format!(
            "document {:?} exceeds {MAX_DOCUMENT_SIZE} bytes",
            entry.name()
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry.read_to_end(&mut bytes).map_err(KepubConversionError::Io)?;
    check_cancelled(cancelled)?;
    Ok(bytes)
}

fn add_document_size(total: &mut u64, size: usize) -> Result<(), KepubConversionError> {
    *total = total
        .checked_add(u64::try_from(size).unwrap_or(u64::MAX))
        .ok_or_else(|| KepubConversionError::ResourceLimit("converted document size overflowed".into()))?;
    if *total > MAX_DOCUMENTS_SIZE {
        return Err(KepubConversionError::ResourceLimit(format!(
            "converted documents exceed {MAX_DOCUMENTS_SIZE} bytes"
        )));
    }
    Ok(())
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes)
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> Result<(), KepubConversionError> {
    if cancelled() {
        Err(KepubConversionError::Cancelled)
    } else {
        Ok(())
    }
}

fn local_name(node: XmlNode<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

fn local_event_name_from_start(start: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(local_bytes_name(start.name().as_ref())).into_owned()
}

fn local_event_name_from_end(end: &BytesEnd<'_>) -> String {
    String::from_utf8_lossy(local_bytes_name(end.name().as_ref())).into_owned()
}

fn local_bytes_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn sibling_name(parent: &str, local: &str) -> String {
    parent
        .rsplit_once(':')
        .map_or_else(|| local.to_owned(), |(prefix, _)| format!("{prefix}:{local}"))
}

fn invalid_zip(error: ZipError) -> KepubConversionError {
    match error {
        ZipError::Io(error) => KepubConversionError::Io(error),
        error
            if {
                let message = error.to_string().to_ascii_lowercase();
                message.contains("duplicate") && message.contains("filename")
            } =>
        {
            KepubConversionError::UnsafeArchive(error.to_string())
        }
        error => KepubConversionError::InvalidEpub(error.to_string()),
    }
}

fn write_zip(error: ZipError) -> KepubConversionError {
    match error {
        ZipError::Io(error) => KepubConversionError::Io(error),
        error => KepubConversionError::ContentConversion(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        path::{Path, PathBuf},
        process::Command,
    };

    use serde::{Deserialize, Serialize};
    use zip::write::{FullFileOptions, SimpleFileOptions};

    use super::*;

    fn epub(content: &str, opf_extra: &str) -> Cursor<Vec<u8>> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer.set_comment("source archive comment").unwrap();
        writer
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(EPUB_MIMETYPE.as_bytes()).unwrap();
        writer
            .start_file("META-INF/container.xml", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><!DOCTYPE container><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            )
            .unwrap();
        writer
            .start_file("OEBPS/content.opf", SimpleFileOptions::default())
            .unwrap();
        write!(
            writer,
            r#"<?xml version="1.0"?><!DOCTYPE package><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata><meta name="cover" content="cover"/><meta name="calibre:timestamp" content="now"/><contributor role="bkp">calibre</contributor></metadata><manifest><item id="cover" href="cover.jpg" media-type="image/jpeg"/><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/>{opf_extra}</manifest><spine><itemref idref="chapter"/></spine></package>"#
        )
        .unwrap();
        writer
            .start_file("OEBPS/chapter.xhtml", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(content.as_bytes()).unwrap();
        writer
            .start_file(
                "OEBPS/cover.jpg",
                FullFileOptions::default()
                    .compression_method(CompressionMethod::Stored)
                    .unix_permissions(0o640)
                    .with_file_comment("cover comment"),
            )
            .unwrap();
        writer.write_all(b"jpeg").unwrap();
        writer
            .start_file("META-INF/calibre_bookmarks.txt", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"filtered").unwrap();
        writer
            .start_file("__MACOSX/junk", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"filtered").unwrap();
        writer.finish().unwrap()
    }

    fn converted(content: &str) -> Vec<u8> {
        let source = epub(content, "");
        let mut destination = Cursor::new(Vec::new());
        convert_epub_to_kepub(source, &mut destination, || false).unwrap();
        destination.into_inner()
    }

    #[test]
    fn sentence_split_matches_kepubify_boundaries() {
        assert_eq!(
            split_sentences("One. Two!  Three?\nFour"),
            ["One. ", "Two!  ", "Three?\n", "Four"]
        );
        assert_eq!(
            split_sentences("Wait... \"Yes?\" Next"),
            ["Wait... ", "\"Yes?\" ", "Next"]
        );
        assert_eq!(split_sentences(""), [""]);
    }

    #[test]
    fn transforms_wrappers_spans_images_and_skipped_content() {
        let output = converted(
            r#"<?xml version="1.0"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><meta charset="windows-1252"/></head><body><p>Hello <em>world.</em> Next.</p><img src="cover.jpg"/><pre>Not split. Next.</pre><svg><text>Vector text.</text></svg><math><mi>x.</mi></math></body></html>"#,
        );
        let mut archive = ZipArchive::new(Cursor::new(output)).unwrap();
        let mut chapter = String::new();
        archive
            .by_name("OEBPS/chapter.xhtml")
            .unwrap()
            .read_to_string(&mut chapter)
            .unwrap();
        assert!(chapter.starts_with("<?xml version=\"1.0\"?>"));
        assert!(chapter.contains("<div id=\"book-columns\"><div id=\"book-inner\">"));
        assert!(chapter.contains("class=\"kobostylehacks\""));
        assert!(chapter.contains("id=\"kobo.1.1\""));
        assert!(chapter.contains("id=\"kobo.2.1\"><img"));
        assert!(chapter.contains("<pre>Not split. Next.</pre>"));
        assert!(!chapter.contains("<text><span"));
        assert!(chapter.contains("charset=\"UTF-8\""));
    }

    #[test]
    fn rewrites_opf_and_emits_valid_archive_layout() {
        let source = epub(
            "<!DOCTYPE html><html><head><title></title></head><body><div>This chapter has enough ordinary words to require the dummy title page because it is clearly content rather than a cover page and continues for quite a while. More text follows here.</div></body></html>",
            "",
        );
        let source_bytes = source.into_inner();
        let mut destination = Cursor::new(Vec::new());
        convert_epub_to_kepub(Cursor::new(source_bytes.clone()), &mut destination, || false).unwrap();
        validate_kepub_conversion(
            Cursor::new(source_bytes),
            Cursor::new(destination.get_ref().clone()),
            || false,
        )
        .unwrap();
        let mut archive = ZipArchive::new(Cursor::new(destination.into_inner())).unwrap();
        assert_eq!(archive.comment(), b"source archive comment");
        let mimetype = archive.by_index(0).unwrap();
        assert_eq!(mimetype.name(), "mimetype");
        assert_eq!(mimetype.compression(), CompressionMethod::Stored);
        drop(mimetype);
        let opf_index = (0..archive.len())
            .find(|index| archive.by_index(*index).unwrap().name() == "OEBPS/content.opf")
            .unwrap();
        assert_eq!(
            archive.by_index(opf_index + 1).unwrap().name(),
            "OEBPS/kepubify-titlepage-dummy.xhtml"
        );
        assert!(archive.by_name("META-INF/calibre_bookmarks.txt").is_err());
        assert!(archive.by_name("__MACOSX/junk").is_err());
        let mut cover = archive.by_name("OEBPS/cover.jpg").unwrap();
        assert_eq!(cover.compression(), CompressionMethod::Stored);
        assert_eq!(cover.unix_mode().map(|mode| mode & 0o777), Some(0o640));
        assert_eq!(cover.comment(), "cover comment");
        let mut cover_bytes = Vec::new();
        cover.read_to_end(&mut cover_bytes).unwrap();
        assert_eq!(cover_bytes, b"jpeg");
        drop(cover);
        let mut opf = String::new();
        archive
            .by_name("OEBPS/content.opf")
            .unwrap()
            .read_to_string(&mut opf)
            .unwrap();
        assert!(opf.contains("properties=\"cover-image\""));
        assert!(!opf.contains("calibre:timestamp"));
        assert!(!opf.contains("role=\"bkp\""));
        assert!(opf.contains("idref=\"kepubify-titlepage-dummy\""));
    }

    #[test]
    fn rejects_unsafe_names_before_conversion() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../book.xhtml", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        let source = writer.finish().unwrap();
        let error = convert_epub_to_kepub(source, Cursor::new(Vec::new()), || false).unwrap_err();
        assert!(matches!(error, KepubConversionError::UnsafeArchive(_)), "{error:?}");
    }

    #[test]
    fn rejects_backslashes_duplicates_case_ambiguity_and_symlinks() {
        for names in [vec!["bad\\name"], vec!["Text/Chapter.xhtml", "text/chapter.xhtml"]] {
            let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
            for name in names {
                writer.start_file(name, SimpleFileOptions::default()).unwrap();
                writer.write_all(b"entry").unwrap();
            }
            let source = writer.finish().unwrap();
            let error = convert_epub_to_kepub(source, Cursor::new(Vec::new()), || false).unwrap_err();
            assert!(matches!(error, KepubConversionError::UnsafeArchive(_)));
        }

        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["same", "sane"] {
            writer.start_file(name, SimpleFileOptions::default()).unwrap();
            writer.write_all(b"entry").unwrap();
        }
        let mut duplicate = writer.finish().unwrap().into_inner();
        let mut offset = 0;
        while let Some(relative) = duplicate[offset..].windows(4).position(|window| window == b"sane") {
            let position = offset + relative;
            duplicate[position..position + 4].copy_from_slice(b"same");
            offset = position + 4;
        }
        let error = convert_epub_to_kepub(Cursor::new(duplicate), Cursor::new(Vec::new()), || false).unwrap_err();
        assert!(matches!(error, KepubConversionError::UnsafeArchive(_)), "{error:?}");

        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink("link", "target", SimpleFileOptions::default())
            .unwrap();
        let source = writer.finish().unwrap();
        let error = convert_epub_to_kepub(source, Cursor::new(Vec::new()), || false).unwrap_err();
        assert!(matches!(error, KepubConversionError::UnsafeArchive(_)));
    }

    #[test]
    fn rejects_oversized_content_document() {
        let oversized = "x".repeat(usize::try_from(MAX_DOCUMENT_SIZE).unwrap() + 1);
        let error = convert_epub_to_kepub(epub(&oversized, ""), Cursor::new(Vec::new()), || false).unwrap_err();
        assert!(matches!(error, KepubConversionError::ResourceLimit(_)));
    }

    #[test]
    fn cancellation_is_not_reported_as_invalid_content() {
        let error = convert_epub_to_kepub(
            epub("<!DOCTYPE html><html><body><p>Text.</p></body></html>", ""),
            Cursor::new(Vec::new()),
            || true,
        )
        .unwrap_err();
        assert!(matches!(error, KepubConversionError::Cancelled));
    }

    const KEPUBIFY_GOLDEN: &str = include_str!("../tests/fixtures/kepubify-v4.0.4-semantics.json");
    const KEPUBIFY_V4_0_4_SHA256: &str = "37d7628d26c5c906f607f24b36f781f306075e7073a6fe7820a751bb60431fc5";

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    struct KepubGoldenCorpus {
        generator: String,
        binary_sha256: String,
        cases: Vec<KepubGoldenCase>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct KepubGoldenCase {
        name: String,
        entries: Vec<String>,
        package: PackageSemantics,
        documents: Vec<DocumentSemantics>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    struct PackageSemantics {
        path: String,
        element_count: usize,
        semantic_hash: String,
        manifest: Vec<String>,
        spine: Vec<String>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "camelCase")]
    struct DocumentSemantics {
        path: String,
        element_count: usize,
        attribute_count: usize,
        span_count: usize,
        semantic_hash: String,
        first_spans: Vec<String>,
        last_spans: Vec<String>,
    }

    #[test]
    fn semantic_corpus_matches_kepubify_v4_0_4_golden() {
        if std::env::var_os("PRINT_KEPUBIFY_GOLDEN").is_some() {
            let executable = std::env::var_os("KEPUBIFY_BIN").expect("PRINT_KEPUBIFY_GOLDEN requires KEPUBIFY_BIN");
            let upstream = summarize_corpus(Some(PathBuf::from(executable)));
            println!("{}", serde_json::to_string_pretty(&upstream).unwrap());
            return;
        }

        let expected: KepubGoldenCorpus = serde_json::from_str(KEPUBIFY_GOLDEN).unwrap();
        assert_eq!(expected.generator, "kepubify v4.0.4");
        assert_eq!(expected.binary_sha256, KEPUBIFY_V4_0_4_SHA256);

        let internal = summarize_corpus(None);
        assert_eq!(
            internal, expected,
            "in-process KEPUB semantics drifted from the v4.0.4 golden"
        );

        if let Some(executable) = std::env::var_os("KEPUBIFY_BIN") {
            let upstream = summarize_corpus(Some(PathBuf::from(executable)));
            assert_eq!(
                upstream, expected,
                "checked-in golden does not match the configured kepubify binary"
            );
        }
    }

    fn summarize_corpus(executable: Option<PathBuf>) -> KepubGoldenCorpus {
        let cases = kepub_corpus_sources()
            .into_iter()
            .map(|(name, source)| {
                let output = match executable.as_ref() {
                    Some(executable) => convert_with_kepubify(executable, name, &source),
                    None => {
                        let mut output = Cursor::new(Vec::new());
                        convert_epub_to_kepub(Cursor::new(source), &mut output, || false).unwrap();
                        output.into_inner()
                    }
                };
                summarize_kepub(name, output)
            })
            .collect();
        KepubGoldenCorpus {
            generator: "kepubify v4.0.4".into(),
            binary_sha256: KEPUBIFY_V4_0_4_SHA256.into(),
            cases,
        }
    }

    fn kepub_corpus_sources() -> Vec<(&'static str, Vec<u8>)> {
        let fixture = |path: &str| {
            std::fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../komga/komga/src/test/resources")
                    .join(path),
            )
            .unwrap()
        };
        vec![
            (
                "synthetic-nested-dom",
                epub(
                    r#"<?xml version="1.0"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Test</title></head><body><h1>Heading.</h1><p>One sentence. <em>Nested sentence!</em> Final?</p><img src="cover.jpg"/><pre>Do not split. This.</pre><svg><text>Vector.</text></svg></body></html>"#,
                    "",
                )
                .into_inner(),
            ),
            (
                "kotlin-reflowable-navigation",
                fixture("epub/The Incomplete Theft - Ralph Burke.epub"),
            ),
            ("kotlin-fixed-layout", fixture("archives/epub3.epub")),
        ]
    }

    fn convert_with_kepubify(executable: &Path, name: &str, source: &[u8]) -> Vec<u8> {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join(format!("{name}.epub"));
        let output_path = temporary.path().join(format!("{name}.kepub.epub"));
        std::fs::write(&source_path, source).unwrap();
        let output = Command::new(executable)
            .arg(&source_path)
            .arg("-o")
            .arg(&output_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "kepubify failed for {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read(output_path).unwrap()
    }

    fn summarize_kepub(name: &str, bytes: Vec<u8>) -> KepubGoldenCase {
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let index = inspect_archive(&mut archive, &|| false).unwrap();
        let package = read_package(&mut archive, &index, &|| false).unwrap();
        let mut entries = index.names.clone();
        entries.sort();

        let mut opf = String::new();
        archive
            .by_name(&package.path)
            .unwrap()
            .read_to_string(&mut opf)
            .unwrap();
        let package_semantics = summarize_package(&package.path, &opf);
        let mut documents = package
            .content_paths
            .iter()
            .map(|path| {
                let mut content = String::new();
                archive.by_name(path).unwrap().read_to_string(&mut content).unwrap();
                summarize_document(path, &content)
            })
            .collect::<Vec<_>>();
        documents.sort_by(|left, right| left.path.cmp(&right.path));
        KepubGoldenCase {
            name: name.into(),
            entries,
            package: package_semantics,
            documents,
        }
    }

    fn summarize_package(path: &str, content: &str) -> PackageSemantics {
        let document = epub_package::parse_xml(content).unwrap();
        let manifest = document
            .descendants()
            .filter(|node| local_name(*node, "item"))
            .filter(|node| node.ancestors().any(|ancestor| local_name(ancestor, "manifest")))
            .map(|node| {
                ["id", "href", "media-type", "properties"]
                    .map(|attribute| node.attribute(attribute).unwrap_or_default())
                    .join("|")
            })
            .collect();
        let spine = document
            .descendants()
            .filter(|node| local_name(*node, "itemref"))
            .filter(|node| node.ancestors().any(|ancestor| local_name(ancestor, "spine")))
            .map(|node| {
                ["idref", "linear"]
                    .map(|attribute| node.attribute(attribute).unwrap_or_default())
                    .join("|")
            })
            .collect();
        let mut projection = String::new();
        let mut element_count = 0;
        for node in document.descendants() {
            if node.is_element() {
                element_count += 1;
                let mut attributes = node
                    .attributes()
                    .map(|attribute| format!("{}={}", attribute.name(), attribute.value()))
                    .collect::<Vec<_>>();
                attributes.sort();
                projection.push_str(&format!(
                    "{}|E|{}|{}\n",
                    node.ancestors().count(),
                    node.tag_name().name(),
                    attributes.join("|")
                ));
            } else if node.is_text() {
                let text = normalized_semantic_text(node.text().unwrap_or_default());
                if !text.is_empty() {
                    projection.push_str(&format!("{}|T|{text}\n", node.ancestors().count()));
                }
            }
        }
        PackageSemantics {
            path: path.into(),
            element_count,
            semantic_hash: semantic_hash(&projection),
            manifest,
            spine,
        }
    }

    fn summarize_document(path: &str, content: &str) -> DocumentSemantics {
        let document = Html::parse_document(content);
        let mut projection = String::new();
        let mut element_count = 0;
        let mut attribute_count = 0;
        for node in document.tree.root().descendants() {
            if let Some(element) = node.value().as_element() {
                element_count += 1;
                attribute_count += element.attrs.len();
                let mut attributes = element
                    .attrs
                    .iter()
                    .map(|(attribute, value)| format!("{}={value}", attribute.local))
                    .collect::<Vec<_>>();
                attributes.sort();
                projection.push_str(&format!(
                    "{}|E|{}|{}\n",
                    node.ancestors().count(),
                    element.name.local,
                    attributes.join("|")
                ));
            } else if let Some(text) = node.value().as_text() {
                let text = normalized_semantic_text(&text.text);
                if !text.is_empty() {
                    projection.push_str(&format!("{}|T|{text}\n", node.ancestors().count()));
                }
            }
        }
        let selector = scraper::Selector::parse(".koboSpan[id]").unwrap();
        let spans = document
            .select(&selector)
            .map(|element| {
                format!(
                    "{}={}",
                    element.value().attr("id").unwrap_or_default(),
                    normalized_semantic_text(&element.text().collect::<String>())
                )
            })
            .collect::<Vec<_>>();
        let first_spans = spans.iter().take(3).cloned().collect();
        let last_spans = spans.iter().rev().take(3).cloned().rev().collect();
        DocumentSemantics {
            path: path.into(),
            element_count,
            attribute_count,
            span_count: spans.len(),
            semantic_hash: semantic_hash(&projection),
            first_spans,
            last_spans,
        }
    }

    fn normalized_semantic_text(value: &str) -> String {
        value.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn semantic_hash(value: &str) -> String {
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(value.as_bytes()))
    }
}
