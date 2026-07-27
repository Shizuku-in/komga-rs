use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use komga_core::{Book, EPUB_MEDIA_TYPE, HashedPageTarget, Media, MediaStatus};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};
use roxmltree::Document;
use url::Url;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::package::{self, EpubPackage, ManifestItem};
use crate::{
    MediaAnalysisError, PageRemovalError, PageRemovalRewrite, analyze_book, hash_page,
    page_removal::CreatedOutput,
    zip_edit::{ensure_book_snapshot, ensure_same_metadata, scan_timestamp, source_fingerprint},
};

const MAX_ENTRIES: usize = 65_535;
const MAX_DOCUMENT_SIZE: u64 = 16 * 1024 * 1024;
const MAX_UNCOMPRESSED_SIZE: u64 = 4 * 1024 * 1024 * 1024;

/// Writes a validated fixed-layout EPUB without selected snapshot-bound pages.
///
/// The source is never modified and `output` must not exist. A page's spine
/// document and exclusive image are removed only after package and resource
/// references prove that doing so cannot leave a dangling link.
///
/// # Errors
///
/// Returns [`PageRemovalError::Unsupported`] for non-Divina EPUBs and EPUBs
/// whose target documents are still referenced, [`PageRemovalError::Stale`]
/// for snapshot mismatches, and [`PageRemovalError::Validation`] if the
/// rewritten publication does not contain exactly the expected survivors.
pub fn rewrite_hashed_epub_pages(
    book: &Book,
    media: &Media,
    targets: &[HashedPageTarget],
    output: &Path,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<PageRemovalRewrite, PageRemovalError> {
    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    if media.status != MediaStatus::Ready
        || media.media_type.as_deref() != Some(EPUB_MEDIA_TYPE)
        || !media.epub_divina_compatible
    {
        return Err(PageRemovalError::Unsupported(
            "media must be a ready Divina-compatible EPUB".into(),
        ));
    }
    if targets.is_empty() {
        return Err(PageRemovalError::Stale("no pages were requested".into()));
    }
    let path = book
        .url
        .to_file_path()
        .map_err(|()| PageRemovalError::Unsupported("book URL is not a local file".into()))?;
    if path == output {
        return Err(PageRemovalError::Unsupported(
            "output must differ from the source".into(),
        ));
    }
    let initial_metadata = fs::metadata(&path)?;
    ensure_book_snapshot(book, &initial_metadata)?;
    let initial_fingerprint = source_fingerprint(&path, &cancelled)?;

    let mut archive = ZipArchive::new(File::open(&path)?).map_err(archive_error)?;
    validate_archive(&mut archive, &cancelled)?;
    let package_path = package::package_path(&mut archive).map_err(PageRemovalError::Archive)?;
    let opf = read_document(&mut archive, &package_path, &cancelled)?;
    let opf_text = std::str::from_utf8(&opf)
        .map_err(|error| PageRemovalError::Unsupported(format!("EPUB package is not UTF-8: {error}")))?;
    let mut package =
        package::parse_package(opf_text, package::parent(&package_path)).map_err(PageRemovalError::Archive)?;
    package.path = package_path;
    validate_package(&package)?;
    if package.spine.len() != media.pages.len() {
        return Err(PageRemovalError::Unsupported(
            "EPUB spine does not map one-to-one to analyzed pages".into(),
        ));
    }
    let target_indices = validate_targets(&mut archive, media, targets, &cancelled)?;
    if target_indices.len() >= media.pages.len() {
        return Err(PageRemovalError::Stale("removal would delete every page".into()));
    }

    let plan = removal_plan(&mut archive, &package, media, &target_indices, &opf, &cancelled)?;
    let rewritten_opf = rewrite_opf(&opf, &target_indices, &plan.removed_manifest_ids, package.spine.len())?;
    let retained_original_indices = (0..media.pages.len())
        .filter(|index| !target_indices.contains(index))
        .collect::<Vec<_>>();
    let retained_pages = retained_original_indices
        .iter()
        .map(|index| media.pages[*index].clone())
        .collect::<Vec<_>>();
    let expected_files = expected_files(media, package.spine.len(), &target_indices, &plan.removed_paths)?;

    let mut output_guard = CreatedOutput::new(output);
    let mut rewritten = OpenOptions::new().write(true).create_new(true).open(output)?;
    output_guard.arm();
    write_archive(
        &mut archive,
        &mut rewritten,
        &package.path,
        &rewritten_opf,
        &plan.removed_paths,
        &cancelled,
    )?;
    rewritten.set_permissions(initial_metadata.permissions())?;
    rewritten.sync_all()?;

    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    let temporary_metadata = rewritten.metadata()?;
    let mut temporary_book = book.clone();
    temporary_book.url = Url::from_file_path(output)
        .map_err(|()| PageRemovalError::Unsupported("output path cannot be represented as a URL".into()))?;
    temporary_book.file_size = i64::try_from(temporary_metadata.len()).unwrap_or(i64::MAX);
    temporary_book.file_last_modified = scan_timestamp(&temporary_metadata);
    temporary_book.file_hash.clear();
    temporary_book.file_hash_koreader.clear();
    let mut created_media =
        analyze_book(&temporary_book, analyze_dimensions, &cancelled).map_err(|error| match error {
            MediaAnalysisError::Cancelled => PageRemovalError::Cancelled,
        })?;
    validate_created_media(&created_media, &retained_pages, &expected_files)?;
    for (page, previous) in created_media.pages.iter_mut().zip(&retained_pages) {
        page.file_hash.clone_from(&previous.file_hash);
    }

    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    let output_fingerprint = source_fingerprint(output, &cancelled)?;
    let current_metadata = fs::metadata(&path)?;
    ensure_same_metadata(&initial_metadata, &current_metadata, &path)?;
    if initial_fingerprint != source_fingerprint(&path, &cancelled)? {
        return Err(PageRemovalError::Stale("source bytes changed during rewrite".into()));
    }
    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    let final_metadata = fs::metadata(output)?;
    let mut updated_book = book.clone();
    updated_book.file_size = i64::try_from(final_metadata.len()).unwrap_or(i64::MAX);
    updated_book.file_last_modified = scan_timestamp(&final_metadata);
    updated_book.file_hash.clear();
    updated_book.file_hash_koreader.clear();
    let result = PageRemovalRewrite {
        book: updated_book,
        media: created_media,
        removed: targets.to_vec(),
        retained_original_indices,
        removed_first_page: targets.iter().any(|target| target.page_number == 1),
        source_fingerprint: initial_fingerprint,
        output_fingerprint,
    };
    output_guard.keep();
    Ok(result)
}

#[derive(Debug)]
struct RemovalPlan {
    removed_manifest_ids: HashSet<String>,
    removed_paths: HashSet<String>,
}

fn validate_archive(archive: &mut ZipArchive<File>, cancelled: &impl Fn() -> bool) -> Result<(), PageRemovalError> {
    if archive.len() > MAX_ENTRIES {
        return Err(PageRemovalError::Unsupported(format!(
            "EPUB has more than {MAX_ENTRIES} entries"
        )));
    }
    let mut raw_names = HashSet::with_capacity(archive.len());
    let mut lowercase_names = HashMap::<String, String>::with_capacity(archive.len());
    let mut declared_size = 0_u64;
    let mut has_encryption_manifest = false;
    for index in 0..archive.len() {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let entry = archive.by_index(index).map_err(archive_error)?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| PageRemovalError::Unsupported("EPUB entry name is not UTF-8".into()))?;
        validate_entry_name(name)?;
        if name.eq_ignore_ascii_case("META-INF/container.xml") && entry.size() > MAX_DOCUMENT_SIZE {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB container exceeds {MAX_DOCUMENT_SIZE} bytes"
            )));
        }
        has_encryption_manifest |= name.eq_ignore_ascii_case("META-INF/encryption.xml");
        if entry.encrypted() {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB entry {name:?} is encrypted"
            )));
        }
        if entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB entry {name:?} is a symbolic link"
            )));
        }
        if !raw_names.insert(entry.name_raw().to_vec()) {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB contains duplicate entry name {name:?}"
            )));
        }
        let lowercase = name.to_lowercase();
        if let Some(previous) = lowercase_names.insert(lowercase, name.to_owned())
            && previous != name
        {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB contains case-ambiguous entries {previous:?} and {name:?}"
            )));
        }
        declared_size = declared_size
            .checked_add(entry.size())
            .ok_or_else(|| PageRemovalError::Unsupported("EPUB declared size overflowed".into()))?;
        if declared_size > MAX_UNCOMPRESSED_SIZE {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB declared size exceeds {MAX_UNCOMPRESSED_SIZE} bytes"
            )));
        }
    }
    if has_encryption_manifest {
        return Err(PageRemovalError::Unsupported(
            "EPUB encryption and resource obfuscation are not supported".into(),
        ));
    }
    let first = archive.by_index(0).map_err(archive_error)?;
    if first.name() != "mimetype" || first.compression() != CompressionMethod::Stored {
        return Err(PageRemovalError::Unsupported(
            "EPUB mimetype must be the first uncompressed entry".into(),
        ));
    }
    drop(first);
    let mimetype = read_document(archive, "mimetype", cancelled)?;
    if mimetype != EPUB_MEDIA_TYPE.as_bytes() {
        return Err(PageRemovalError::Unsupported("EPUB mimetype is invalid".into()));
    }
    Ok(())
}

