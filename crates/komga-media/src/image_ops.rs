use std::{io::Cursor, sync::Once};

#[cfg(feature = "heif-native")]
use std::sync::OnceLock;

use hayro_jpeg2000::integration::Jp2Decoder;
use image::{
    DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader, Rgb, RgbImage, codecs::tga::TgaDecoder,
    imageops::FilterType,
};
use jxl_oxide::integration::JxlDecoder;
use komga_core::Dimension;

use crate::{ImageOutput, MediaOperationError, TypedBytes};

const JXL_CODESTREAM_MAGIC: &[u8] = &[0xff, 0x0a];
const JXL_CONTAINER_MAGIC: &[u8] = b"\0\0\0\x0cJXL \r\n\x87\n";
const JP2_MAGIC: &[u8] = b"\0\0\0\x0cjP  \r\n\x87\n";
const J2C_MAGIC: &[u8] = &[0xff, 0x4f, 0xff, 0x51];
const TGA_FOOTER: &[u8] = b"TRUEVISION-XFILE.\0";

pub(crate) fn collage(sources: &[Vec<u8>], size: u32) -> Result<Vec<u8>, MediaOperationError> {
    let height = size.max(2);
    let width = ((f64::from(height) * 0.706_666_666_7).round() as u32).max(2);
    let cell_w = width / 2;
    let cell_h = height / 2;
    let mut canvas = RgbImage::new(width, height);
    for (slot, source) in sources.iter().take(4).enumerate() {
        let tile = decode(source)
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
    let mut image = decode(source).map_err(|error| MediaOperationError::Image(error.to_string()))?;
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
    let (width, height) = dimensions(bytes)?;
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
    match media_type.as_str() {
        "image/heif" | "image/heic" | "image/heic-sequence" => return native_heif_capabilities().hevc,
        "image/avif" | "image/avif-sequence" => return native_heif_capabilities().av1,
        _ => {}
    }
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
            | "image/jxl"
            | "image/jp2"
            | "image/j2c"
            | "image/x-j2c"
            | "image/x-targa"
            | "image/x-tga"
            | "image/vnd-ms.dds"
            | "image/x-dds"
            | "image/vnd.radiance"
            | "image/x-hdr"
            | "image/x-exr"
            | "image/exr"
            | "image/x-portable-bitmap"
            | "image/x-portable-graymap"
            | "image/x-portable-pixmap"
            | "image/x-portable-anymap"
            | "image/x-farbfeld"
            | "image/x-qoi"
            | "image/qoi"
    )
}

pub(crate) fn detected_media_type(bytes: &[u8]) -> Option<&'static str> {
    if is_jxl(bytes) {
        return Some("image/jxl");
    }
    if bytes.starts_with(JP2_MAGIC) {
        return Some("image/jp2");
    }
    if bytes.starts_with(J2C_MAGIC) {
        return Some("image/j2c");
    }
    match image::guess_format(bytes) {
        Ok(ImageFormat::Farbfeld) => Some("image/x-farbfeld"),
        Ok(format) => Some(format.to_mime_type()),
        Err(_) if is_tga(bytes) => Some("image/x-targa"),
        Err(_) => None,
    }
}

pub(crate) fn decode(bytes: &[u8]) -> image::ImageResult<DynamicImage> {
    if is_jxl(bytes) {
        return DynamicImage::from_decoder(JxlDecoder::new(Cursor::new(bytes))?);
    }
    if bytes.starts_with(JP2_MAGIC) || bytes.starts_with(J2C_MAGIC) {
        return DynamicImage::from_decoder(Jp2Decoder::new(Cursor::new(bytes))?);
    }
    if is_native_heif(bytes) {
        ensure_heif_hooks();
        return ImageReader::new(Cursor::new(bytes)).with_guessed_format()?.decode();
    }
    if let Ok(format) = image::guess_format(bytes) {
        return ImageReader::with_format(Cursor::new(bytes), format).decode();
    }
    ImageReader::with_format(Cursor::new(bytes), ImageFormat::Tga).decode()
}

fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if is_jxl(bytes) {
        return JxlDecoder::new(Cursor::new(bytes))
            .ok()
            .map(|decoder| decoder.dimensions());
    }
    if bytes.starts_with(JP2_MAGIC) || bytes.starts_with(J2C_MAGIC) {
        return Jp2Decoder::new(Cursor::new(bytes))
            .ok()
            .map(|decoder| decoder.dimensions());
    }
    if is_native_heif(bytes) {
        ensure_heif_hooks();
        return ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok();
    }
    if let Ok(format) = image::guess_format(bytes) {
        return ImageReader::with_format(Cursor::new(bytes), format)
            .into_dimensions()
            .ok();
    }
    ImageReader::with_format(Cursor::new(bytes), ImageFormat::Tga)
        .into_dimensions()
        .ok()
}

