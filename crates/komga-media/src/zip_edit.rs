use std::{
    collections::HashSet,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

use chrono::{DateTime, Local};
use komga_core::{Book, HashedPageTarget, Media, MediaStatus, ZIP_MEDIA_TYPE};
use url::Url;
use zip::{ZipArchive, ZipWriter};

use crate::{
    FileHashError, MediaAnalysisError, PageRemovalError, PageRemovalRewrite, analyze_book, hash_file, hash_page,
    page_removal::CreatedOutput,
};

/// Writes a validated ZIP containing every page except the snapshot-bound
/// targets to `output` without replacing the source file.
///
/// Every retained entry is copied as compressed data in original archive
/// order. Entry metadata, archive comment, directories, and non-page files are
/// preserved. The source is never modified, and `output` must not exist.
///
/// # Errors
///
/// Returns [`PageRemovalError::Stale`] when any task snapshot differs from
/// current media or source metadata, and [`PageRemovalError::Validation`]
/// when the generated archive differs from the expected survivors.
pub fn rewrite_hashed_zip_pages(
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
    if media.status != MediaStatus::Ready || media.media_type.as_deref() != Some(ZIP_MEDIA_TYPE) {
        return Err(PageRemovalError::Unsupported(
            "media must be a ready application/zip".into(),
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

    let mut target_indices = HashSet::new();
    for target in targets {
        let index = usize::try_from(target.page_number - 1)
            .map_err(|_| PageRemovalError::Stale("page number is not positive".into()))?;
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
    }
    if target_indices.len() >= media.pages.len() {
        return Err(PageRemovalError::Stale("removal would delete every page".into()));
    }

    validate_source_archive(&path, targets, &cancelled)?;
    let removed_names = targets
        .iter()
        .map(|target| target.file_name.as_str())
        .collect::<HashSet<_>>();
    let retained_original_indices = media
        .pages
        .iter()
        .enumerate()
        .filter(|(index, _)| !target_indices.contains(index))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let retained_pages = retained_original_indices
        .iter()
        .map(|index| media.pages[*index].clone())
        .collect::<Vec<_>>();

    let mut output_guard = CreatedOutput::new(output);
    let mut rewritten = OpenOptions::new().write(true).create_new(true).open(output)?;
    output_guard.arm();
    rewrite_archive(&path, &mut rewritten, &removed_names, &cancelled)?;
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
    validate_created_media(&created_media, &retained_pages, media)?;
    for page in &mut created_media.pages {
        let previous = retained_pages
            .iter()
            .find(|candidate| {
                candidate.file_name == page.file_name
                    && candidate.media_type == page.media_type
                    && candidate.file_size == page.file_size
            })
            .ok_or_else(|| PageRemovalError::Validation(format!("unexpected page {}", page.file_name)))?;
        page.file_hash.clone_from(&previous.file_hash);
    }

    if cancelled() {
        return Err(PageRemovalError::Cancelled);
    }
    let output_fingerprint = source_fingerprint(output, &cancelled)?;
    let current_metadata = fs::metadata(&path)?;
    ensure_same_metadata(&initial_metadata, &current_metadata, &path)?;
    let current_fingerprint = source_fingerprint(&path, &cancelled)?;
    if initial_fingerprint != current_fingerprint {
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

fn validate_source_archive(
    path: &Path,
    targets: &[HashedPageTarget],
    cancelled: &impl Fn() -> bool,
) -> Result<(), PageRemovalError> {
    ensure_unique_entry_names(path)?;
    let mut archive = ZipArchive::new(File::open(path)?).map_err(archive_error)?;
    let mut names = HashSet::new();
    for index in 0..archive.len() {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let entry = archive.by_index(index).map_err(archive_error)?;
        if !names.insert(entry.name().to_owned()) {
            return Err(PageRemovalError::Stale(format!(
                "archive contains duplicate entry name {:?}",
                entry.name()
            )));
        }
    }
    for target in targets {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let mut entry = archive.by_name(&target.file_name).map_err(archive_error)?;
        if i64::try_from(entry.size()).ok() != Some(target.file_size) {
            return Err(PageRemovalError::Stale(format!(
                "entry size changed for {:?}",
                target.file_name
            )));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry.read_to_end(&mut bytes).map_err(PageRemovalError::Io)?;
        if hash_page(&bytes, &target.media_type)?.hash != target.file_hash {
            return Err(PageRemovalError::Stale(format!(
                "entry hash changed for {:?}",
                target.file_name
            )));
        }
    }
    Ok(())
}

fn ensure_unique_entry_names(path: &Path) -> Result<(), PageRemovalError> {
    const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    const ZIP64_EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x06];
    const CENTRAL_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];

    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let tail_len = usize::try_from(file_len.min(65_557)).unwrap_or(65_557);
    file.seek(SeekFrom::End(-i64::try_from(tail_len).unwrap_or(i64::MAX)))?;
    let mut tail = vec![0; tail_len];
    file.read_exact(&mut tail)?;
    let eocd_index = tail
        .windows(EOCD_SIGNATURE.len())
        .rposition(|value| value == EOCD_SIGNATURE)
        .ok_or_else(|| PageRemovalError::Archive("end-of-central-directory record is missing".into()))?;
    if tail.len().saturating_sub(eocd_index) < 22 {
        return Err(PageRemovalError::Archive(
            "end-of-central-directory record is truncated".into(),
        ));
    }
    let eocd = &tail[eocd_index..];
    let mut entry_count = u64::from(u16::from_le_bytes([eocd[10], eocd[11]]));
    let mut central_offset = u64::from(u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]));
    if entry_count == u64::from(u16::MAX) || central_offset == u64::from(u32::MAX) {
        let eocd_absolute = file_len - u64::try_from(tail.len() - eocd_index).unwrap_or(0);
        if eocd_absolute < 20 {
            return Err(PageRemovalError::Archive("ZIP64 locator is missing".into()));
        }
        file.seek(SeekFrom::Start(eocd_absolute - 20))?;
        let mut locator = [0; 20];
        file.read_exact(&mut locator)?;
        if locator[..4] != ZIP64_LOCATOR_SIGNATURE {
            return Err(PageRemovalError::Archive("ZIP64 locator is invalid".into()));
        }
        let zip64_offset = u64::from_le_bytes(locator[8..16].try_into().expect("fixed slice"));
        file.seek(SeekFrom::Start(zip64_offset))?;
        let mut zip64 = [0; 56];
        file.read_exact(&mut zip64)?;
        if zip64[..4] != ZIP64_EOCD_SIGNATURE {
            return Err(PageRemovalError::Archive("ZIP64 end record is invalid".into()));
        }
        entry_count = u64::from_le_bytes(zip64[32..40].try_into().expect("fixed slice"));
        central_offset = u64::from_le_bytes(zip64[48..56].try_into().expect("fixed slice"));
    }

    file.seek(SeekFrom::Start(central_offset))?;
    let mut names = HashSet::<Vec<u8>>::new();
    for _ in 0..entry_count {
        let mut header = [0; 46];
        file.read_exact(&mut header)?;
        if header[..4] != CENTRAL_SIGNATURE {
            return Err(PageRemovalError::Archive("central-directory entry is invalid".into()));
        }
        let name_len = usize::from(u16::from_le_bytes([header[28], header[29]]));
        let extra_len = u64::from(u16::from_le_bytes([header[30], header[31]]));
        let comment_len = u64::from(u16::from_le_bytes([header[32], header[33]]));
        let mut name = vec![0; name_len];
        file.read_exact(&mut name)?;
        if !names.insert(name.clone()) {
            return Err(PageRemovalError::Stale(format!(
                "archive contains duplicate entry name {:?}",
                String::from_utf8_lossy(&name)
            )));
        }
        file.seek(SeekFrom::Current(i64::try_from(extra_len + comment_len).map_err(
            |_| PageRemovalError::Archive("central-directory entry is too large".into()),
        )?))?;
    }
    Ok(())
}

fn rewrite_archive(
    path: &Path,
    temporary: &mut File,
    removed_names: &HashSet<&str>,
    cancelled: &impl Fn() -> bool,
) -> Result<(), PageRemovalError> {
    let mut archive = ZipArchive::new(File::open(path)?).map_err(archive_error)?;
    let comment = archive.comment().to_vec();
    let mut writer = ZipWriter::new(temporary);
    writer
        .set_raw_comment(comment.into_boxed_slice())
        .map_err(archive_error)?;
    for index in 0..archive.len() {
        if cancelled() {
            return Err(PageRemovalError::Cancelled);
        }
        let entry = archive.by_index(index).map_err(archive_error)?;
        if !removed_names.contains(entry.name()) {
            if entry.is_dir() {
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
    }
    writer.finish().map_err(archive_error)?;
    Ok(())
}

fn validate_created_media(
    created: &Media,
    retained_pages: &[komga_core::BookPage],
    previous: &Media,
) -> Result<(), PageRemovalError> {
    if created.status != MediaStatus::Ready || created.media_type.as_deref() != Some(ZIP_MEDIA_TYPE) {
        return Err(PageRemovalError::Validation(
            "temporary archive is not a ready ZIP".into(),
        ));
    }
    let pages_match = created.pages.len() == retained_pages.len()
        && created.pages.iter().zip(retained_pages).all(|(left, right)| {
            left.file_name == right.file_name
                && left.media_type == right.media_type
                && left.file_size == right.file_size
        });
    if !pages_match {
        return Err(PageRemovalError::Validation(
            "temporary archive does not contain exactly the retained pages".into(),
        ));
    }
    let files_match = created.files.len() == previous.files.len()
        && created.files.iter().zip(&previous.files).all(|(left, right)| {
            left.file_name == right.file_name
                && left.media_type == right.media_type
                && left.file_size == right.file_size
                && left.sub_type == right.sub_type
        });
    if !files_match {
        return Err(PageRemovalError::Validation(
            "temporary archive does not contain exactly the ancillary files".into(),
        ));
    }
    Ok(())
}

pub(crate) fn source_fingerprint(path: &Path, cancelled: &impl Fn() -> bool) -> Result<String, PageRemovalError> {
    hash_file(path, cancelled).map_err(|error| match error {
        FileHashError::Cancelled => PageRemovalError::Cancelled,
        FileHashError::Io(error) => PageRemovalError::Io(error),
        FileHashError::InvalidSource(source) | FileHashError::SourceChanged(source) => PageRemovalError::Stale(source),
    })
}

pub(crate) fn ensure_book_snapshot(book: &Book, metadata: &Metadata) -> Result<(), PageRemovalError> {
    let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
    if size != book.file_size || scan_timestamp(metadata) != book.file_last_modified {
        return Err(PageRemovalError::Stale(
            "book filesystem snapshot changed before rewrite".into(),
        ));
    }
    Ok(())
}

pub(crate) fn ensure_same_metadata(
    initial: &Metadata,
    current: &Metadata,
    path: &Path,
) -> Result<(), PageRemovalError> {
    if initial.len() != current.len() || initial.modified()? != current.modified()? {
        return Err(PageRemovalError::Stale(format!(
            "source metadata changed during rewrite: {}",
            path.display()
        )));
    }
    Ok(())
}

pub(crate) fn scan_timestamp(metadata: &Metadata) -> chrono::NaiveDateTime {
    let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let created = metadata.created().unwrap_or(modified);
    DateTime::<Local>::from(modified.max(created)).naive_local()
}

fn archive_error(error: zip::result::ZipError) -> PageRemovalError {
    PageRemovalError::Archive(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        fs::File,
        io::{Cursor, Read, Write},
        path::Path,
    };

    use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
    use komga_core::{Book, HashedPageTarget, new_id};
    use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::extract_page;

    fn remove_hashed_zip_pages(
        book: &Book,
        media: &Media,
        targets: &[HashedPageTarget],
        analyze_dimensions: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<PageRemovalRewrite, PageRemovalError> {
        let source = book.url.to_file_path().unwrap();
        let output = source.with_extension("rewrite.tmp");
        let result = rewrite_hashed_zip_pages(book, media, targets, &output, analyze_dimensions, cancelled)?;
        fs::rename(output, source)?;
        Ok(result)
    }

    #[test]
    fn removal_preserves_raw_entries_order_metadata_directories_and_files() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        write_zip(&path, &["folder/", "02.png", "ComicInfo.xml", "01.png", "03.png"]);
        let (book, media) = hashed_media(&path);
        let before = raw_entries(&path);
        let target = target(&media, 2);

        let result = remove_hashed_zip_pages(&book, &media, std::slice::from_ref(&target), true, || false).unwrap();
        assert_eq!(result.removed, [target]);
        assert!(!result.removed_first_page);
        assert_eq!(
            result
                .media
                .pages
                .iter()
                .map(|page| page.file_name.as_str())
                .collect::<Vec<_>>(),
            ["01.png", "03.png"]
        );
        assert!(result.media.pages.iter().all(|page| !page.file_hash.is_empty()));
        assert!(result.book.file_hash.is_empty() && result.book.file_hash_koreader.is_empty());

        let after = raw_entries(&path);
        let expected = before
            .into_iter()
            .filter(|entry| entry.name != "02.png")
            .collect::<Vec<_>>();
        assert_eq!(after, expected);
        assert_eq!(fs::read(&path).unwrap().len() as i64, result.book.file_size);
    }

    #[test]
    fn first_and_multiple_page_removal_is_reported_and_validated() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("book.cbz");
        write_zip(&path, &["01.png", "02.png", "03.png", "notes.txt"]);
        let (book, media) = hashed_media(&path);

        let result =
            remove_hashed_zip_pages(&book, &media, &[target(&media, 1), target(&media, 3)], false, || false).unwrap();
        assert!(result.removed_first_page);
        assert_eq!(result.media.pages.len(), 1);
        assert_eq!(result.media.pages[0].file_name, "02.png");
        assert_eq!(result.media.files[0].file_name, "notes.txt");
    }

    #[test]
    fn stale_all_page_duplicate_name_and_cancelled_removals_leave_source_unchanged() {
        let temporary = tempfile::tempdir().unwrap();

        let stale_path = temporary.path().join("stale.cbz");
        write_zip(&stale_path, &["01.png", "02.png"]);
        let (book, media) = hashed_media(&stale_path);
        let original = fs::read(&stale_path).unwrap();
        let mut stale = target(&media, 1);
        stale.file_size += 1;
        assert!(matches!(
            remove_hashed_zip_pages(&book, &media, &[stale], false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        assert_eq!(fs::read(&stale_path).unwrap(), original);
        assert!(matches!(
            remove_hashed_zip_pages(&book, &media, &[target(&media, 1), target(&media, 2)], false, || false),
            Err(PageRemovalError::Stale(_))
        ));
        assert_eq!(fs::read(&stale_path).unwrap(), original);
        assert!(matches!(
            remove_hashed_zip_pages(&book, &media, &[target(&media, 1)], false, || true),
            Err(PageRemovalError::Cancelled)
        ));
        assert_eq!(fs::read(&stale_path).unwrap(), original);

        let existing_output = temporary.path().join("existing.tmp");
        fs::write(&existing_output, b"existing output").unwrap();
        assert!(matches!(
            rewrite_hashed_zip_pages(
                &book,
                &media,
                &[target(&media, 1)],
                &existing_output,
                false,
                || false
            ),
            Err(PageRemovalError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read(&existing_output).unwrap(), b"existing output");

        let duplicate_path = temporary.path().join("duplicate.cbz");
        write_zip(&duplicate_path, &["same1.png", "same2.png", "other.png"]);
        let mut duplicate_bytes = fs::read(&duplicate_path).unwrap();
        let offsets = duplicate_bytes
            .windows(b"same2.png".len())
            .enumerate()
            .filter_map(|(index, value)| (value == b"same2.png").then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(offsets.len(), 2);
        for offset in offsets {
            duplicate_bytes[offset..offset + b"same1.png".len()].copy_from_slice(b"same1.png");
        }
        fs::write(&duplicate_path, duplicate_bytes).unwrap();
        let (duplicate_book, duplicate_media) = hashed_media(&duplicate_path);
        let original = fs::read(&duplicate_path).unwrap();
        let duplicate_result = remove_hashed_zip_pages(
            &duplicate_book,
            &duplicate_media,
            &[target(&duplicate_media, 1)],
            false,
            || false,
        );
        assert!(
            matches!(duplicate_result, Err(PageRemovalError::Stale(_))),
            "{duplicate_result:?}"
        );
        assert_eq!(fs::read(&duplicate_path).unwrap(), original);
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RawEntry {
        name: String,
        compression: CompressionMethod,
        crc32: u32,
        size: u64,
        compressed_size: u64,
        unix_mode: Option<u32>,
        modified: Option<zip::DateTime>,
        bytes: Vec<u8>,
    }

    fn raw_entries(path: &Path) -> Vec<RawEntry> {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        (0..archive.len())
            .map(|index| {
                let mut entry = archive.by_index_raw(index).unwrap();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).unwrap();
                RawEntry {
                    name: entry.name().to_owned(),
                    compression: entry.compression(),
                    crc32: entry.crc32(),
                    size: entry.size(),
                    compressed_size: entry.compressed_size(),
                    unix_mode: entry.unix_mode(),
                    modified: entry.last_modified(),
                    bytes,
                }
            })
            .collect()
    }

    fn write_zip(path: &Path, names: &[&str]) {
        let file = File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        writer.set_comment("archive comment").unwrap();
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(0o640);
        let directory_options = SimpleFileOptions::default().unix_permissions(0o750);
        for (index, name) in names.iter().enumerate() {
            if name.ends_with('/') {
                writer.add_directory(*name, directory_options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                if name.ends_with(".png") {
                    writer.write_all(&png(index as u8)).unwrap();
                } else {
                    writer.write_all(format!("content-{index}").as_bytes()).unwrap();
                }
            }
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
