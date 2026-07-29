//! Bounded MOBI-to-EPUB conversion used by analysis and resource delivery.

use std::{
    cell::Cell,
    collections::{BTreeSet, HashSet},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
};

use boko::{
    Book, Format,
    export::{EpubExporter, Exporter},
};
use encoding_rs::WINDOWS_1252;
use regex::{Captures, Regex};
use thiserror::Error;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

const MAX_ENTRIES: usize = 65_535;
const MAX_MARKUP_SIZE: usize = 16 * 1024 * 1024;
const MAX_TEXT_SIZE: usize = 256 * 1024 * 1024;
const MAX_UNCOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// MOBI section selected for a successful conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobiVariant {
    /// A KF8 section from a pure or hybrid publication.
    Kf8,
    /// The legacy MOBI6/7 section.
    Legacy,
}

/// Errors produced while materializing a MOBI publication as EPUB.
#[derive(Debug, Error)]
pub enum MobiConversionError {
    /// Conversion was stopped between bounded processing stages.
    #[error("MOBI conversion cancelled")]
    Cancelled,
    /// The publication is encrypted with Kindle DRM.
    #[error("DRM-protected MOBI files are not supported")]
    Drm,
    /// Input structure or generated EPUB structure is invalid.
    #[error("invalid MOBI publication: {0}")]
    Invalid(String),
    /// A configured record, markup, text, or output-size limit was exceeded.
    #[error("MOBI conversion limit exceeded: {0}")]
    Limit(&'static str),
    /// Source or destination I/O failed.
    #[error("MOBI conversion I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Checks the Palm database type/creator signature used by MOBI files.
pub fn is_mobi(path: &Path) -> bool {
    let mut signature = [0_u8; 8];
    File::open(path)
        .and_then(|mut file| {
            file.seek(SeekFrom::Start(60))?;
            file.read_exact(&mut signature)
        })
        .is_ok_and(|()| signature == *b"BOOKMOBI")
}

/// Converts an unencrypted MOBI6/7 or KF8-containing MOBI to a bounded UTF-8 EPUB.
///
/// KF8 is attempted first and accepted only after its spine, assets, cover, and
/// generated EPUB package validate. A malformed KF8 section falls back to the
/// legacy section. The destination should be a temporary path because an error
/// may leave a partial file.
///
/// # Errors
///
/// Returns [`MobiConversionError::Drm`] for encrypted input,
/// [`MobiConversionError::Cancelled`] when `cancelled` requests shutdown, and
/// an invalid/limit/I/O error for unusable publications.
pub fn convert_mobi_to_epub(
    source: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<MobiVariant, MobiConversionError> {
    if cancelled() {
        return Err(MobiConversionError::Cancelled);
    }
    validate_pdb(source)?;

    let kf8 = catch_attempt(|| attempt(source, destination, Format::Azw3, MobiVariant::Kf8, &cancelled));
    match kf8 {
        Ok(variant) => return Ok(variant),
        Err(MobiConversionError::Cancelled | MobiConversionError::Drm) => return kf8,
        Err(_) => {}
    }
    if cancelled() {
        return Err(MobiConversionError::Cancelled);
    }
    catch_attempt(|| attempt(source, destination, Format::Mobi, MobiVariant::Legacy, &cancelled))
}

fn catch_attempt(
    attempt: impl FnOnce() -> Result<MobiVariant, MobiConversionError>,
) -> Result<MobiVariant, MobiConversionError> {
    catch_unwind(AssertUnwindSafe(attempt)).unwrap_or_else(|_| {
        Err(MobiConversionError::Invalid(
            "MOBI parser rejected malformed offsets".to_owned(),
        ))
    })
}

fn attempt(
    source: &Path,
    destination: &Path,
    format: Format,
    variant: MobiVariant,
    cancelled: &impl Fn() -> bool,
) -> Result<MobiVariant, MobiConversionError> {
    let mut book = Book::open_format(source, format).map_err(classify_boko_error)?;
    let cover_path = validate_book(&mut book, cancelled)?;
    if cancelled() {
        return Err(MobiConversionError::Cancelled);
    }

    let mut intermediate = tempfile::tempfile()?;
    EpubExporter::new()
        .export(&mut book, &mut intermediate)
        .map_err(classify_boko_error)?;
    if cancelled() {
        return Err(MobiConversionError::Cancelled);
    }
    intermediate.seek(SeekFrom::Start(0))?;
    rewrite_epub(intermediate, destination, cover_path.as_deref(), cancelled)?;
    validate_generated_epub(destination)?;
    Ok(variant)
}

fn classify_boko_error(error: io::Error) -> MobiConversionError {
    if error.to_string().to_ascii_lowercase().contains("encrypt") {
        MobiConversionError::Drm
    } else {
        MobiConversionError::Invalid(error.to_string())
    }
}

fn validate_pdb(path: &Path) -> Result<(), MobiConversionError> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_UNCOMPRESSED_SIZE {
        return Err(MobiConversionError::Limit("source file exceeds 4 GiB"));
    }
    let mut header = [0_u8; 78];
    file.read_exact(&mut header)
        .map_err(|_| MobiConversionError::Invalid("truncated Palm database header".to_owned()))?;
    if header[60..68] != *b"BOOKMOBI" {
        return Err(MobiConversionError::Invalid("Palm database is not BOOKMOBI".to_owned()));
    }
    let record_count = usize::from(u16::from_be_bytes([header[76], header[77]]));
    if record_count == 0 || record_count > MAX_ENTRIES {
        return Err(MobiConversionError::Limit("invalid Palm record count"));
    }
    let table_len = record_count
        .checked_mul(8)
        .and_then(|value| value.checked_add(78))
        .ok_or(MobiConversionError::Limit("Palm record table overflow"))?;
    let mut table = vec![0_u8; table_len];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut table)
        .map_err(|_| MobiConversionError::Invalid("truncated Palm record table".to_owned()))?;
    let first_offset = read_be_u32(&table, 78)? as u64;
    let record_end = if record_count > 1 {
        read_be_u32(&table, 86)? as u64
    } else {
        file_len
    };
    if first_offset >= record_end || record_end > file_len || record_end - first_offset < 16 {
        return Err(MobiConversionError::Invalid(
            "invalid MOBI header record range".to_owned(),
        ));
    }
    let mut palmdoc = [0_u8; 16];
    file.seek(SeekFrom::Start(first_offset))?;
    file.read_exact(&mut palmdoc)?;
    if u16::from_be_bytes([palmdoc[12], palmdoc[13]]) != 0 {
        return Err(MobiConversionError::Drm);
    }
    Ok(())
}

fn read_be_u32(bytes: &[u8], offset: usize) -> Result<u32, MobiConversionError> {
    let end = offset
        .checked_add(4)
        .ok_or(MobiConversionError::Limit("offset overflow"))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| MobiConversionError::Invalid("record offset is out of range".to_owned()))?;
    Ok(u32::from_be_bytes(value.try_into().expect("four-byte slice")))
}

