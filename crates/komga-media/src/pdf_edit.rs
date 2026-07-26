use std::{
    collections::HashSet,
    fs::{self, Metadata, OpenOptions},
    io::Write,
    path::Path,
    time::SystemTime,
};

use chrono::{DateTime, Local};
use hayro::hayro_syntax::Pdf;
use image::GenericImageView;
use komga_core::{Book, HashedPageTarget, JPEG_MEDIA_TYPE, Media, MediaProfile, MediaStatus, PDF_MEDIA_TYPE};
use url::Url;

use crate::{
    FileHashError, MediaAnalysisError, PageRemovalError, PageRemovalRewrite, analyze_book, hash_file, hash_page,
    page_removal::CreatedOutput, pdf,
};

/// Writes a validated PDF containing every page except the snapshot-bound
/// targets to `output` without replacing the source file.
///
/// Page content, resources, dimensions, rotation, and order are preserved.
/// Document-level bookmarks, attachments, forms, page labels, and catalog
/// metadata are not retained. The source is never modified, and `output` must
/// not exist.
///
/// # Errors
///
/// Returns [`PageRemovalError::Stale`] when a source or target snapshot no
/// longer matches, and [`PageRemovalError::Validation`] when retained pages do
/// not render identically after rewriting.
pub fn rewrite_hashed_pdf_pages(
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
    if media.status != MediaStatus::Ready || media.media_type.as_deref() != Some(PDF_MEDIA_TYPE) {
        return Err(PageRemovalError::Unsupported(
            "media must be a ready application/pdf".into(),
        ));
    }
    if targets.is_empty() {
        return Err(PageRemovalError::Stale("no pages were requested".into()));
    }
    let source = book
        .url
        .to_file_path()
        .map_err(|()| PageRemovalError::Unsupported("book URL is not a local file".into()))?;
    if source == output {
        return Err(PageRemovalError::Unsupported(
            "output must differ from the source".into(),
        ));
    }
    let initial_metadata = fs::metadata(&source)?;
    ensure_book_snapshot(book, &initial_metadata)?;
    let initial_fingerprint = source_fingerprint(&source, &cancelled)?;
    let bytes = fs::read(&source)?;
    let document = Pdf::new(bytes).map_err(|error| PageRemovalError::Pdf(format!("invalid source PDF: {error:?}")))?;
    if document.pages().len() != media.pages.len() {
        return Err(PageRemovalError::Stale(
            "PDF page count no longer matches analyzed media".into(),
        ));
    }

    let mut target_indices = HashSet::new();
    for target in targets {
        let index = target
            .page_number
            .checked_sub(1)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| PageRemovalError::Stale("page number is not positive".into()))?;
        if !target_indices.insert(index) {
            return Err(PageRemovalError::Stale(
                "the same page was requested more than once".into(),
            ));
        }
        let page = media
            .pages
            .get(index)
            .ok_or_else(|| PageRemovalError::Stale("page number is outside current media".into()))?;
        if page.file_name != target.file_name
            || page.file_name != (index + 1).to_string()
            || !page.media_type.is_empty()
            || page.file_size.is_some()
            || target.media_type != JPEG_MEDIA_TYPE
            || target.file_size != 0
            || page.file_hash != target.file_hash
            || target.file_hash.is_empty()
        {
            return Err(PageRemovalError::Stale(format!(
                "page {} no longer matches its task payload",
                target.page_number
            )));
        }
    }
    if target_indices.len() >= media.pages.len() {
        return Err(PageRemovalError::Stale("removal would delete every page".into()));
    }
    for target in targets {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let index = usize::try_from(target.page_number - 1).expect("validated positive page number");
        let rendered = pdf::render_page(&source, index).map_err(media_error)?;
        if hash_page(&rendered.bytes, &rendered.media_type)?.hash != target.file_hash {
            return Err(PageRemovalError::Stale(format!(
                "rendered hash changed for page {}",
                target.page_number
            )));
        }
    }
    let retained_original_indices = (0..media.pages.len())
        .filter(|index| !target_indices.contains(index))
        .collect::<Vec<_>>();
    let rewritten_bytes = pdf::write_pages(&document, &retained_original_indices).map_err(media_error)?;

    let mut output_guard = CreatedOutput::new(output);
    let mut rewritten = OpenOptions::new().write(true).create_new(true).open(output)?;
    output_guard.arm();
    let result = (|| {
        rewritten.write_all(&rewritten_bytes)?;
        rewritten.set_permissions(initial_metadata.permissions())?;
        rewritten.sync_all()?;
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }

        let rewritten_metadata = rewritten.metadata()?;
        let mut temporary_book = book.clone();
        temporary_book.url = Url::from_file_path(output)
            .map_err(|()| PageRemovalError::Unsupported("output path cannot be represented as a URL".into()))?;
        temporary_book.file_size = i64::try_from(rewritten_metadata.len()).unwrap_or(i64::MAX);
        temporary_book.file_last_modified = scan_timestamp(&rewritten_metadata);
        temporary_book.file_hash.clear();
        temporary_book.file_hash_koreader.clear();
        let mut created_media =
            analyze_book(&temporary_book, analyze_dimensions, &cancelled).map_err(|error| match error {
                MediaAnalysisError::Cancelled => PageRemovalError::Cancelled,
            })?;
        if created_media.status != MediaStatus::Ready
            || created_media.profile() != Some(MediaProfile::Pdf)
            || created_media.pages.len() != retained_original_indices.len()
            || created_media.pages.iter().enumerate().any(|(index, page)| {
                page.file_name != (index + 1).to_string() || !page.media_type.is_empty() || page.file_size.is_some()
            })
        {
            return Err(PageRemovalError::Validation(
                "rewritten file is not the expected ready PDF".into(),
            ));
        }

        for (new_index, original_index) in retained_original_indices.iter().copied().enumerate() {
            if cancelled() {
                return Err(PageRemovalError::Cancelled);
            }
            let source_render = pdf::render_page(&source, original_index).map_err(media_error)?;
            let output_render = pdf::render_page(output, new_index).map_err(media_error)?;
            let source_hash = hash_page(&source_render.bytes, &source_render.media_type)?;
            let output_hash = hash_page(&output_render.bytes, &output_render.media_type)?;
            let source_dimensions = image::load_from_memory(&source_render.bytes)
                .map_err(|error| PageRemovalError::Validation(error.to_string()))?
                .dimensions();
            let output_dimensions = image::load_from_memory(&output_render.bytes)
                .map_err(|error| PageRemovalError::Validation(error.to_string()))?
                .dimensions();
            if source_hash != output_hash || source_dimensions != output_dimensions {
                return Err(PageRemovalError::Validation(format!(
                    "rewritten page {} does not render like original page {}",
                    new_index + 1,
                    original_index + 1
                )));
            }
            created_media.pages[new_index]
                .file_hash
                .clone_from(&media.pages[original_index].file_hash);
        }

        let current_metadata = fs::metadata(&source)?;
        ensure_same_metadata(&initial_metadata, &current_metadata, &source)?;
        if source_fingerprint(&source, &cancelled)? != initial_fingerprint {
            return Err(PageRemovalError::Stale("source bytes changed during rewrite".into()));
        }
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let mut updated_book = book.clone();
        updated_book.file_size = i64::try_from(rewritten_metadata.len()).unwrap_or(i64::MAX);
        updated_book.file_last_modified = scan_timestamp(&rewritten_metadata);
        updated_book.file_hash.clear();
        updated_book.file_hash_koreader.clear();
        Ok(PageRemovalRewrite {
            book: updated_book,
            media: created_media,
            removed: targets.to_vec(),
            retained_original_indices,
            removed_first_page: targets.iter().any(|target| target.page_number == 1),
        })
    })();
    if result.is_ok() {
        output_guard.keep();
    }
    result
}