#[derive(Clone, Copy, Default)]
struct NativeHeifCapabilities {
    hevc: bool,
    av1: bool,
}

#[cfg(feature = "heif-native")]
fn native_heif_capabilities() -> NativeHeifCapabilities {
    static CAPABILITIES: OnceLock<NativeHeifCapabilities> = OnceLock::new();
    *CAPABILITIES.get_or_init(|| {
        use libheif_rs::{CompressionFormat, LibHeif};

        let lib = LibHeif::new();
        NativeHeifCapabilities {
            hevc: !lib.decoder_descriptors(1, Some(CompressionFormat::Hevc)).is_empty(),
            av1: !lib.decoder_descriptors(1, Some(CompressionFormat::Av1)).is_empty(),
        }
    })
}

#[cfg(not(feature = "heif-native"))]
fn native_heif_capabilities() -> NativeHeifCapabilities {
    NativeHeifCapabilities::default()
}

fn ensure_heif_hooks() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        #[cfg(feature = "heif-native")]
        libheif_rs::integration::image::register_all_decoding_hooks();
    });
}

fn is_jxl(bytes: &[u8]) -> bool {
    bytes.starts_with(JXL_CODESTREAM_MAGIC) || bytes.starts_with(JXL_CONTAINER_MAGIC)
}

fn is_tga(bytes: &[u8]) -> bool {
    bytes.ends_with(TGA_FOOTER) || TgaDecoder::new(Cursor::new(bytes)).is_ok()
}