fn validate_book(book: &mut Book, cancelled: &impl Fn() -> bool) -> Result<Option<String>, MobiConversionError> {
    let spine = book.spine().to_vec();
    let assets = book.list_assets();
    let entry_count = spine
        .len()
        .checked_add(assets.len())
        .and_then(|value| value.checked_add(4))
        .ok_or(MobiConversionError::Limit("EPUB entry count overflow"))?;
    if spine.is_empty() {
        return Err(MobiConversionError::Invalid("publication has no spine".to_owned()));
    }
    if entry_count > MAX_ENTRIES {
        return Err(MobiConversionError::Limit("too many EPUB entries"));
    }

    let mut source_paths = HashSet::new();
    let mut text_size = 0_usize;
    for entry in spine {
        if cancelled() {
            return Err(MobiConversionError::Cancelled);
        }
        let source_path = book
            .source_id(entry.id)
            .ok_or_else(|| MobiConversionError::Invalid("spine item has no source path".to_owned()))?
            .to_owned();
        validate_relative_path(&source_path)?;
        if !source_paths.insert(source_path) {
            return Err(MobiConversionError::Invalid("duplicate spine path".to_owned()));
        }
        let content = book.load_raw(entry.id).map_err(classify_boko_error)?;
        if content.len() > MAX_MARKUP_SIZE {
            return Err(MobiConversionError::Limit("HTML item exceeds 16 MiB"));
        }
        text_size = text_size
            .checked_add(content.len())
            .ok_or(MobiConversionError::Limit("text size overflow"))?;
        if text_size > MAX_TEXT_SIZE {
            return Err(MobiConversionError::Limit("text content exceeds 256 MiB"));
        }
    }

    let mut asset_paths = HashSet::new();
    for path in &assets {
        if cancelled() {
            return Err(MobiConversionError::Cancelled);
        }
        let path = path.to_string_lossy().replace('\\', "/");
        validate_relative_path(&path)?;
        if !asset_paths.insert(path.clone()) {
            return Err(MobiConversionError::Invalid("duplicate asset path".to_owned()));
        }
        book.load_asset(Path::new(&path)).map_err(classify_boko_error)?;
    }

    let cover = book.metadata().cover_image.as_deref().and_then(|declared| {
        if asset_paths.contains(declared) {
            Some(declared.to_owned())
        } else {
            let stem = Path::new(declared).file_stem()?;
            asset_paths
                .iter()
                .find(|candidate| Path::new(candidate.as_str()).file_stem() == Some(stem))
                .cloned()
        }
    });
    if book.metadata().cover_image.is_some() && cover.is_none() {
        return Err(MobiConversionError::Invalid(
            "declared cover resource is missing".to_owned(),
        ));
    }
    Ok(cover)
}

