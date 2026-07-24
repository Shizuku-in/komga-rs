use std::io::Cursor;

use image::{DynamicImage, GenericImageView, ImageFormat, ImageReader, Rgb, RgbImage, imageops::FilterType};
use komga_core::Dimension;

use crate::{ImageOutput, MediaOperationError, TypedBytes};

pub(crate) fn collage(sources: &[Vec<u8>], size: u32) -> Result<Vec<u8>, MediaOperationError> {
    let width = size.max(2);
    let height = width.saturating_mul(3) / 2;
    let cell_w = width / 2;
    let cell_h = height / 2;
    let mut canvas = RgbImage::new(width, height);
    for slot in 0..if sources.is_empty() { 0 } else { 4usize } {
        let reader = ImageReader::new(Cursor::new(&sources[slot % sources.len()]))
            .with_guessed_format()
            .map_err(|e| MediaOperationError::Image(e.to_string()))?;
        let tile = reader
            .decode()
            .map_err(|e| MediaOperationError::Image(e.to_string()))?
            .resize_to_fill(cell_w, cell_h, FilterType::Lanczos3)
            .to_rgb8();
        image::imageops::replace(
            &mut canvas,
            &tile,
            i64::from((slot as u32 % 2) * cell_w),
            i64::from((slot as u32 / 2) * cell_h),
        );
    }
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(canvas)
        .write_to(&mut output, ImageFormat::Jpeg)
        .map_err(|e| MediaOperationError::Image(e.to_string()))?;
    Ok(output.into_inner())
}

pub(crate) fn convert(
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

pub(crate) fn dimension(bytes: &[u8]) -> Option<Dimension> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(Dimension {
        width: i32::try_from(width).ok()?,
        height: i32::try_from(height).ok()?,
    })
}

pub(crate) fn can_decode_media_type(media_type: &str) -> bool {
    let media_type = media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase();
    matches!(
        media_type.as_str(),
        "image/jpeg"
            | "image/png"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/tiff"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
    )
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
