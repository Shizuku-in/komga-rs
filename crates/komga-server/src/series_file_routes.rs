//! Streaming delivery of every source file in a visible series as a ZIP archive.

use std::{
    fs::File,
    io::{self, Read, Write},
    path::PathBuf,
};

use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use komga_core::{Book, UserRole};
use tokio_util::io::{ReaderStream, SyncIoBridge};
use zip::{CompressionMethod, ZipWriter, result::ZipError, write::SimpleFileOptions};

use crate::{
    AppState, auth::AuthContext, book_file_routes::content_disposition, content_access::load_visible_series,
    error::ApiError,
};

const COPY_BUFFER_CAPACITY: usize = 8 * 1024;
const PIPE_CAPACITY: usize = 64 * 1024;

struct ZipEntry {
    book_id: String,
    path: PathBuf,
}

pub(crate) async fn download_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(series_id): Path<String>,
) -> Result<Response, ApiError> {
    if !auth.user.roles.contains(&UserRole::FileDownload) {
        return Err(ApiError::forbidden());
    }
    let series = load_visible_series(&state, &auth.user, &series_id).await?;
    let books = state.storage.list_books_by_series_id(&series_id).await?;
    let entries = books.into_iter().filter_map(zip_entry).collect();
    let (writer, reader) = tokio::io::duplex(PIPE_CAPACITY);
    tokio::task::spawn_blocking(move || write_series_zip(writer, entries));

    let mut response = Body::from_stream(ReaderStream::with_capacity(reader, COPY_BUFFER_CAPACITY)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&format!("{}.zip", series.metadata.title))
            .map_err(|_| ApiError::internal("Could not encode series attachment filename"))?,
    );
    Ok(response)
}

fn zip_entry(book: Book) -> Option<ZipEntry> {
    let path = match book.url.to_file_path() {
        Ok(path) => path,
        Err(()) => {
            tracing::warn!(book_id = %book.id, url = %book.url, "book URL is not a local file; skipping series ZIP entry");
            return None;
        }
    };
    Some(ZipEntry { book_id: book.id, path })
}

fn write_series_zip(writer: tokio::io::DuplexStream, entries: Vec<ZipEntry>) {
    let bridge = SyncIoBridge::new(writer);
    let mut archive = ZipWriter::new_stream(bridge);
    archive.set_raw_zip64_extensible_data_sector(Box::default());
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        // zip 8.6 rejects DEFLATE level 0 despite documenting it; level 1 is its
        // lowest supported streaming setting and preserves the wire format.
        .compression_level(Some(1))
        .large_file(true);
    let mut buffer = [0_u8; COPY_BUFFER_CAPACITY];
    for entry in entries {
        let metadata = match entry.path.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(%error, book_id = %entry.book_id, path = %entry.path.display(), "could not inspect book file; skipping series ZIP entry");
                continue;
            }
        };
        if !metadata.is_file() {
            tracing::warn!(book_id = %entry.book_id, path = %entry.path.display(), "book path is not a regular file; skipping series ZIP entry");
            continue;
        }
        let Some(file_name) = entry.path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            tracing::warn!(book_id = %entry.book_id, path = %entry.path.display(), "book path has no filename; skipping series ZIP entry");
            continue;
        };
        let mut file = match File::open(&entry.path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(%error, book_id = %entry.book_id, path = %entry.path.display(), "could not open book file; skipping series ZIP entry");
                continue;
            }
        };
        if let Err(error) = archive.start_file(file_name, options) {
            log_zip_error(error, "could not start series ZIP entry");
            return;
        }
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if let Err(error) = archive.write_all(&buffer[..count]) {
                        log_io_error(error, "could not stream series ZIP entry");
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, book_id = %entry.book_id, path = %entry.path.display(), "could not read book file while streaming series ZIP");
                    return;
                }
            }
        }
    }
    if let Err(error) = archive.finish() {
        log_zip_error(error, "could not finish series ZIP");
    }
}

fn log_zip_error(error: ZipError, message: &'static str) {
    match error {
        ZipError::Io(error) => log_io_error(error, message),
        error => tracing::warn!(%error, "{message}"),
    }
}

fn log_io_error(error: io::Error, message: &'static str) {
    if matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) {
        tracing::debug!(%error, "series ZIP client disconnected");
    } else {
        tracing::warn!(%error, "{message}");
    }
}