fn validate_package(package: &EpubPackage) -> Result<(), PageRemovalError> {
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for item in &package.manifest {
        if !ids.insert(item.id.as_str()) {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB manifest contains duplicate id {:?}",
                item.id
            )));
        }
        if !paths.insert(item.path.as_str()) {
            return Err(PageRemovalError::Unsupported(format!(
                "EPUB manifest contains duplicate path {:?}",
                item.path
            )));
        }
    }
    if package.manifest.iter().any(|item| {
        item.properties.contains("scripted")
            || matches!(
                item.media_type.as_str(),
                "application/javascript" | "text/javascript" | "application/ecmascript" | "text/ecmascript"
            )
    }) {
        return Err(PageRemovalError::Unsupported(
            "scripted EPUB page removal is not supported".into(),
        ));
    }
    Ok(())
}

fn validate_targets(
    archive: &mut ZipArchive<File>,
    media: &Media,
    targets: &[HashedPageTarget],
    cancelled: &impl Fn() -> bool,
) -> Result<HashSet<usize>, PageRemovalError> {
    let mut indices = HashSet::new();
    for target in targets {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let index = target
            .page_number
            .checked_sub(1)
            .and_then(|number| usize::try_from(number).ok())
            .ok_or_else(|| PageRemovalError::Stale("page number is not positive".into()))?;
        if !indices.insert(index) {
            return Err(PageRemovalError::Stale(
                "the same page was requested more than once".into(),
            ));
        }
        let page = media
            .pages
            .get(index)
            .ok_or_else(|| PageRemovalError::Stale("page number is outside current media".into()))?;
        if page.file_name != target.file_name
            || page.media_type != target.media_type
            || page.file_hash != target.file_hash
            || page.file_size != Some(target.file_size)
            || target.file_hash.is_empty()
        {
            return Err(PageRemovalError::Stale(format!(
                "page {} no longer matches its task payload",
                target.page_number
            )));
        }
        let bytes = package::read_entry(archive, &target.file_name).map_err(PageRemovalError::Archive)?;
        if i64::try_from(bytes.len()).ok() != Some(target.file_size)
            || hash_page(&bytes, &target.media_type)?.hash != target.file_hash
        {
            return Err(PageRemovalError::Stale(format!(
                "page {} bytes changed after analysis",
                target.page_number
            )));
        }
    }
    Ok(indices)
}

