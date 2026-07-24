mod navigation;
mod package;
mod positions;

use std::{collections::BTreeSet, path::Path};

use komga_core::{
    BookPage, EPUB_ASSET_SUBTYPE, EPUB_MEDIA_TYPE, EPUB_PAGE_SUBTYPE, Media, MediaExtension, MediaExtensionEpub,
    MediaFile, MediaStatus,
};
use scraper::{Html, Selector};

use crate::{TypedBytes, image_dimension};

const DIVINA_LETTER_COUNT_THRESHOLD: usize = 15;

#[derive(Debug)]
pub(crate) enum AnalysisError {
    Cancelled,
    Invalid,
}

pub(crate) fn is_epub(path: &Path) -> bool {
    package::has_epub_mimetype(path)
}

pub(crate) fn analyze(
    path: &Path,
    book_id: &str,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Media, AnalysisError> {
    let (mut archive, package) = package::open(path).map_err(|_| AnalysisError::Invalid)?;
    let resources = resources(&mut archive, &package, &cancelled)?;
    let is_kepub = match positions::is_kepub(&mut archive, &resources, &cancelled) {
        Ok(value) => value,
        Err(positions::PositionError::Cancelled) => return Err(AnalysisError::Cancelled),
        Err(positions::PositionError::Invalid) => false,
    };
    let mut comments = Vec::new();

    let toc = navigation::toc(&mut archive, &package).unwrap_or_else(|_| {
        comments.push("ERR_1035".to_owned());
        Vec::new()
    });
    let landmarks = navigation::landmarks(&mut archive, &package).unwrap_or_else(|_| {
        comments.push("ERR_1036".to_owned());
        Vec::new()
    });
    let page_list = navigation::page_list(&mut archive, &package).unwrap_or_else(|_| {
        comments.push("ERR_1037".to_owned());
        Vec::new()
    });
    if cancelled() {
        return Err(AnalysisError::Cancelled);
    }

    let pages = match divina_pages(&mut archive, &package, analyze_dimensions, &cancelled) {
        Ok(pages) => pages,
        Err(AnalysisError::Cancelled) => return Err(AnalysisError::Cancelled),
        Err(AnalysisError::Invalid) => {
            comments.push("ERR_1038".to_owned());
            Vec::new()
        }
    };
    let fixed_layout = !pages.is_empty() || package.fixed_layout;
    let epub_positions = match positions::compute(&mut archive, &resources, fixed_layout, is_kepub, &cancelled) {
        Ok(positions) => positions,
        Err(positions::PositionError::Cancelled) => return Err(AnalysisError::Cancelled),
        Err(positions::PositionError::Invalid) => {
            comments.push("ERR_1039".to_owned());
            Vec::new()
        }
    };

    let missing = resources
        .iter()
        .filter(|resource| resource.file_size.is_none())
        .map(|resource| resource.file_name.as_str())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        comments.push(format!("ERR_1033 [{}]", missing.join(", ")));
    }

    let divina_compatible = !pages.is_empty();
    let page_count = if pages.is_empty() {
        approximate_page_count(&mut archive, &package)
    } else {
        i32::try_from(pages.len()).unwrap_or(i32::MAX)
    };
    Ok(Media {
        status: MediaStatus::Ready,
        media_type: Some(EPUB_MEDIA_TYPE.to_owned()),
        pages,
        page_count,
        files: resources,
        comment: (!comments.is_empty()).then(|| comments.join(" ")),
        extension: Some(MediaExtension::Epub(MediaExtensionEpub {
            toc,
            landmarks,
            page_list,
            is_fixed_layout: fixed_layout,
            positions: epub_positions,
        })),
        book_id: book_id.to_owned(),
        epub_divina_compatible: divina_compatible,
        epub_is_kepub: is_kepub,
        created_date: None,
        last_modified_date: None,
    })
}

pub(crate) fn cover(path: &Path) -> Result<Option<TypedBytes>, String> {
    let (mut archive, package) = package::open(path)?;
    let Some(cover) = package.cover else {
        return Ok(None);
    };
    let bytes = package::read_entry(&mut archive, &cover.path)?;
    Ok(Some(TypedBytes {
        bytes,
        media_type: cover.media_type,
    }))
}

pub(crate) fn extract(path: &Path, entry_name: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
    package::read_entry(&mut archive, entry_name)
}