fn validate_relative_path(path: &str) -> Result<(), MobiConversionError> {
    let candidate = Path::new(path);
    if path.is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(MobiConversionError::Invalid(
            "publication contains an unsafe resource path".to_owned(),
        ));
    }
    Ok(())
}

struct GeneratedEntry {
    name: String,
    compression: CompressionMethod,
    data: Vec<u8>,
}

fn rewrite_epub(
    intermediate: File,
    destination: &Path,
    cover_path: Option<&str>,
    cancelled: &impl Fn() -> bool,
) -> Result<(), MobiConversionError> {
    let mut archive = ZipArchive::new(intermediate).map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(MobiConversionError::Limit("too many generated EPUB entries"));
    }
    let mut total_size = 0_u64;
    let mut entries = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        if cancelled() {
            return Err(MobiConversionError::Cancelled);
        }
        let mut entry = archive
            .by_index(index)
            .map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        validate_relative_path(entry.name())?;
        total_size = total_size
            .checked_add(entry.size())
            .ok_or(MobiConversionError::Limit("generated size overflow"))?;
        if total_size > MAX_UNCOMPRESSED_SIZE {
            return Err(MobiConversionError::Limit("generated EPUB exceeds 4 GiB uncompressed"));
        }
        if is_markup(entry.name()) && entry.size() > MAX_MARKUP_SIZE as u64 {
            return Err(MobiConversionError::Limit("HTML/XML item exceeds 16 MiB"));
        }
        let mut data = Vec::with_capacity(
            usize::try_from(entry.size()).map_err(|_| MobiConversionError::Limit("entry size overflow"))?,
        );
        entry.read_to_end(&mut data)?;
        entries.push(GeneratedEntry {
            name: entry.name().to_owned(),
            compression: entry.compression(),
            data,
        });
    }

    rewrite_markup_entries(&mut entries, cover_path)?;
    let rewritten_size = entries.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(
                u64::try_from(entry.data.len())
                    .map_err(|_| MobiConversionError::Limit("rewritten entry size overflow"))?,
            )
            .ok_or(MobiConversionError::Limit("rewritten EPUB size overflow"))
    })?;
    if rewritten_size > MAX_UNCOMPRESSED_SIZE {
        return Err(MobiConversionError::Limit("rewritten EPUB exceeds 4 GiB uncompressed"));
    }
    let mut output = File::create(destination)?;
    let mut writer = ZipWriter::new(&mut output);
    for entry in entries {
        let method = if entry.name == "mimetype" {
            CompressionMethod::Stored
        } else {
            match entry.compression {
                CompressionMethod::Stored => CompressionMethod::Stored,
                _ => CompressionMethod::Deflated,
            }
        };
        let options = SimpleFileOptions::default().compression_method(method);
        writer
            .start_file(&entry.name, options)
            .map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
        writer.write_all(&entry.data)?;
    }
    writer
        .finish()
        .map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
    output.sync_all()?;
    Ok(())
}