fn removal_plan(
    archive: &mut ZipArchive<File>,
    package: &EpubPackage,
    media: &Media,
    target_indices: &HashSet<usize>,
    opf: &[u8],
    cancelled: &impl Fn() -> bool,
) -> Result<RemovalPlan, PageRemovalError> {
    let mut spine_uses = HashMap::<&str, Vec<usize>>::new();
    for (index, item) in package.spine.iter().enumerate() {
        spine_uses.entry(&item.id).or_default().push(index);
    }
    let content_items = package
        .spine
        .iter()
        .filter(|item| {
            spine_uses
                .get(item.id.as_str())
                .is_some_and(|uses| uses.iter().all(|index| target_indices.contains(index)))
        })
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    let candidate_content_paths = content_items
        .values()
        .map(|item| item.path.as_str())
        .collect::<HashSet<_>>();
    if let Some(cover) = &package.cover
        && content_items.contains_key(cover.id.as_str())
    {
        return Err(PageRemovalError::Unsupported(
            "a direct-image cover cannot be removed safely".into(),
        ));
    }
    if package
        .nav
        .as_ref()
        .into_iter()
        .chain(package.ncx.as_ref())
        .any(|item| content_items.contains_key(item.id.as_str()))
    {
        return Err(PageRemovalError::Unsupported(
            "a navigation document cannot be removed as a page".into(),
        ));
    }

    let opf_text = std::str::from_utf8(opf)
        .map_err(|error| PageRemovalError::Unsupported(format!("EPUB package is not UTF-8: {error}")))?;
    let package_references = opf_references(opf_text, package::parent(&package.path), &content_items, target_indices)?;
    if let Some(path) = package_references
        .paths
        .iter()
        .find(|path| candidate_content_paths.contains(path.as_str()))
    {
        return Err(PageRemovalError::Unsupported(format!(
            "target spine resource {path:?} is referenced by the package"
        )));
    }
    if let Some(id) = package_references
        .ids
        .iter()
        .find(|id| content_items.contains_key(id.as_str()))
    {
        return Err(PageRemovalError::Unsupported(format!(
            "target spine item {id:?} is referenced by the package"
        )));
    }

    let mut referenced_paths = package_references.paths;
    for item in &package.manifest {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        if candidate_content_paths.contains(item.path.as_str()) || !is_xml_resource(item) {
            continue;
        }
        let bytes = read_document(archive, &item.path, cancelled)?;
        for path in resource_references(&bytes, package::parent(&item.path))? {
            if candidate_content_paths.contains(path.as_str()) {
                return Err(PageRemovalError::Unsupported(format!(
                    "target spine resource {path:?} is referenced by {:?}",
                    item.path
                )));
            }
            referenced_paths.insert(path);
        }
    }

    let page_uses =
        media
            .pages
            .iter()
            .enumerate()
            .fold(HashMap::<&str, Vec<usize>>::new(), |mut uses, (index, page)| {
                uses.entry(&page.file_name).or_default().push(index);
                uses
            });
    let css_exists = package.manifest.iter().any(|item| item.media_type == "text/css");
    let mut image_items = HashMap::<&str, &ManifestItem>::new();
    for (path, uses) in page_uses {
        if uses.iter().all(|index| target_indices.contains(index)) {
            let item =
                package.manifest.iter().find(|item| item.path == path).ok_or_else(|| {
                    PageRemovalError::Unsupported(format!("page image {path:?} is not in the manifest"))
                })?;
            let is_cover = package.cover.as_ref().is_some_and(|cover| cover.id == item.id);
            if !is_cover && !css_exists && !referenced_paths.contains(path) {
                image_items.insert(&item.id, item);
            }
        }
    }

    let mut all_candidates = content_items.clone();
    all_candidates.extend(image_items.iter().map(|(id, item)| (*id, *item)));
    let all_opf_references = opf_references(
        opf_text,
        package::parent(&package.path),
        &all_candidates,
        target_indices,
    )?;
    for id in &all_opf_references.ids {
        image_items.remove(id.as_str());
    }
    let mut removed_manifest_ids = content_items.keys().map(|id| (*id).to_owned()).collect::<HashSet<_>>();
    removed_manifest_ids.extend(image_items.keys().map(|id| (*id).to_owned()));
    let mut removed_paths = content_items
        .values()
        .map(|item| item.path.clone())
        .collect::<HashSet<_>>();
    removed_paths.extend(image_items.values().map(|item| item.path.clone()));
    Ok(RemovalPlan {
        removed_manifest_ids,
        removed_paths,
    })
}