fn media_error(error: crate::MediaOperationError) -> PageRemovalError {
    PageRemovalError::Pdf(error.to_string())
}

fn source_fingerprint(path: &Path, cancelled: &impl Fn() -> bool) -> Result<String, PageRemovalError> {
    hash_file(path, cancelled).map_err(|error| match error {
        FileHashError::Cancelled => PageRemovalError::Cancelled,
        FileHashError::Io(error) => PageRemovalError::Io(error),
        FileHashError::InvalidSource(source) | FileHashError::SourceChanged(source) => PageRemovalError::Stale(source),
    })
}

fn ensure_book_snapshot(book: &Book, metadata: &Metadata) -> Result<(), PageRemovalError> {
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    if size != book.file_size || scan_timestamp(metadata) != book.file_last_modified {
        return Err(PageRemovalError::Stale(
            "book filesystem snapshot changed before rewrite".into(),
        ));
    }
    Ok(())
}

fn ensure_same_metadata(initial: &Metadata, current: &Metadata, path: &Path) -> Result<(), PageRemovalError> {
    if initial.len() != current.len() || initial.modified()? != current.modified()? {
        return Err(PageRemovalError::Stale(format!(
            "source metadata changed during rewrite: {}",
            path.display()
        )));
    }
    Ok(())
}

fn scan_timestamp(metadata: &Metadata) -> chrono::NaiveDateTime {
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    DateTime::<Local>::from(modified.max(created)).naive_local()
}

#[cfg(test)]
mod tests {
    use pdf_writer::{Content, Finish, Rect, Ref};

    use super::*;
    use crate::extract_page;
    use komga_core::new_id;

