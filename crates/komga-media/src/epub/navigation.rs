use std::fs::File;

use komga_core::EpubTocEntry;
use roxmltree::{Document, Node, ParsingOptions};
use zip::ZipArchive;

use super::package::{EpubPackage, is_element, normalize_href, parent, read_entry_text};

pub(super) fn toc(archive: &mut ZipArchive<File>, package: &EpubPackage) -> Result<Vec<EpubTocEntry>, String> {
    if let Some(entries) = nav_entries(archive, package, "toc")?
        && !entries.is_empty()
    {
        return Ok(entries);
    }
    ncx_entries(archive, package, "navMap", "navPoint")
}

pub(super) fn page_list(archive: &mut ZipArchive<File>, package: &EpubPackage) -> Result<Vec<EpubTocEntry>, String> {
    if let Some(entries) = nav_entries(archive, package, "page-list")?
        && !entries.is_empty()
    {
        return Ok(entries);
    }
    ncx_entries(archive, package, "pageList", "pageTarget")
}

pub(super) fn landmarks(archive: &mut ZipArchive<File>, package: &EpubPackage) -> Result<Vec<EpubTocEntry>, String> {
    if let Some(entries) = nav_entries(archive, package, "landmarks")?
        && !entries.is_empty()
    {
        return Ok(entries);
    }
    Ok(package.guide.clone())
}

fn nav_entries(
    archive: &mut ZipArchive<File>,
    package: &EpubPackage,
    nav_type: &str,
) -> Result<Option<Vec<EpubTocEntry>>, String> {
    let Some(nav) = &package.nav else {
        return Ok(None);
    };
    let content = read_entry_text(archive, &nav.path)?;
    parse_nav(&content, &nav.path, nav_type).map(Some)
}

fn parse_nav(content: &str, resource_path: &str, nav_type: &str) -> Result<Vec<EpubTocEntry>, String> {
    let document = Document::parse(content).map_err(|error| error.to_string())?;
    let nav_node = document.descendants().find(|node| {
        is_element(*node, "nav")
            && node
                .attributes()
                .any(|attribute| attribute.name().ends_with("type") && attribute.value() == nav_type)
    });
    let Some(nav_node) = nav_node else {
        return Ok(Vec::new());
    };
    let entries = direct_child(nav_node, "ol")
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| is_element(*node, "li"))
        .filter_map(|node| nav_item(node, parent(resource_path)).transpose())
        .collect::<Result<Vec<_>, String>>()?;
    Ok(entries)
}

fn nav_item(node: Node<'_, '_>, base_dir: &str) -> Result<Option<EpubTocEntry>, String> {
    let label = node
        .children()
        .find(|child| is_element(*child, "a") || is_element(*child, "span"));
    let Some(label) = label else {
        return Ok(None);
    };
    let title = normalized_text(label);
    let href = node
        .children()
        .find(|child| is_element(*child, "a"))
        .and_then(|anchor| anchor.attribute("href"))
        .map(|href| normalize_href(base_dir, href))
        .transpose()?;
    let children = direct_child(node, "ol")
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|child| is_element(*child, "li"))
        .filter_map(|child| nav_item(child, base_dir).transpose())
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Some(EpubTocEntry { title, href, children }))
}

fn ncx_entries(
    archive: &mut ZipArchive<File>,
    package: &EpubPackage,
    container_name: &str,
    item_name: &str,
) -> Result<Vec<EpubTocEntry>, String> {
    let Some(ncx) = &package.ncx else {
        return Ok(Vec::new());
    };
    let content = read_entry_text(archive, &ncx.path)?;
    parse_ncx(&content, &ncx.path, container_name, item_name)
}

fn parse_ncx(
    content: &str,
    resource_path: &str,
    container_name: &str,
    item_name: &str,
) -> Result<Vec<EpubTocEntry>, String> {
    let document = Document::parse_with_options(
        content,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let Some(container) = document.descendants().find(|node| is_element(*node, container_name)) else {
        return Ok(Vec::new());
    };
    container
        .children()
        .filter(|node| is_element(*node, item_name))
        .filter_map(|node| ncx_item(node, parent(resource_path), item_name).transpose())
        .collect()
}

fn ncx_item(node: Node<'_, '_>, base_dir: &str, item_name: &str) -> Result<Option<EpubTocEntry>, String> {
    let title = node
        .children()
        .find(|child| is_element(*child, "navLabel"))
        .and_then(|label| label.children().find(|child| is_element(*child, "text")))
        .map(normalized_text);
    let Some(title) = title else {
        return Ok(None);
    };
    let href = node
        .children()
        .find(|child| is_element(*child, "content"))
        .and_then(|content| content.attribute("src"))
        .map(|href| normalize_href(base_dir, href))
        .transpose()?;
    let children = node
        .children()
        .filter(|child| is_element(*child, item_name))
        .filter_map(|child| ncx_item(child, base_dir, item_name).transpose())
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Some(EpubTocEntry { title, href, children }))
}

fn direct_child<'a>(node: Node<'a, 'a>, name: &str) -> Option<Node<'a, 'a>> {
    node.children().find(|child| is_element(*child, name))
}

fn normalized_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn fixture(name: &str) -> String {
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../komga/komga/src/test/resources/epub")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn parses_epub3_navigation_with_nested_and_unlinked_entries() {
        let toc = parse_nav(&fixture("nav.xhtml"), "PREFIX/nav.xhtml", "toc").unwrap();
        assert_eq!(toc.len(), 7);
        assert_eq!(toc[0].href.as_deref(), Some("PREFIX/cover.xhtml"));
        assert_eq!(toc[4].title, "An unlinked heading");
        assert_eq!(toc[4].href, None);
        assert_eq!(toc[5].children.len(), 4);
        assert_eq!(toc[5].children[0].href.as_deref(), Some("PREFIX/chapter 001.xhtml"));

        let landmarks = parse_nav(&fixture("nav.xhtml"), "nav.xhtml", "landmarks").unwrap();
        assert_eq!(landmarks.len(), 2);
        assert_eq!(landmarks[0].href.as_deref(), Some("cover.xhtml#coverimage"));
        let pages = parse_nav(&fixture("nav.xhtml"), "nav.xhtml", "page-list").unwrap();
        assert_eq!(pages.len(), 8);
        assert_eq!(pages[2].href.as_deref(), Some("xhtml/chapter1.xhtml#pg_1"));
    }

    #[test]
    fn parses_epub2_ncx_toc_and_page_list() {
        let content = fixture("toc.ncx");
        let toc = parse_ncx(&content, "PREFIX/toc.ncx", "navMap", "navPoint").unwrap();
        assert_eq!(toc.len(), 5);
        assert_eq!(toc[2].children.len(), 6);
        assert_eq!(toc[2].children[5].children.len(), 2);
        assert_eq!(
            toc[4].href.as_deref(),
            Some("PREFIX/Text/First Omake - Ichika’s Q&A.xhtml")
        );
        let pages = parse_ncx(&content, "toc.ncx", "pageList", "pageTarget").unwrap();
        assert_eq!(pages.len(), 11);
        assert_eq!(pages[8].href.as_deref(), Some("xhtml/chapter1.xhtml#pg_1"));
    }
}