#[derive(Default)]
struct OpfReferences {
    paths: HashSet<String>,
    ids: HashSet<String>,
}

fn opf_references(
    opf: &str,
    base_dir: &str,
    removed_items: &HashMap<&str, &ManifestItem>,
    target_indices: &HashSet<usize>,
) -> Result<OpfReferences, PageRemovalError> {
    let document = Document::parse(opf)
        .map_err(|error| PageRemovalError::Unsupported(format!("could not parse EPUB package: {error}")))?;
    let mut references = OpfReferences::default();
    let mut itemref_index = 0_usize;
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        let name = node.tag_name().name();
        let is_manifest_item = name.eq_ignore_ascii_case("item");
        let own_id = is_manifest_item.then(|| node.attribute("id")).flatten();
        if own_id.is_some_and(|id| removed_items.contains_key(id)) {
            continue;
        }
        if name.eq_ignore_ascii_case("itemref") {
            let targeted = target_indices.contains(&itemref_index);
            itemref_index += 1;
            if targeted {
                continue;
            }
        }
        for attribute in node.attributes() {
            let attribute_name = local_bytes_name(attribute.name().as_bytes());
            if !is_manifest_item
                && matches!(attribute_name, b"href" | b"src")
                && let Ok(path) = package::normalize_href(base_dir, attribute.value())
            {
                references.paths.insert(package::remove_fragment(&path).to_owned());
            }
            let value = attribute.value().trim();
            for id in removed_items.keys() {
                if value == *id || value == format!("#{id}") || value.split_whitespace().any(|token| token == *id) {
                    references.ids.insert((*id).to_owned());
                }
            }
        }
    }
    Ok(references)
}