    #[test]
    fn removes_pdf_pages_in_order_and_preserves_rendered_content() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("book.pdf");
        fs::write(&source, colored_pdf()).unwrap();
        let (book, media) = hashed_media(&source);
        let original_bytes = fs::read(&source).unwrap();
        let output = temporary.path().join("rewritten.tmp");

        let rewrite = rewrite_hashed_pdf_pages(&book, &media, &[target(&media, 2)], &output, true, || false).unwrap();

        assert_eq!(fs::read(&source).unwrap(), original_bytes);
        assert_eq!(rewrite.retained_original_indices, [0, 2]);
        assert_eq!(
            rewrite
                .media
                .pages
                .iter()
                .map(|page| (page.file_name.as_str(), page.file_hash.as_str()))
                .collect::<Vec<_>>(),
            [
                ("1", media.pages[0].file_hash.as_str()),
                ("2", media.pages[2].file_hash.as_str())
            ]
        );
        assert_eq!(Pdf::new(fs::read(&output).unwrap()).unwrap().pages().len(), 2);

        assert!(rewrite.media.pages.iter().all(|page| !page.file_hash.is_empty()));

        let multiple_output = temporary.path().join("multiple.tmp");
        let multiple = rewrite_hashed_pdf_pages(
            &book,
            &media,
            &[target(&media, 1), target(&media, 3)],
            &multiple_output,
            true,
            || false,
        )
        .unwrap();
        assert!(multiple.removed_first_page);
        assert_eq!(multiple.retained_original_indices, [1]);
        assert_eq!(multiple.media.pages[0].file_name, "1");
        assert_eq!(multiple.media.pages[0].file_hash, media.pages[1].file_hash);
        assert_eq!(Pdf::new(fs::read(&multiple_output).unwrap()).unwrap().pages().len(), 1);
    }

    #[test]
    fn rejects_stale_duplicate_out_of_bounds_all_page_and_cancelled_pdf_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("book.pdf");
        fs::write(&source, colored_pdf()).unwrap();
        let (book, media) = hashed_media(&source);
        let output = temporary.path().join("rewritten.tmp");

        let mut stale = target(&media, 1);
        stale.file_hash.push('0');
        assert!(matches!(
            rewrite_hashed_pdf_pages(&book, &media, &[stale], &output, false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        let first = target(&media, 1);
        assert!(matches!(
            rewrite_hashed_pdf_pages(&book, &media, &[first.clone(), first], &output, false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        let mut outside = target(&media, 1);
        outside.page_number = 4;
        outside.file_name = "4".into();
        assert!(matches!(
            rewrite_hashed_pdf_pages(&book, &media, &[outside], &output, false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        assert!(matches!(
            rewrite_hashed_pdf_pages(
                &book,
                &media,
                &[target(&media, 1), target(&media, 2), target(&media, 3)],
                &output,
                false,
                || false
            ),
            Err(PageRemovalError::Stale(_))
        ));
        assert!(matches!(
            rewrite_hashed_pdf_pages(&book, &media, &[target(&media, 1)], &output, false, || true),
            Err(PageRemovalError::Cancelled)
        ));
        assert!(!output.exists());

        fs::write(&output, b"existing output").unwrap();
        assert!(matches!(
            rewrite_hashed_pdf_pages(&book, &media, &[target(&media, 1)], &output, false, || false),
            Err(PageRemovalError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read(&output).unwrap(), b"existing output");
    }

    fn colored_pdf() -> Vec<u8> {
        let mut output = pdf_writer::Pdf::new();
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page_refs = [Ref::new(3), Ref::new(4), Ref::new(5)];
        let content_refs = [Ref::new(6), Ref::new(7), Ref::new(8)];
        output.catalog(catalog).pages(pages);
        output.pages(pages).kids(page_refs).count(3);
        for (index, (page_ref, content_ref)) in page_refs.into_iter().zip(content_refs).enumerate() {
            let mut page = output.page(page_ref);
            page.parent(pages);
            page.media_box(Rect::new(0.0, 0.0, 120.0 + index as f32 * 10.0, 180.0));
            page.contents(content_ref);
            page.resources();
            page.finish();

            let mut content = Content::new();
            content.set_fill_rgb(index as f32 / 2.0, 0.2, 1.0 - index as f32 / 2.0);
            content.rect(10.0, 10.0, 80.0, 120.0);
            content.fill_nonzero();
            output.stream(content_ref, &content.finish());
        }
        output.finish()
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
            media.pages[index].file_hash = hash_page(&content.bytes, &content.media_type).unwrap().hash;
        }
        (book, media)
    }

    fn target(media: &Media, page_number: i32) -> HashedPageTarget {
        let page = &media.pages[usize::try_from(page_number - 1).unwrap()];
        HashedPageTarget {
            file_name: page.file_name.clone(),
            media_type: JPEG_MEDIA_TYPE.into(),
            file_hash: page.file_hash.clone(),
            file_size: 0,
            page_number,
        }
    }
}
