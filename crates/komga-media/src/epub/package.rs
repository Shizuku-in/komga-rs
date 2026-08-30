//! EPUB container and OPF package parsing.
//!
//! Hrefs follow Kotlin's compatibility behavior: `+` is decoded as a space,
//! percent escapes are decoded before normalization, fragments are preserved,
//! and paths attempting to escape the archive root are rejected.

use std::{collections::BTreeSet, fs::File, io::Read, path::Path};

use komga_core::EpubTocEntry;
use percent_encoding::percent_decode_str;
use roxmltree::{Document, Node, ParsingOptions};
use zip::ZipArchive;

/// One normalized OPF manifest item used by later EPUB analysis stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManifestItem {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) media_type: String,
    pub(super) properties: BTreeSet<String>,
}

/// Parsed package structures needed for resources, navigation, and covers.
#[derive(Debug)]
pub(super) struct EpubPackage {
    pub(super) path: String,
    pub(super) manifest: Vec<ManifestItem>,
    pub(super) spine: Vec<ManifestItem>,
    pub(super) fixed_layout: bool,
    pub(super) guide: Vec<EpubTocEntry>,
    pub(super) cover: Option<ManifestItem>,
    pub(super) nav: Option<ManifestItem>,
    pub(super) ncx: Option<ManifestItem>,
}

/// Parses EPUB XML while accepting document type declarations without resolving
/// external entities.
pub(crate) fn parse_xml(content: &str) -> Result<Document<'_>, String> {
    Document::parse_with_options(
        content,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )
    .map_err(|error| error.to_string())
}

/// Opens the ZIP and resolves its root OPF through `META-INF/container.xml`.
pub(super) fn open(path: &Path) -> Result<(ZipArchive<File>, EpubPackage), String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let package_path = package_path(&mut archive)?;
    let package_xml = read_entry_text(&mut archive, &package_path)?;
    let mut package = parse_package(&package_xml, parent(&package_path))?;
    package.path = package_path;
    Ok((archive, package))
}

pub(crate) fn metadata_xml(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let path = package_path(&mut archive)?;
    read_entry_text(&mut archive, &path)
}

pub(super) fn has_epub_mimetype(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return false;
    };
    read_entry_text(&mut archive, "mimetype").is_ok_and(|value| value.trim() == "application/epub+zip")
}

pub(super) fn package_path(archive: &mut ZipArchive<File>) -> Result<String, String> {
    let container = read_entry_text(archive, "META-INF/container.xml")?;
    let document = parse_xml(&container)?;
    let rootfile = document
        .descendants()
        .find(|node| is_element(*node, "rootfile"))
        .and_then(|node| node.attribute("full-path"))
        .ok_or_else(|| "META-INF/container.xml does not contain a rootfile".to_owned())?;
    normalize_href("", rootfile).map(|value| remove_fragment(&value).to_owned())
}