fn resource_references(bytes: &[u8], base_dir: &str) -> Result<HashSet<String>, PageRemovalError> {
    let mut reader = Reader::from_reader(bytes);
    let mut references = HashSet::new();
    let mut buffer = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buffer).map_err(|error| {
            PageRemovalError::Unsupported(format!("could not parse EPUB resource references: {error}"))
        })?;
        match event {
            Event::Start(start) | Event::Empty(start) => {
                collect_element_references(&reader, &start, base_dir, &mut references)?;
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(references)
}

fn collect_element_references(
    reader: &Reader<&[u8]>,
    start: &BytesStart<'_>,
    base_dir: &str,
    references: &mut HashSet<String>,
) -> Result<(), PageRemovalError> {
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute
            .map_err(|error| PageRemovalError::Unsupported(format!("invalid EPUB resource attribute: {error}")))?;
        if !matches!(
            local_bytes_name(attribute.key.as_ref()),
            b"href" | b"src" | b"poster" | b"data"
        ) {
            continue;
        }
        let value = attribute
            .decode_and_unescape_value(reader.decoder())
            .map_err(|error| PageRemovalError::Unsupported(format!("invalid EPUB resource URL: {error}")))?;
        if value.contains("://") || value.starts_with("data:") || value.starts_with("mailto:") {
            continue;
        }
        let path = package::normalize_href(base_dir, &value).map_err(PageRemovalError::Unsupported)?;
        references.insert(package::remove_fragment(&path).to_owned());
    }
    Ok(())
}

fn rewrite_opf(
    opf: &[u8],
    target_indices: &HashSet<usize>,
    removed_manifest_ids: &HashSet<String>,
    expected_spine_items: usize,
) -> Result<Vec<u8>, PageRemovalError> {
    let mut reader = Reader::from_reader(opf);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::with_capacity(opf.len()));
    let mut buffer = Vec::new();
    let mut manifest_depth = 0_usize;
    let mut spine_depth = 0_usize;
    let mut skip_depth = 0_usize;
    let mut itemref_index = 0_usize;
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|error| PageRemovalError::Unsupported(format!("could not parse EPUB package: {error}")))?;
        match event {
            Event::Start(_) if skip_depth > 0 => skip_depth += 1,
            Event::End(_) if skip_depth > 0 => skip_depth -= 1,
            Event::Start(start)
                if should_remove_opf_element(
                    &reader,
                    &start,
                    manifest_depth,
                    spine_depth,
                    target_indices,
                    removed_manifest_ids,
                    &mut itemref_index,
                )? =>
            {
                skip_depth = 1;
            }
            Event::Empty(start)
                if should_remove_opf_element(
                    &reader,
                    &start,
                    manifest_depth,
                    spine_depth,
                    target_indices,
                    removed_manifest_ids,
                    &mut itemref_index,
                )? => {}
            Event::Start(start) => {
                let is_manifest = local_bytes_name(start.name().as_ref()) == b"manifest";
                let is_spine = local_bytes_name(start.name().as_ref()) == b"spine";
                writer.write_event(Event::Start(start)).map_err(PageRemovalError::Io)?;
                if is_manifest {
                    manifest_depth += 1;
                } else if is_spine {
                    spine_depth += 1;
                }
            }
            Event::End(end) => {
                let is_manifest = local_bytes_name(end.name().as_ref()) == b"manifest";
                let is_spine = local_bytes_name(end.name().as_ref()) == b"spine";
                if is_manifest {
                    manifest_depth = manifest_depth.saturating_sub(1);
                } else if is_spine {
                    spine_depth = spine_depth.saturating_sub(1);
                }
                writer.write_event(Event::End(end)).map_err(PageRemovalError::Io)?;
            }
            Event::Eof => break,
            event => writer.write_event(event).map_err(PageRemovalError::Io)?,
        }
        buffer.clear();
    }
    if itemref_index != expected_spine_items {
        return Err(PageRemovalError::Unsupported(
            "EPUB spine changed while preparing removal".into(),
        ));
    }
    Ok(writer.into_inner())
}

