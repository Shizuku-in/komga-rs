//! ISBN discovery from page barcodes.

use std::collections::HashSet;

use komga_core::{Book, Media, MediaProfile};
use rxing::{
    BarcodeFormat, BinaryBitmap, DecodeHintValue, DecodeHints, Luma8LuminanceSource, MultiFormatReader, Reader,
    common::HybridBinarizer,
};

use crate::{extract_page, image_ops};

const FIRST_PAGES: i32 = 3;
const LAST_PAGES: i32 = 3;

/// Searches the last three and first three image pages for an ISBN-13 barcode.
///
/// Individual page extraction, image decoding, and barcode decoding failures are
/// ignored so one malformed page does not prevent later candidates from being
/// checked. Reflowable and fixed-layout EPUBs are skipped to match Komga's
/// barcode provider.
pub fn extract_isbn_barcode(book: &Book, media: &Media) -> Option<String> {
    if media.profile() == Some(MediaProfile::Epub) {
        return None;
    }

    for page_number in candidate_pages(media.page_count) {
        let Ok(page) = extract_page(book, media, page_number) else {
            continue;
        };
        if let Some(isbn) = decode_isbn(&page.bytes) {
            return Some(isbn);
        }
    }
    None
}

fn candidate_pages(page_count: i32) -> Vec<i32> {
    if page_count <= 0 {
        return Vec::new();
    }
    let mut pages = ((page_count - LAST_PAGES + 1).max(1)..=page_count)
        .rev()
        .collect::<Vec<_>>();
    for page in 1..=page_count.min(FIRST_PAGES) {
        if !pages.contains(&page) {
            pages.push(page);
        }
    }
    pages
}

fn decode_isbn(bytes: &[u8]) -> Option<String> {
    let image = image_ops::decode(bytes).ok()?.to_luma8();
    let (width, height) = image.dimensions();
    let source = Luma8LuminanceSource::new(image.into_raw(), width, height).ok()?;
    let mut bitmap = BinaryBitmap::new(HybridBinarizer::new(source));
    let hints = DecodeHints::default()
        .with(DecodeHintValue::PossibleFormats(HashSet::from([BarcodeFormat::EAN_13])))
        .with(DecodeHintValue::TryHarder(true));
    let result = MultiFormatReader::default()
        .decode_with_hints(&mut bitmap, &hints)
        .ok()?;
    valid_isbn_13(result.getText()).then(|| result.getText().to_owned())
}

fn valid_isbn_13(value: &str) -> bool {
    value.len() == 13
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value.starts_with("978") || value.starts_with("979"))
        && value
            .bytes()
            .enumerate()
            .map(|(index, byte)| u32::from(byte - b'0') * if index % 2 == 0 { 1 } else { 3 })
            .sum::<u32>()
            % 10
            == 0
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write};

    use komga_core::{EPUB_MEDIA_TYPE, MediaStatus, scan_book_file};
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::analyze_book;

    const BARCODE_PAGE: &[u8] = include_bytes!("../../../komga/komga/src/test/resources/barcode/page_384.jpg");
    const PLAIN_PAGE: &[u8] = include_bytes!("../../../komga/komga/src/test/resources/barcode/komga.png");

    fn analyzed_archive(pages: &[(&str, &[u8])]) -> (tempfile::TempDir, Book, Media) {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        let mut archive = ZipWriter::new(File::create(&path).unwrap());
        for (name, bytes) in pages {
            archive.start_file(*name, SimpleFileOptions::default()).unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
        let book = scan_book_file(&path, "library", false).unwrap();
        let media = analyze_book(&book, false, || false).unwrap();
        (temporary, book, media)
    }

    #[test]
    fn extracts_isbn_from_kotlin_barcode_fixture() {
        let (_temporary, book, media) = analyzed_archive(&[("page.jpg", BARCODE_PAGE)]);

        assert_eq!(extract_isbn_barcode(&book, &media).as_deref(), Some("9782811632397"));
    }

    #[test]
    fn searches_last_pages_before_first_pages_and_ignores_invalid_images() {
        let (_temporary, book, media) = analyzed_archive(&[
            ("1.png", PLAIN_PAGE),
            ("2.png", PLAIN_PAGE),
            ("3.png", PLAIN_PAGE),
            ("4.png", b"invalid image"),
            ("5.jpg", BARCODE_PAGE),
            ("6.png", PLAIN_PAGE),
        ]);

        assert_eq!(candidate_pages(6), [6, 5, 4, 1, 2, 3]);
        assert_eq!(extract_isbn_barcode(&book, &media).as_deref(), Some("9782811632397"));
    }

    #[test]
    fn skips_epub_and_rejects_non_isbn_ean_values() {
        let (_temporary, book, mut media) = analyzed_archive(&[("page.png", PLAIN_PAGE)]);
        media.media_type = Some(EPUB_MEDIA_TYPE.into());
        media.status = MediaStatus::Ready;

        assert_eq!(extract_isbn_barcode(&book, &media), None);
        assert!(!valid_isbn_13("1234567890128"));
        assert!(valid_isbn_13("9782811632397"));
    }
}