fn rewrite_markup_entries(entries: &mut [GeneratedEntry], cover_path: Option<&str>) -> Result<(), MobiConversionError> {
    let filepos = Regex::new(r"(?i)#filepos([0-9]+)").expect("static regex");
    let kindle_embed = Regex::new(r#"(?i)kindle:embed:([0-9a-v]+)(?:\?[^"'\s<>\)]*)?"#).expect("static regex");
    let kindle_position = Regex::new(r#"(?i)kindle:pos:[^"'\s<>]+"#).expect("static regex");
    let asset_paths = entries
        .iter()
        .filter(|entry| entry.name.starts_with("OEBPS/images/"))
        .map(|entry| entry.name.trim_start_matches("OEBPS/").to_owned())
        .collect::<Vec<_>>();
    let requested_filepos = entries
        .iter()
        .filter(|entry| is_markup(&entry.name))
        .flat_map(|entry| {
            let content = decode_markup(&entry.data);
            filepos
                .captures_iter(&content)
                .filter_map(|capture| capture.get(1)?.as_str().parse::<usize>().ok())
                .collect::<Vec<_>>()
        })
        .collect::<BTreeSet<_>>();
    let mut valid_filepos = BTreeSet::new();
    if let Some(entry) = entries.iter_mut().find(|entry| is_html(&entry.name)) {
        let mut html = decode_markup(&entry.data);
        let original_len = html.len();
        for offset in requested_filepos.iter().rev().copied() {
            if offset > original_len {
                continue;
            }
            let insertion = nearest_tag_boundary(&html, offset);
            html.insert_str(insertion, &format!(r#"<a id="mobi-filepos-{offset}"></a>"#));
            valid_filepos.insert(offset);
        }
        entry.data = html.into_bytes();
    }

    for entry in entries.iter_mut().filter(|entry| is_html(&entry.name)) {
        let html = decode_markup(&entry.data);
        let html = filepos
            .replace_all(&html, |captures: &Captures<'_>| {
                let offset = captures[1].parse::<usize>().ok();
                offset
                    .filter(|offset| valid_filepos.contains(offset))
                    .map(|offset| format!("#mobi-filepos-{offset}"))
                    .unwrap_or_default()
            })
            .into_owned();
        let missing_embed = Cell::new(false);
        let html = kindle_embed
            .replace_all(&html, |captures: &Captures<'_>| {
                let index = boko::mobi::parse_base32(captures[1].as_bytes());
                asset_paths.get(index).cloned().unwrap_or_else(|| {
                    missing_embed.set(true);
                    String::new()
                })
            })
            .into_owned();
        let html = kindle_position.replace_all(&html, "").into_owned();
        if missing_embed.get() || html.to_ascii_lowercase().contains("kindle:embed:") {
            return Err(MobiConversionError::Invalid(
                "KF8 chapter references a missing image".to_owned(),
            ));
        }
        entry.data = html.into_bytes();
    }

    for entry in entries.iter_mut().filter(|entry| entry.name.ends_with(".ncx")) {
        let ncx = decode_markup(&entry.data);
        entry.data = filepos
            .replace_all(&ncx, |captures: &Captures<'_>| {
                let offset = captures[1].parse::<usize>().ok();
                offset
                    .filter(|offset| valid_filepos.contains(offset))
                    .map(|offset| format!("#mobi-filepos-{offset}"))
                    .unwrap_or_default()
            })
            .into_owned()
            .into_bytes();
    }

    if let Some(cover_path) = cover_path
        && let Some(opf) = entries.iter_mut().find(|entry| entry.name.ends_with(".opf"))
    {
        let patched = add_cover_markers(&decode_markup(&opf.data), cover_path)?;
        opf.data = patched.into_bytes();
    }
    for entry in entries.iter_mut().filter(|entry| is_markup(&entry.name)) {
        if entry.data.len() > MAX_MARKUP_SIZE {
            return Err(MobiConversionError::Limit("rewritten HTML/XML item exceeds 16 MiB"));
        }
    }
    Ok(())
}

fn decode_markup(bytes: &[u8]) -> String {
    let mut text = match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => WINDOWS_1252.decode(bytes).0.into_owned(),
    };
    for encoding in ["windows-1252", "Windows-1252", "cp1252", "CP1252"] {
        text = text.replace(encoding, "utf-8");
    }
    text
}