#[allow(clippy::too_many_arguments)]
fn should_remove_opf_element(
    reader: &Reader<&[u8]>,
    start: &BytesStart<'_>,
    manifest_depth: usize,
    spine_depth: usize,
    target_indices: &HashSet<usize>,
    removed_manifest_ids: &HashSet<String>,
    itemref_index: &mut usize,
) -> Result<bool, PageRemovalError> {
    let binding = start.name();
    let name = local_bytes_name(binding.as_ref());
    if manifest_depth > 0 && name == b"item" {
        return Ok(attribute_value(reader, start, b"id")?.is_some_and(|id| removed_manifest_ids.contains(&id)));
    }
    if spine_depth > 0 && name == b"itemref" {
        let remove = target_indices.contains(itemref_index);
        *itemref_index += 1;
        return Ok(remove);
    }
    Ok(false)
}

fn attribute_value(
    reader: &Reader<&[u8]>,
    start: &BytesStart<'_>,
    name: &[u8],
) -> Result<Option<String>, PageRemovalError> {
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute
            .map_err(|error| PageRemovalError::Unsupported(format!("invalid EPUB package attribute: {error}")))?;
        if local_bytes_name(attribute.key.as_ref()) == name {
            return attribute
                .decode_and_unescape_value(reader.decoder())
                .map(|value| Some(value.into_owned()))
                .map_err(|error| PageRemovalError::Unsupported(format!("invalid EPUB package attribute: {error}")));
        }
    }
    Ok(None)
}

fn write_archive(
    archive: &mut ZipArchive<File>,
    output: &mut File,
    opf_path: &str,
    opf: &[u8],
    removed_paths: &HashSet<String>,
    cancelled: &impl Fn() -> bool,
) -> Result<(), PageRemovalError> {
    let comment = archive.comment().to_vec();
    let mut writer = ZipWriter::new(output);
    writer
        .set_raw_comment(comment.into_boxed_slice())
        .map_err(archive_error)?;
    for index in 0..archive.len() {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let entry = archive.by_index(index).map_err(archive_error)?;
        if removed_paths.contains(entry.name()) {
            continue;
        }
        if entry.name() == opf_path {
            let name = entry.name().to_owned();
            let mut options = entry
                .options()
                .into_full_options()
                .large_file(opf.len() > u32::MAX as usize);
            if !entry.comment().is_empty() {
                options = options.with_file_comment(entry.comment());
            }
            writer.start_file(name, options).map_err(archive_error)?;
            writer.write_all(opf)?;
        } else if entry.is_dir() {
            let name = entry.name().to_owned();
            let mut options = entry
                .options()
                .into_full_options()
                .unix_permissions(entry.unix_mode().unwrap_or(0o755));
            if !entry.comment().is_empty() {
                options = options.with_file_comment(entry.comment());
            }
            writer.add_directory(name, options).map_err(archive_error)?;
        } else {
            writer.raw_copy_file(entry).map_err(archive_error)?;
        }
    }
    writer.finish().map_err(archive_error)?;
    Ok(())
}

fn expected_files(
    media: &Media,
    spine_count: usize,
    target_indices: &HashSet<usize>,
    removed_paths: &HashSet<String>,
) -> Result<Vec<komga_core::MediaFile>, PageRemovalError> {
    if media.files.len() < spine_count {
        return Err(PageRemovalError::Unsupported(
            "EPUB media files do not contain the complete spine".into(),
        ));
    }
    Ok(media.files[..spine_count]
        .iter()
        .enumerate()
        .filter(|(index, _)| !target_indices.contains(index))
        .map(|(_, file)| file.clone())
        .chain(
            media.files[spine_count..]
                .iter()
                .filter(|file| !removed_paths.contains(&file.file_name))
                .cloned(),
        )
        .collect())
}