fn resources(
    archive: &mut zip::ZipArchive<std::fs::File>,
    package: &package::EpubPackage,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<MediaFile>, AnalysisError> {
    let spine_ids = package
        .spine
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    package
        .spine
        .iter()
        .map(|item| (item, EPUB_PAGE_SUBTYPE))
        .chain(
            package
                .manifest
                .iter()
                .filter(|item| !spine_ids.contains(item.id.as_str()))
                .map(|item| (item, EPUB_ASSET_SUBTYPE)),
        )
        .map(|(item, sub_type)| {
            if cancelled() {
                return Err(AnalysisError::Cancelled);
            }
            Ok(MediaFile {
                file_name: item.path.clone(),
                media_type: Some(item.media_type.clone()),
                sub_type: Some(sub_type.to_owned()),
                file_size: package::entry_sizes(archive, &item.path).map(|(size, _)| size),
            })
        })
        .collect()
}

fn approximate_page_count(archive: &mut zip::ZipArchive<std::fs::File>, package: &package::EpubPackage) -> i32 {
    package
        .spine
        .iter()
        .filter_map(|item| package::entry_sizes(archive, &item.path).map(|(_, compressed)| compressed))
        .map(|size| size.saturating_add(1023) / 1024)
        .fold(0_u64, u64::saturating_add)
        .try_into()
        .unwrap_or(i32::MAX)
}

fn divina_pages(
    archive: &mut zip::ZipArchive<std::fs::File>,
    package: &package::EpubPackage,
    analyze_dimensions: bool,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<BookPage>, AnalysisError> {
    let body_selector = Selector::parse("body").map_err(|_| AnalysisError::Invalid)?;
    let image_selector = Selector::parse("img[src], svg image").map_err(|_| AnalysisError::Invalid)?;
    let page_count = package
        .spine
        .iter()
        .filter(|item| package::entry_sizes(archive, &item.path).is_some())
        .count();
    let mut images_by_page = Vec::with_capacity(package.spine.len());
    for item in &package.spine {
        if cancelled() {
            return Err(AnalysisError::Cancelled);
        }
        if item.media_type.to_ascii_lowercase().starts_with("image/") {
            images_by_page.push(vec![item.path.clone()]);
            continue;
        }
        let Ok(content) = package::read_entry_text(archive, &item.path) else {
            images_by_page.push(Vec::new());
            continue;
        };
        let document = Html::parse_document(&content);
        let body_text = document
            .select(&body_selector)
            .flat_map(|body| body.text())
            .flat_map(str::split_whitespace)
            .collect::<Vec<_>>()
            .join(" ");
        let letter_count = body_text.chars().count();
        if letter_count > DIVINA_LETTER_COUNT_THRESHOLD {
            return Ok(Vec::new());
        }
        let mut images = Vec::new();
        for image in document.select(&image_selector) {
            let Some(href) = image
                .value()
                .attr("src")
                .or_else(|| image.value().attr("xlink:href"))
                .or_else(|| image.value().attr("href"))
                .or_else(|| {
                    image
                        .value()
                        .attrs()
                        .find_map(|(name, value)| (name == "href").then_some(value))
                })
            else {
                continue;
            };
            let Ok(path) = package::normalize_href(package::parent(&item.path), href) else {
                continue;
            };
            let path = package::remove_fragment(&path).to_owned();
            if !images.contains(&path) {
                images.push(path);
            }
        }
        images_by_page.push(images);
    }
    if images_by_page.len() != page_count || images_by_page.iter().any(|images| images.len() != 1) {
        return Ok(Vec::new());
    }

    let mut pages = Vec::with_capacity(page_count);
    for image_path in images_by_page.into_iter().flatten() {
        let Some(item) = package.manifest.iter().find(|item| item.path == image_path) else {
            return Ok(Vec::new());
        };
        if !item.media_type.to_ascii_lowercase().starts_with("image/") {
            return Ok(Vec::new());
        }
        let Some((file_size, _)) = package::entry_sizes(archive, &image_path) else {
            return Ok(Vec::new());
        };
        let dimension = if analyze_dimensions {
            package::read_entry(archive, &image_path)
                .ok()
                .and_then(|bytes| image_dimension(&bytes))
        } else {
            None
        };
        pages.push(BookPage {
            file_name: image_path,
            media_type: item.media_type.clone(),
            dimension,
            file_hash: String::new(),
            file_size: Some(file_size),
        });
    }
    Ok(pages)
}