fn nearest_tag_boundary(html: &str, offset: usize) -> usize {
    let mut boundary = offset.min(html.len());
    while boundary > 0 && !html.is_char_boundary(boundary) {
        boundary -= 1;
    }
    html[..boundary].rfind('<').unwrap_or(0)
}

fn add_cover_markers(opf: &str, cover_path: &str) -> Result<String, MobiConversionError> {
    let escaped_path = xml_escape(cover_path);
    let href = format!(r#"href="{escaped_path}""#);
    let line = opf
        .lines()
        .find(|line| line.contains("<item ") && line.contains(&href))
        .ok_or_else(|| MobiConversionError::Invalid("cover is absent from the EPUB manifest".to_owned()))?;
    let id = attribute(line, "id")
        .ok_or_else(|| MobiConversionError::Invalid("cover manifest item has no id".to_owned()))?;
    let marked = if line.contains("properties=") {
        line.to_owned()
    } else {
        line.replacen("/>", r#" properties="cover-image"/>"#, 1)
    };
    let mut output = opf.replacen(line, &marked, 1);
    if !output.contains(r#"name="cover""#) {
        output = output.replacen(
            "  </metadata>",
            &format!(
                r#"    <meta name="cover" content="{}"/>{}"#,
                xml_escape(&id),
                "\n  </metadata>"
            ),
            1,
        );
    }
    Ok(output)
}

fn attribute(line: &str, name: &str) -> Option<String> {
    let prefix = format!(r#"{name}=""#);
    let start = line.find(&prefix)?.checked_add(prefix.len())?;
    let end = line[start..].find('"')?.checked_add(start)?;
    Some(line[start..end].to_owned())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn validate_generated_epub(path: &Path) -> Result<(), MobiConversionError> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file).map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
    let mimetype = {
        let mut entry = archive
            .by_name("mimetype")
            .map_err(|_| MobiConversionError::Invalid("generated EPUB has no mimetype".to_owned()))?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        bytes
    };
    if mimetype != b"application/epub+zip" {
        return Err(MobiConversionError::Invalid(
            "generated EPUB has an invalid mimetype".to_owned(),
        ));
    }
    let mut container = String::new();
    archive
        .by_name("META-INF/container.xml")
        .map_err(|_| MobiConversionError::Invalid("generated EPUB has no container".to_owned()))?
        .read_to_string(&mut container)?;
    let document =
        roxmltree::Document::parse(&container).map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
    let package_path = document
        .descendants()
        .find(|node| node.tag_name().name() == "rootfile")
        .and_then(|node| node.attribute("full-path"))
        .ok_or_else(|| MobiConversionError::Invalid("generated EPUB has no package path".to_owned()))?
        .to_owned();
    let mut package = String::new();
    archive
        .by_name(&package_path)
        .map_err(|_| MobiConversionError::Invalid("generated EPUB package is missing".to_owned()))?
        .read_to_string(&mut package)?;
    let package_document =
        roxmltree::Document::parse(&package).map_err(|error| MobiConversionError::Invalid(error.to_string()))?;
    let package_directory = Path::new(&package_path)
        .parent()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let manifest_hrefs = package_document
        .descendants()
        .filter(|node| node.tag_name().name() == "item")
        .filter_map(|node| node.attribute("href"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let manifest_ids = package_document
        .descendants()
        .filter(|node| node.tag_name().name() == "item")
        .filter_map(|node| node.attribute("id"))
        .collect::<HashSet<_>>();
    let spine = package_document
        .descendants()
        .filter(|node| node.tag_name().name() == "itemref")
        .filter_map(|node| node.attribute("idref"))
        .collect::<Vec<_>>();
    if spine.is_empty() || spine.iter().any(|id| !manifest_ids.contains(id)) {
        return Err(MobiConversionError::Invalid(
            "generated EPUB spine is incomplete".to_owned(),
        ));
    }
    drop(package_document);
    for href in manifest_hrefs {
        let href = href.split('#').next().unwrap_or_default();
        let resource = if package_directory.is_empty() {
            href.to_owned()
        } else {
            format!("{package_directory}/{href}")
        };
        validate_relative_path(&resource)?;
        archive
            .by_name(&resource)
            .map_err(|_| MobiConversionError::Invalid(format!("generated EPUB resource is missing: {resource}")))?;
    }
    Ok(())
}

fn is_html(path: &str) -> bool {
    let extension = PathBuf::from(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(extension.as_str(), "html" | "htm" | "xhtml")
}

fn is_markup(path: &str) -> bool {
    is_html(path)
        || Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "xml" | "opf" | "ncx"))
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor};

    use super::*;
    use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};

    #[test]
    fn cover_markers_support_epub_two_and_three_readers() {
        let opf = r#"<package><metadata>
  </metadata>
  <manifest>
    <item id="asset_0" href="images/cover.jpg" media-type="image/jpeg"/>
  </manifest></package>"#;
        let patched = add_cover_markers(opf, "images/cover.jpg").unwrap();
        assert!(patched.contains(r#"name="cover" content="asset_0""#));
        assert!(patched.contains(r#"properties="cover-image""#));
    }

    #[test]
    fn filepos_links_receive_stable_anchors_and_invalid_offsets_fall_back() {
        let mut entries = vec![
            GeneratedEntry {
                name: "OEBPS/content.html".to_owned(),
                compression: CompressionMethod::Deflated,
                data: br##"<html><body><a href="#filepos12">valid</a><a href="#filepos999">invalid</a></body></html>"##
                    .to_vec(),
            },
            GeneratedEntry {
                name: "OEBPS/toc.ncx".to_owned(),
                compression: CompressionMethod::Deflated,
                data: br#"<content src="content.html#filepos12"/>"#.to_vec(),
            },
        ];
        rewrite_markup_entries(&mut entries, None).unwrap();
        let html = String::from_utf8(entries[0].data.clone()).unwrap();
        let ncx = String::from_utf8(entries[1].data.clone()).unwrap();
        assert!(html.contains(r#"id="mobi-filepos-12""#));
        assert!(html.contains(r##"href="#mobi-filepos-12""##));
        assert!(html.contains(r#"href="""#));
        assert!(ncx.contains("content.html#mobi-filepos-12"));
    }

    #[test]
    fn palmdoc_mobi_is_converted_to_utf8_epub() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("sample.mobi");
        let destination = directory.path().join("sample.epub");
        let html =
            r#"<html><head><title>测试</title></head><body><img recindex="00001"/><p>Grüße 世界</p></body></html>"#;
        let cover = png();
        fs::write(&source, minimal_mobi(html.as_bytes(), true, false, Some(&cover))).unwrap();

        let variant = convert_mobi_to_epub(&source, &destination, || false).unwrap();

        assert_eq!(variant, MobiVariant::Legacy);
        let file = File::open(&destination).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let mut content = String::new();
        archive
            .by_name("OEBPS/content.html")
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert!(content.contains("Grüße 世界"));
        assert!(std::str::from_utf8(content.as_bytes()).is_ok());

        let book = komga_core::scan_book_file(&destination, "library", false).unwrap();
        let media = crate::analyze_materialized_mobi(&destination, &book.id, true, || false).unwrap();
        assert_eq!(media.status, komga_core::MediaStatus::Ready);
        assert_eq!(media.media_type.as_deref(), Some(komga_core::MOBI_MEDIA_TYPE));
        assert_eq!(media.profile(), Some(komga_core::MediaProfile::Epub));
        assert!(!media.epub_divina_compatible);
        assert!(!media.epub_is_kepub);
        assert!(matches!(media.extension, Some(komga_core::MediaExtension::Epub(_))));
        assert!(
            media
                .files
                .iter()
                .any(|file| file.media_type.as_deref() == Some("image/png"))
        );
        let metadata = crate::extract_epub_metadata(&book, &media).unwrap().unwrap();
        assert_eq!(metadata.book.title.as_deref(), Some("MOBI 测试"));
        let thumbnail = crate::generate_thumbnail(&book, &media, 300).unwrap();
        assert_eq!(thumbnail.media_type, komga_core::JPEG_MEDIA_TYPE);
    }

    #[test]
    fn kf8_publication_is_selected_before_legacy_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let legacy = directory.path().join("legacy.mobi");
        let intermediate = directory.path().join("intermediate.epub");
        let kf8 = directory.path().join("kf8.mobi");
        let converted = directory.path().join("converted.epub");
        fs::write(
            &legacy,
            minimal_mobi(
                "<html><body><p>混合格式优先 KF8</p></body></html>".as_bytes(),
                true,
                false,
                Some(&png()),
            ),
        )
        .unwrap();
        assert_eq!(
            convert_mobi_to_epub(&legacy, &intermediate, || false).unwrap(),
            MobiVariant::Legacy
        );

        let mut book = Book::open_format(&intermediate, Format::Epub).unwrap();
        let mut output = File::create(&kf8).unwrap();
        boko::export::Azw3Exporter::new()
            .export(&mut book, &mut output)
            .unwrap();
        drop(output);

        assert_eq!(
            convert_mobi_to_epub(&kf8, &converted, || false).unwrap(),
            MobiVariant::Kf8
        );
        validate_generated_epub(&converted).unwrap();
    }

    #[test]
    fn encrypted_mobi_is_reported_as_drm() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("encrypted.mobi");
        let destination = directory.path().join("encrypted.epub");
        fs::write(
            &source,
            minimal_mobi(b"<html><body>DRM</body></html>", false, true, None),
        )
        .unwrap();

        assert!(matches!(
            convert_mobi_to_epub(&source, &destination, || false),
            Err(MobiConversionError::Drm)
        ));
        let book = komga_core::scan_book_file(&source, "library", false).unwrap();
        let media = crate::analyze_book(&book, true, || false).unwrap();
        assert_eq!(media.status, komga_core::MediaStatus::Unsupported);
        assert_eq!(media.comment.as_deref(), Some("ERR_1040"));
        assert_eq!(media.media_type.as_deref(), Some(komga_core::MOBI_MEDIA_TYPE));
    }

    #[test]
    fn cancellation_malformed_offsets_and_markup_limits_are_enforced() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mobi");
        let destination = directory.path().join("converted.epub");
        fs::write(
            &source,
            minimal_mobi(b"<html><body>cancel</body></html>", false, false, None),
        )
        .unwrap();
        assert!(matches!(
            convert_mobi_to_epub(&source, &destination, || true),
            Err(MobiConversionError::Cancelled)
        ));
        assert!(!destination.exists());

        let mut malformed = fs::read(&source).unwrap();
        malformed[86..90].copy_from_slice(&u32::MAX.to_be_bytes());
        fs::write(&source, malformed).unwrap();
        assert!(matches!(
            convert_mobi_to_epub(&source, &destination, || false),
            Err(MobiConversionError::Invalid(_))
        ));

        let oversized = vec![b'x'; MAX_MARKUP_SIZE + 1];
        fs::write(&source, minimal_mobi(&oversized, false, false, None)).unwrap();
        assert!(matches!(
            convert_mobi_to_epub(&source, &destination, || false),
            Err(MobiConversionError::Limit("HTML item exceeds 16 MiB"))
        ));
    }

    fn minimal_mobi(html: &[u8], palmdoc: bool, encrypted: bool, cover: Option<&[u8]>) -> Vec<u8> {
        let text = if palmdoc {
            boko::mobi::palmdoc::compress(html)
        } else {
            html.to_vec()
        };
        let title = "MOBI 测试".as_bytes();
        let author = "Komga Test".as_bytes();
        let cover_offset = 0_u32.to_be_bytes();
        let mut exth = Vec::new();
        exth.extend_from_slice(b"EXTH");
        exth.extend_from_slice(&0_u32.to_be_bytes());
        exth.extend_from_slice(&3_u32.to_be_bytes());
        for (kind, value) in [(503_u32, title), (100_u32, author), (201_u32, &cover_offset)] {
            exth.extend_from_slice(&kind.to_be_bytes());
            exth.extend_from_slice(&(u32::try_from(value.len() + 8).unwrap()).to_be_bytes());
            exth.extend_from_slice(value);
        }
        let exth_len = u32::try_from(exth.len()).unwrap();
        exth[4..8].copy_from_slice(&exth_len.to_be_bytes());
        let exth_start = 248_usize;
        let title_offset = exth_start + 12 + 8;
        let mut record_zero = vec![0_u8; exth_start + exth.len()];
        record_zero[0..2].copy_from_slice(&(if palmdoc { 2_u16 } else { 1_u16 }).to_be_bytes());
        record_zero[4..8].copy_from_slice(&(u32::try_from(html.len()).unwrap()).to_be_bytes());
        record_zero[8..10].copy_from_slice(&1_u16.to_be_bytes());
        record_zero[10..12].copy_from_slice(&4096_u16.to_be_bytes());
        record_zero[12..14].copy_from_slice(&(u16::from(encrypted)).to_be_bytes());
        record_zero[16..20].copy_from_slice(b"MOBI");
        record_zero[20..24].copy_from_slice(&232_u32.to_be_bytes());
        record_zero[24..28].copy_from_slice(&2_u32.to_be_bytes());
        record_zero[28..32].copy_from_slice(&65001_u32.to_be_bytes());
        record_zero[0x54..0x58].copy_from_slice(&(u32::try_from(title_offset).unwrap()).to_be_bytes());
        record_zero[0x58..0x5c].copy_from_slice(&(u32::try_from(title.len()).unwrap()).to_be_bytes());
        record_zero[0x68..0x6c].copy_from_slice(&6_u32.to_be_bytes());
        record_zero[0x6c..0x70].copy_from_slice(&cover.map(|_| 2_u32).unwrap_or(u32::MAX).to_be_bytes());
        for offset in [0x70, 0x74, 0xf4] {
            record_zero[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        }
        record_zero[0x80..0x84].copy_from_slice(&0x40_u32.to_be_bytes());
        record_zero[exth_start..].copy_from_slice(&exth);

        let record_count = if cover.is_some() { 3_u16 } else { 2_u16 };
        let header_size = 78 + usize::from(record_count) * 8 + 2;
        let record_one_offset = header_size + record_zero.len();
        let record_two_offset = record_one_offset + text.len();
        let mut pdb = vec![0_u8; header_size];
        pdb[..9].copy_from_slice(b"MOBI test");
        pdb[60..68].copy_from_slice(b"BOOKMOBI");
        pdb[76..78].copy_from_slice(&record_count.to_be_bytes());
        pdb[78..82].copy_from_slice(&(u32::try_from(header_size).unwrap()).to_be_bytes());
        pdb[86..90].copy_from_slice(&(u32::try_from(record_one_offset).unwrap()).to_be_bytes());
        if cover.is_some() {
            pdb[94..98].copy_from_slice(&(u32::try_from(record_two_offset).unwrap()).to_be_bytes());
        }
        pdb.extend_from_slice(&record_zero);
        pdb.extend_from_slice(&text);
        if let Some(cover) = cover {
            pdb.extend_from_slice(cover);
        }
        pdb
    }

    fn png() -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 3, Rgba([10, 20, 30, 255])));
        let mut output = Cursor::new(Vec::new());
        image.write_to(&mut output, ImageFormat::Png).unwrap();
        output.into_inner()
    }
}