fn validate_created_media(
    created: &Media,
    retained_pages: &[komga_core::BookPage],
    expected_files: &[komga_core::MediaFile],
) -> Result<(), PageRemovalError> {
    if created.status != MediaStatus::Ready
        || created.media_type.as_deref() != Some(EPUB_MEDIA_TYPE)
        || !created.epub_divina_compatible
    {
        return Err(PageRemovalError::Validation(
            "temporary publication is not a ready Divina-compatible EPUB".into(),
        ));
    }
    if created.pages.len() != retained_pages.len()
        || !created.pages.iter().zip(retained_pages).all(|(left, right)| {
            left.file_name == right.file_name
                && left.media_type == right.media_type
                && left.file_size == right.file_size
                && left.dimension == right.dimension
        })
    {
        return Err(PageRemovalError::Validation(
            "temporary EPUB does not contain exactly the retained pages".into(),
        ));
    }
    if created.files.len() != expected_files.len()
        || !created.files.iter().zip(expected_files).all(|(left, right)| {
            left.file_name == right.file_name
                && left.media_type == right.media_type
                && left.file_size == right.file_size
                && left.sub_type == right.sub_type
        })
    {
        return Err(PageRemovalError::Validation(
            "temporary EPUB does not contain exactly the expected resources".into(),
        ));
    }
    Ok(())
}

