use std::{io, path::Path};

use hayro::{
    RenderCache, RenderSettings,
    hayro_interpret::InterpreterSettings,
    hayro_syntax::{Pdf, page::Page},
    render,
    vello_cpu::color::palette::css::WHITE,
};
use hayro_write::{ChunkSettings, ExtractionQuery};
use image::{RgbImage, codecs::jpeg::JpegEncoder};
use komga_core::{BookPage, Dimension, JPEG_MEDIA_TYPE, Media, MediaStatus, PDF_MEDIA_TYPE};
use pdf_writer::Ref;

use crate::{MediaOperationError, TypedBytes};

const RENDER_SHORT_EDGE: f32 = 3200.0;
const JPEG_QUALITY: u8 = 75;

#[derive(Debug)]
pub(crate) enum AnalysisError {
    Cancelled,
    Io(io::ErrorKind),
    Invalid,
}

pub(crate) fn analyze(
    path: &Path,
    book_id: &str,
    analyze_dimensions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Media, AnalysisError> {
    let bytes = std::fs::read(path).map_err(|error| AnalysisError::Io(error.kind()))?;
    if cancelled() {
        return Err(AnalysisError::Cancelled);
    }
    let pdf = Pdf::new(bytes).map_err(|_| AnalysisError::Invalid)?;
    let mut pages = Vec::with_capacity(pdf.pages().len());
    for (index, page) in pdf.pages().iter().enumerate() {
        if cancelled() {
            return Err(AnalysisError::Cancelled);
        }
        pages.push(BookPage {
            file_name: (index + 1).to_string(),
            media_type: String::new(),
            dimension: if analyze_dimensions {
                dimension_from_page(page)
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
        extension: None,
        book_id: book_id.to_owned(),
        epub_divina_compatible: false,
        epub_is_kepub: false,
        created_date: None,
        last_modified_date: None,
    })
}

pub(crate) fn render_page(path: &Path, index: usize) -> Result<TypedBytes, MediaOperationError> {
    let pdf = load(path)?;
    let page = pdf.pages().get(index).ok_or(MediaOperationError::PageOutOfBounds)?;
    let scale = render_scale(page)
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
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
        .encode_image(&image)
        .map_err(|error| MediaOperationError::Image(error.to_string()))?;
    Ok(TypedBytes {
        bytes,
        media_type: JPEG_MEDIA_TYPE.to_owned(),
    })
}

pub(crate) fn extract_page(path: &Path, index: usize) -> Result<TypedBytes, MediaOperationError> {
    let pdf = load(path)?;
    let bytes = write_pages(&pdf, &[index])?;
    Ok(TypedBytes {
        bytes,
        media_type: PDF_MEDIA_TYPE.to_owned(),
    })
}

pub(crate) fn output_dimension(dimension: Dimension) -> Option<Dimension> {
    let width = dimension.width as f32;
    let height = dimension.height as f32;
    let scale = scale(width, height, width, height)?;
    Some(Dimension {
        width: rounded_i32(f64::from(width * scale))?,
        height: rounded_i32(f64::from(height * scale))?,
    })
}

fn load(path: &Path) -> Result<Pdf, MediaOperationError> {
    let bytes = std::fs::read(path).map_err(|error| MediaOperationError::Read(error.to_string()))?;
    Pdf::new(bytes).map_err(|error| MediaOperationError::Read(format!("invalid PDF: {error:?}")))
}

fn dimension_from_page(page: &Page<'_>) -> Option<Dimension> {
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

fn render_scale(page: &Page<'_>) -> Option<f32> {
    let crop_box = page.crop_box();
    let crop_width = crop_box.width() as f32;
    let crop_height = crop_box.height() as f32;
    let (render_width, render_height) = page.render_dimensions();
    scale(crop_width, crop_height, render_width, render_height)
}

fn scale(crop_width: f32, crop_height: f32, render_width: f32, render_height: f32) -> Option<f32> {
    let values = [crop_width, crop_height, render_width, render_height];
    if values.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        return None;
    }
    let requested = RENDER_SHORT_EDGE / crop_width.min(crop_height);
    let viewport_limit = f32::from(u16::MAX) / render_width.max(render_height);
    Some(requested.min(viewport_limit))
}

fn write_pages(pdf: &Pdf, page_indices: &[usize]) -> Result<Vec<u8>, MediaOperationError> {
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