fn is_native_heif(bytes: &[u8]) -> bool {
    infer::get(bytes).is_some_and(|kind| matches!(kind.mime_type(), "image/heif" | "image/avif"))
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

#[cfg(test)]
mod tests {
    use super::*;

    const JXL_1X1_RED: &[u8] = &[
        0xff, 0x0a, 0x00, 0x10, 0x10, 0x09, 0x08, 0x00, 0x01, 0x00, 0x28, 0x00, 0x4b, 0x18, 0x8b, 0x15, 0xc2, 0x49,
        0x41, 0x4e, 0x7f, 0x00,
    ];
    const JP2_2X2_RED: &[u8] = &[
        0x00, 0x00, 0x00, 0x0c, 0x6a, 0x50, 0x20, 0x20, 0x0d, 0x0a, 0x87, 0x0a, 0x00, 0x00, 0x00, 0x14, 0x66, 0x74,
        0x79, 0x70, 0x6a, 0x70, 0x32, 0x20, 0x00, 0x00, 0x00, 0x00, 0x6a, 0x70, 0x32, 0x20, 0x00, 0x00, 0x00, 0x2d,
        0x6a, 0x70, 0x32, 0x68, 0x00, 0x00, 0x00, 0x16, 0x69, 0x68, 0x64, 0x72, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00,
        0x00, 0x02, 0x00, 0x03, 0x0f, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0f, 0x63, 0x6f, 0x6c, 0x72, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x99, 0x6a, 0x70, 0x32, 0x63, 0xff, 0x4f, 0xff, 0x51, 0x00,
        0x2f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x03, 0x0f, 0x01, 0x01, 0x0f, 0x01, 0x01, 0x0f, 0x01, 0x01, 0xff, 0x52, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x01,
        0x01, 0x01, 0x04, 0x04, 0x00, 0x01, 0xff, 0x5c, 0x00, 0x07, 0x40, 0x80, 0x88, 0x88, 0x90, 0xff, 0x64, 0x00,
        0x25, 0x00, 0x01, 0x43, 0x72, 0x65, 0x61, 0x74, 0x65, 0x64, 0x20, 0x62, 0x79, 0x20, 0x4f, 0x70, 0x65, 0x6e,
        0x4a, 0x50, 0x45, 0x47, 0x20, 0x76, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x20, 0x32, 0x2e, 0x35, 0x2e, 0x30,
        0xff, 0x90, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1e, 0x00, 0x01, 0xff, 0x93, 0xcf, 0xfc, 0x30, 0x08,
        0x07, 0xb7, 0x80, 0xdf, 0xf8, 0x90, 0x10, 0x01, 0x3f, 0x80, 0x80, 0x80, 0xff, 0xd9,
    ];

    #[test]
    fn collage_uses_configured_max_edge_without_repeating_sources() {
        let mut source = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(RgbImage::from_pixel(8, 8, Rgb([240, 16, 16])))
            .write_to(&mut source, ImageFormat::Png)
            .unwrap();

        let collage = collage(&[source.into_inner()], 300).unwrap();
        let collage = image::load_from_memory(&collage).unwrap().to_rgb8();
        assert_eq!(collage.dimensions(), (212, 300));
        assert!(collage.get_pixel(50, 75).0[0] > 200);
        assert!(collage.get_pixel(175, 75).0.iter().all(|channel| *channel < 10));
        assert!(collage.get_pixel(50, 225).0.iter().all(|channel| *channel < 10));
    }

    #[test]
    fn decodes_jpeg_xl_and_jpeg_2000() {
        for (bytes, media_type, expected_dimension) in
            [(JXL_1X1_RED, "image/jxl", (1, 1)), (JP2_2X2_RED, "image/jp2", (2, 2))]
        {
            assert_eq!(detected_media_type(bytes), Some(media_type));
            assert_eq!(dimensions(bytes), Some(expected_dimension));
            let output = convert(bytes, ImageOutput::Png, None).unwrap();
            assert_eq!(output.media_type, "image/png");
            assert_eq!(
                image::load_from_memory(&output.bytes).unwrap().dimensions(),
                expected_dimension
            );
        }
    }

    #[test]
    fn decodes_enabled_uncommon_image_formats() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([240, 32, 16])));
        for (format, media_type) in [
            (ImageFormat::Tga, "image/x-targa"),
            (ImageFormat::Hdr, "image/vnd.radiance"),
            (ImageFormat::OpenExr, "image/x-exr"),
            (ImageFormat::Pnm, "image/x-portable-anymap"),
            (ImageFormat::Farbfeld, "image/x-farbfeld"),
            (ImageFormat::Qoi, "image/x-qoi"),
        ] {
            let mut encoded = Cursor::new(Vec::new());
            let encoded_source = if matches!(format, ImageFormat::Hdr | ImageFormat::OpenExr) {
                DynamicImage::ImageRgb32F(source.to_rgb32f())
            } else if format == ImageFormat::Farbfeld {
                DynamicImage::ImageRgba16(source.to_rgba16())
            } else {
                source.clone()
            };
            encoded_source.write_to(&mut encoded, format).unwrap();
            let encoded = encoded.into_inner();
            assert_eq!(detected_media_type(&encoded), Some(media_type), "{format:?}");
            assert_eq!(dimensions(&encoded), Some((4, 4)), "{format:?}");
            assert_eq!(decode(&encoded).unwrap().dimensions(), (4, 4), "{format:?}");
        }

        let dds = dxt1_dds();
        assert_eq!(detected_media_type(&dds), Some("image/vnd-ms.dds"));
        assert_eq!(dimensions(&dds), Some((4, 4)));
        assert_eq!(decode(&dds).unwrap().dimensions(), (4, 4));
    }

    #[cfg(feature = "heif-native")]
    #[test]
    fn decodes_heic_and_avif_with_native_codecs() {
        let fixtures = [
            (
                hex_fixture(include_str!("../tests/fixtures/red.heic.hex")),
                "image/heif",
                (64, 48),
            ),
            (
                hex_fixture(include_str!("../tests/fixtures/blue.avif.hex")),
                "image/avif",
                (3, 2),
            ),
        ];
        for (source, media_type, expected_dimension) in fixtures {
            assert_eq!(crate::detect_image_media_type(&source).as_deref(), Some(media_type));
            assert_eq!(dimensions(&source), Some(expected_dimension));
            let output = convert(&source, ImageOutput::Png, None).unwrap();
            assert_eq!(output.media_type, "image/png");
            assert_eq!(
                image::load_from_memory(&output.bytes).unwrap().dimensions(),
                expected_dimension
            );
        }
    }

    #[cfg(feature = "heif-native")]
    fn hex_fixture(value: &str) -> Vec<u8> {
        value
            .split_ascii_whitespace()
            .flat_map(|line| line.as_bytes().chunks_exact(2))
            .map(|byte| u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap())
            .collect()
    }

    fn dxt1_dds() -> Vec<u8> {
        let mut bytes = b"DDS ".to_vec();
        for value in [124, 0x1007, 4, 4, 8, 0, 0] {
            bytes.extend_from_slice(&u32::to_le_bytes(value));
        }
        bytes.extend_from_slice(&[0; 44]);
        bytes.extend_from_slice(&u32::to_le_bytes(32));
        bytes.extend_from_slice(&u32::to_le_bytes(4));
        bytes.extend_from_slice(b"DXT1");
        bytes.extend_from_slice(&[0; 20]);
        bytes.extend_from_slice(&u32::to_le_bytes(0x1000));
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&[0x00, 0xf8, 0x00, 0xf8, 0, 0, 0, 0]);
        bytes
    }
}
