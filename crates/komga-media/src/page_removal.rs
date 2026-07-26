use std::{fs, io, path::Path};

use komga_core::{Book, HashedPageTarget, Media};
use thiserror::Error;

use crate::PageHashError;

/// Validated result of rewriting a book without selected hashed pages.
#[derive(Debug)]
pub struct PageRemovalRewrite {
    /// Filesystem snapshot represented by the rewritten file.
    pub book: Book,
    /// Reanalyzed media with surviving page hashes restored.
    pub media: Media,
    /// Snapshot-bound targets that were removed.
    pub removed: Vec<HashedPageTarget>,
    /// New-page-index to original-page-index mapping for retained pages.
    pub retained_original_indices: Vec<usize>,
    /// Whether the original first page was removed.
    pub removed_first_page: bool,
}

/// Failures produced while validating and rewriting hashed pages.
#[derive(Debug, Error)]
pub enum PageRemovalError {
    /// Cancellation was requested before the rewrite completed.
    #[error("page removal cancelled")]
    Cancelled,
    /// The task no longer matches the book, media, or target pages.
    #[error("page removal target is stale: {0}")]
    Stale(String),
    /// The source media cannot be rewritten.
    #[error("page removal is unsupported: {0}")]
    Unsupported(String),
    /// A source archive or generated archive could not be read or written.
    #[error("could not rewrite archive: {0}")]
    Archive(String),
    /// A source PDF or generated PDF could not be read or written.
    #[error("could not rewrite PDF: {0}")]
    Pdf(String),
    /// Filesystem metadata, hashing, or file creation failed.
    #[error("could not write rewritten media: {0}")]
    Io(#[from] io::Error),
    /// A page hash could not be recomputed for validation.
    #[error("could not validate page hash: {0}")]
    Hash(#[from] PageHashError),
    /// The generated media did not preserve the expected pages.
    #[error("rewritten media failed validation: {0}")]
    Validation(String),
}

/// Removes a newly created output on early return without touching a path that
/// existed before `create_new` succeeded.
pub(crate) struct CreatedOutput<'a> {
    path: &'a Path,
    armed: bool,
}

impl<'a> CreatedOutput<'a> {
    /// Creates a disarmed guard for a not-yet-created output path.
    pub(crate) fn new(path: &'a Path) -> Self {
        Self { path, armed: false }
    }

    /// Marks the path as owned after exclusive creation succeeds.
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }

    /// Preserves the validated output after a successful rewrite.
    pub(crate) fn keep(&mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedOutput<'_> {
    fn drop(&mut self) {
        if self.armed
            && fs::symlink_metadata(self.path)
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        {
            let _ = fs::remove_file(self.path);
        }
    }
}