pub(super) fn parse_package(xml: &str, base_dir: &str) -> Result<EpubPackage, String> {
    let document = parse_xml(xml)?;
    let manifest_node = document
        .descendants()
        .find(|node| is_element(*node, "manifest"))
        .ok_or_else(|| "OPF does not contain a manifest".to_owned())?;
    let manifest = manifest_node
        .children()
        .filter(|node| is_element(*node, "item"))
        .map(|node| {
            let id = node.attribute("id").unwrap_or_default().to_owned();
            let href = node.attribute("href").unwrap_or_default();
            let path = normalize_href(base_dir, href)?;
            let media_type = node.attribute("media-type").unwrap_or_default().to_owned();
            let properties = node
                .attribute("properties")
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_owned)
                .collect();
            Ok(ManifestItem {
                id,
                path: remove_fragment(&path).to_owned(),
                media_type,
                properties,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let spine = document
        .descendants()
        .find(|node| is_element(*node, "spine"))
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| is_element(*node, "itemref"))
        .filter_map(|node| node.attribute("idref"))
        .filter_map(|id| manifest.iter().find(|item| item.id == id).cloned())
        .collect::<Vec<_>>();

    let fixed_layout = document.descendants().any(|node| {
        is_element(node, "meta")
            && ((node.attribute("property") == Some("rendition:layout") && normalized_text(node) == "pre-paginated")
                || (node.attribute("name") == Some("fixed-layout") && node.attribute("content") == Some("true")))
    });

    let guide = document
        .descendants()
        .find(|node| is_element(*node, "guide"))
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| is_element(*node, "reference"))
        .map(|node| {
            let href = node
                .attribute("href")
                .filter(|value| !value.is_empty())
                .map(|value| normalize_href(base_dir, value))
                .transpose()?;
            Ok(EpubTocEntry {
                title: node.attribute("title").unwrap_or_default().to_owned(),
                href,
                children: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let cover_id = document.descendants().find_map(|node| {
        (is_element(node, "meta") && node.attribute("name") == Some("cover"))
            .then(|| node.attribute("content"))
            .flatten()
    });
    let cover = manifest
        .iter()
        .find(|item| item.properties.contains("cover-image"))
        .or_else(|| cover_id.and_then(|id| manifest.iter().find(|item| item.id == id)))
        .or_else(|| manifest.iter().find(|item| item.id == "cover-image"))
        .cloned();
    let nav = manifest.iter().find(|item| item.properties.contains("nav")).cloned();
    let ncx = manifest
        .iter()
        .find(|item| item.media_type == "application/x-dtbncx+xml")
        .or_else(|| {
            manifest
                .iter()
                .find(|item| matches!(item.id.to_ascii_lowercase().as_str(), "toc" | "ncx" | "ncxtoc"))
        })
        .cloned();

    Ok(EpubPackage {
        path: String::new(),
        manifest,
        spine,
        fixed_layout,
        guide,
        cover,
        nav,
        ncx,
    })
}

/// Resolves an encoded package href while preventing archive-root traversal.
pub(super) fn normalize_href(base_dir: &str, href: &str) -> Result<String, String> {
    let plus_decoded = href.replace('+', " ");
    let decoded = percent_decode_str(&plus_decoded)
        .decode_utf8()
        .map_err(|error| error.to_string())?;
    let (path, fragment) = decoded
        .split_once('#')
        .map_or((decoded.as_ref(), None), |(path, fragment)| (path, Some(fragment)));
    let mut components = Vec::new();
    for component in base_dir
        .split('/')
        .chain(path.trim_start_matches('/').split('/'))
        .filter(|component| !component.is_empty() && *component != ".")
    {
        if component == ".." {
            components
                .pop()
                .ok_or_else(|| format!("EPUB href escapes the archive root: {href:?}"))?;
        } else {
            components.push(component);
        }
    }
    let mut normalized = components.join("/");
    if let Some(fragment) = fragment.filter(|value| !value.is_empty()) {
        normalized.push('#');
        normalized.push_str(fragment);
    }
    Ok(normalized)
}

pub(super) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

pub(super) fn remove_fragment(path: &str) -> &str {
    path.split_once('#').map_or(path, |(path, _)| path)
}

pub(super) fn read_entry(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
    let mut entry = archive.by_name(name).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size().min(16 * 1024 * 1024)).unwrap_or(0));
    entry.read_to_end(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

pub(super) fn read_entry_text(archive: &mut ZipArchive<File>, name: &str) -> Result<String, String> {
    decode_xml_text(&read_entry(archive, name)?)
}

pub(crate) fn decode_xml_text(bytes: &[u8]) -> Result<String, String> {
    if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|error| error.to_string());
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return decode_utf16(bytes, u16::from_le_bytes);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return decode_utf16(bytes, u16::from_be_bytes);
    }
    if bytes.starts_with(&[0x3c, 0x00, 0x3f, 0x00]) {
        return decode_utf16(bytes, u16::from_le_bytes);
    }
    if bytes.starts_with(&[0x00, 0x3c, 0x00, 0x3f]) {
        return decode_utf16(bytes, u16::from_be_bytes);
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

fn decode_utf16(bytes: &[u8], decode: fn([u8; 2]) -> u16) -> Result<String, String> {
    let (chunks, remainder) = bytes.as_chunks::<2>();
    let code_units = chunks.iter().map(|chunk| decode(*chunk)).collect::<Vec<_>>();
    if !remainder.is_empty() {
        return Err("UTF-16 EPUB resource has an odd byte length".to_owned());
    }
    String::from_utf16(&code_units).map_err(|error| error.to_string())
}

pub(super) fn entry_sizes(archive: &mut ZipArchive<File>, name: &str) -> Option<(i64, u64)> {
    archive.by_name(name).ok().and_then(|entry| {
        i64::try_from(entry.size())
            .ok()
            .map(|size| (size, entry.compressed_size()))
    })
}

fn normalized_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn is_element(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().name().eq_ignore_ascii_case(name)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn utf16_bytes(value: &str, little_endian: bool) -> Vec<u8> {
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
    fn parses_opf_guide_and_normalizes_encoded_paths() {
        let content = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/epub/clash.opf"),
        )
        .unwrap();
        let package = parse_package(&content, "PREFIX").unwrap();
        assert_eq!(package.guide.len(), 3);
        assert_eq!(package.guide[0].title, "Table Of Contents");
        assert_eq!(
            package.guide[0].href.as_deref(),
            Some("PREFIX/Text/Mart_9780553897852_epub_toc_r1.htm")
        );
        assert_eq!(
            normalize_href("OPS/Text", "../Images/cover%20page.jpg#cover").unwrap(),
            "OPS/Images/cover page.jpg#cover"
        );
        assert!(normalize_href("", "../outside.xhtml").is_err());
    }

    #[test]
    fn keeps_manifest_items_with_missing_attributes_for_resource_level_errors() {
        let package = parse_package(
            r#"<package><manifest>
                <item id="page" href="page.xhtml" media-type="application/xhtml+xml"/>
                <item href="missing-id.css" media-type="text/css"/>
                <item id="missing-href" media-type="image/jpeg"/>
                <item id="missing-type" href="image.jpg"/>
              </manifest><spine><itemref idref="page"/></spine></package>"#,
            "OPS",
        )
        .unwrap();

        assert_eq!(package.manifest.len(), 4);
        assert_eq!(package.spine[0].path, "OPS/page.xhtml");
        assert_eq!(package.manifest[1].id, "");
        assert_eq!(package.manifest[1].path, "OPS/missing-id.css");
        assert_eq!(package.manifest[2].path, "OPS");
        assert_eq!(package.manifest[3].media_type, "");
    }

    #[test]
    fn decodes_standard_xml_encodings_and_accepts_doctype() {
        let xml = r#"<?xml version="1.0" encoding="UTF-16"?><!DOCTYPE package><package/>"#;
        for encoded in [utf16_bytes(xml, true), utf16_bytes(xml, false)] {
            let decoded = decode_xml_text(&encoded).unwrap();
            assert_eq!(decoded, xml);
            assert_eq!(parse_xml(&decoded).unwrap().root_element().tag_name().name(), "package");
            assert_eq!(decode_xml_text(&encoded[2..]).unwrap(), xml);
        }

        let mut utf8_bom = vec![0xef, 0xbb, 0xbf];
        utf8_bom.extend_from_slice(xml.as_bytes());
        assert_eq!(decode_xml_text(&utf8_bom).unwrap(), xml);
        assert!(decode_xml_text(&[0xff, 0xfe, 0x3c]).is_err());
    }
}