fn read_document(
    archive: &mut ZipArchive<File>,
    name: &str,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<u8>, PageRemovalError> {
    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    let mut entry = archive.by_name(name).map_err(archive_error)?;
    if entry.size() > MAX_DOCUMENT_SIZE {
        return Err(PageRemovalError::Unsupported(format!(
            "EPUB document {name:?} exceeds {MAX_DOCUMENT_SIZE} bytes"
        )));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry.read_to_end(&mut bytes)?;
    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    Ok(bytes)
}

fn is_xml_resource(item: &ManifestItem) -> bool {
    item.media_type == "text/html" || item.media_type.ends_with("+xml") || item.media_type.ends_with("/xml")
}

fn validate_entry_name(name: &str) -> Result<(), PageRemovalError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\\')
        || name.contains('\0')
        || name.split('/').any(|component| component == "..")
    {
        return Err(PageRemovalError::Unsupported(format!(
            "EPUB contains unsafe entry name {name:?}"
        )));
    }
    Ok(())
}

fn local_bytes_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn archive_error(error: zip::result::ZipError) -> PageRemovalError {
    PageRemovalError::Archive(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Cursor, Write},
    };

    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use komga_core::new_id;
    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::extract_page;

    #[test]
    fn removes_unreferenced_spine_document_and_exclusive_image() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("book.epub");
        let output = temporary.path().join("book.tmp");
        write_epub(&source, false);
        let (book, media) = hashed_media(&source);
        let before = fs::read(&source).unwrap();

        let result = rewrite_hashed_epub_pages(&book, &media, &[target(&media, 2)], &output, true, || false).unwrap();

        assert_eq!(fs::read(&source).unwrap(), before);
        assert_eq!(result.retained_original_indices, [0, 2]);
        assert_eq!(result.media.pages.len(), 2);
        assert_eq!(result.media.pages[0].file_name, "images/1.png");
        assert_eq!(result.media.pages[1].file_name, "images/3.png");
        assert!(result.media.pages.iter().all(|page| !page.file_hash.is_empty()));
        let mut archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
        let first = archive.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype");
        assert_eq!(first.compression(), CompressionMethod::Stored);
        drop(first);
        assert!(archive.by_name("pages/2.xhtml").is_err());
        assert!(archive.by_name("images/2.png").is_err());
        let opf = package::read_entry_text(&mut archive, "content.opf").unwrap();
        assert!(!opf.contains("idref=\"p2\""));
        assert!(!opf.contains("id=\"p2\""));
        assert!(!opf.contains("id=\"i2\""));
        assert!(opf.contains("idref=\"p1\"") && opf.contains("idref=\"p3\""));
    }

    #[test]
    fn referenced_stale_all_page_and_cancelled_removals_leave_no_output() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("book.epub");
        write_epub(&source, true);
        let (book, media) = hashed_media(&source);
        let before = fs::read(&source).unwrap();

        let referenced_output = temporary.path().join("referenced.tmp");
        assert!(matches!(
            rewrite_hashed_epub_pages(
                &book,
                &media,
                &[target(&media, 2)],
                &referenced_output,
                false,
                || false
            ),
            Err(PageRemovalError::Unsupported(message)) if message.contains("referenced")
        ));
        assert!(!referenced_output.exists());

        let mut stale = target(&media, 2);
        stale.file_hash.push('0');
        let stale_output = temporary.path().join("stale.tmp");
        assert!(matches!(
            rewrite_hashed_epub_pages(&book, &media, &[stale], &stale_output, false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        assert!(!stale_output.exists());

        let mut invalid_number = target(&media, 2);
        invalid_number.page_number = i32::MIN;
        let invalid_output = temporary.path().join("invalid-number.tmp");
        assert!(matches!(
            rewrite_hashed_epub_pages(&book, &media, &[invalid_number], &invalid_output, false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        assert!(!invalid_output.exists());

        let all_output = temporary.path().join("all.tmp");
        let all = (1..=3).map(|number| target(&media, number)).collect::<Vec<_>>();
        assert!(matches!(
            rewrite_hashed_epub_pages(&book, &media, &all, &all_output, false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        assert!(!all_output.exists());

        let cancelled_output = temporary.path().join("cancelled.tmp");
        assert!(matches!(
            rewrite_hashed_epub_pages(&book, &media, &[target(&media, 2)], &cancelled_output, false, || true),
            Err(PageRemovalError::Cancelled)
        ));
        assert!(!cancelled_output.exists());
        assert_eq!(fs::read(source).unwrap(), before);
    }

    fn write_epub(path: &Path, nav_references_second_page: bool) {
        let mut writer = ZipWriter::new(File::create(path).unwrap());
        writer
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(EPUB_MEDIA_TYPE.as_bytes()).unwrap();
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file("META-INF/container.xml", options).unwrap();
        writer
            .write_all(
                br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            )
            .unwrap();
        let nav_item = if nav_references_second_page {
            r#"<item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>"#
        } else {
            ""
        };
        let opf = format!(
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata><meta property="rendition:layout">pre-paginated</meta></metadata><manifest><item id="p1" href="pages/1.xhtml" media-type="application/xhtml+xml"/><item id="p2" href="pages/2.xhtml" media-type="application/xhtml+xml"/><item id="p3" href="pages/3.xhtml" media-type="application/xhtml+xml"/><item id="i1" href="images/1.png" media-type="image/png"/><item id="i2" href="images/2.png" media-type="image/png"/><item id="i3" href="images/3.png" media-type="image/png"/>{nav_item}</manifest><spine><itemref idref="p1"/><itemref idref="p2"/><itemref idref="p3"/></spine></package>"#
        );
        writer.start_file("content.opf", options).unwrap();
        writer.write_all(opf.as_bytes()).unwrap();
        for number in 1..=3 {
            writer.start_file(format!("pages/{number}.xhtml"), options).unwrap();
            writer
                .write_all(
                    format!(
                        r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>{number}</title></head><body><img src="../images/{number}.png"/></body></html>"#
                    )
                    .as_bytes(),
                )
                .unwrap();
            writer.start_file(format!("images/{number}.png"), options).unwrap();
            writer.write_all(&png(number)).unwrap();
        }
        if nav_references_second_page {
            writer.start_file("nav.xhtml", options).unwrap();
            writer
                .write_all(
                    br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="pages/2.xhtml">Two</a></li></ol></nav></body></html>"#,
                )
                .unwrap();
        }
        writer.finish().unwrap();
    }

    fn png(value: u8) -> Vec<u8> {
        let image = RgbImage::from_pixel(2, 2, Rgb([value, value.wrapping_add(1), value.wrapping_add(2)]));
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn hashed_media(path: &Path) -> (Book, Media) {
        let metadata = fs::metadata(path).unwrap();
        let book = Book {
            id: new_id(),
            name: "Book".into(),
            url: Url::from_file_path(path).unwrap(),
            file_last_modified: scan_timestamp(&metadata),
            file_size: i64::try_from(metadata.len()).unwrap(),
            file_hash: "book-hash".into(),
            file_hash_koreader: "koreader-hash".into(),
            number: 1,
            series_id: "series".into(),
            library_id: "library".into(),
            deleted_date: None,
            oneshot: false,
            created_date: None,
            last_modified_date: None,
        };
        let mut media = analyze_book(&book, true, || false).unwrap();
        for index in 0..media.pages.len() {
            let content = extract_page(&book, &media, i32::try_from(index + 1).unwrap()).unwrap();
            media.pages[index].file_hash = hash_page(&content.bytes, &media.pages[index].media_type).unwrap().hash;
        }
        (book, media)
    }

    fn target(media: &Media, page_number: i32) -> HashedPageTarget {
        let page = &media.pages[usize::try_from(page_number - 1).unwrap()];
        HashedPageTarget {
            file_name: page.file_name.clone(),
            media_type: page.media_type.clone(),
            file_hash: page.file_hash.clone(),
            file_size: page.file_size.unwrap(),
            page_number,
        }
    }
}
