//! Streaming delivery of visible read-list source files as ZIP archives.

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
use komga_core::UserRole;
use komga_storage::ReadListFile;
use tokio_util::io::{ReaderStream, SyncIoBridge};
use url::Url;
use zip::{CompressionMethod, ZipWriter, result::ZipError, write::SimpleFileOptions};

use crate::{AppState, auth::AuthContext, book_file_routes::content_disposition, error::ApiError};

const COPY_BUFFER_CAPACITY: usize = 8 * 1024;
const PIPE_CAPACITY: usize = 64 * 1024;

struct ZipEntry {
    number: i64,
    book_id: String,
    path: PathBuf,
}

pub(crate) async fn download_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !auth.user.roles.contains(&UserRole::FileDownload) {
        return Err(ApiError::forbidden());
    }
    let read_list = state
        .storage
        .find_read_list(&id, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Read list not found"))?;
    let files = state.storage.read_list_files(&id, &auth.user).await?;
    if files.is_empty() {
        return Err(ApiError::not_found("Read list not found"));
    }
    let entries = files.into_iter().filter_map(zip_entry).collect();
    let (writer, reader) = tokio::io::duplex(PIPE_CAPACITY);
    tokio::task::spawn_blocking(move || write_read_list_zip(writer, entries));
    let mut response = Body::from_stream(ReaderStream::with_capacity(reader, COPY_BUFFER_CAPACITY)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&format!("{}.zip", read_list.name))
            .map_err(|_| ApiError::internal("Could not encode read-list attachment filename"))?,
    );
    Ok(response)
}

fn zip_entry(file: ReadListFile) -> Option<ZipEntry> {
    let path = match Url::parse(&file.url).ok().and_then(|url| url.to_file_path().ok()) {
        Some(path) => path,
        None => {
            tracing::warn!(book_id = %file.book_id, url = %file.url, "book URL is not a local file; skipping ZIP entry");
            return None;
        }
    };
    let Some(number) = file.number.checked_add(1).filter(|number| *number > 0) else {
        tracing::warn!(book_id = %file.book_id, number = file.number, "read-list member has an invalid position; skipping ZIP entry");
        return None;
    };
    Some(ZipEntry {
        number,
        book_id: file.book_id,
        path,
    })
}

fn write_read_list_zip(writer: tokio::io::DuplexStream, entries: Vec<ZipEntry>) {
    let bridge = SyncIoBridge::new(writer);
    let mut archive = ZipWriter::new_stream(bridge);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    let mut buffer = [0_u8; COPY_BUFFER_CAPACITY];
    for entry in entries {
        let metadata = match entry.path.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(%error, book_id = %entry.book_id, path = %entry.path.display(), "could not inspect book file; skipping ZIP entry");
                continue;
            }
        };
        if !metadata.is_file() {
            tracing::warn!(book_id = %entry.book_id, path = %entry.path.display(), "book path is not a regular file; skipping ZIP entry");
            continue;
        }
        let Some(file_name) = entry.path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            tracing::warn!(book_id = %entry.book_id, path = %entry.path.display(), "book path has no filename; skipping ZIP entry");
            continue;
        };
        let mut file = match File::open(&entry.path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(%error, book_id = %entry.book_id, path = %entry.path.display(), "could not open book file; skipping ZIP entry");
                continue;
            }
        };
        if let Err(error) = archive.start_file(format!("{} - {file_name}", entry.number), options) {
            log_zip_error(error, "could not start read-list ZIP entry");
            return;
        }
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if let Err(error) = archive.write_all(&buffer[..count]) {
                        log_io_error(error, "could not stream read-list ZIP entry");
                        return;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, book_id = %entry.book_id, path = %entry.path.display(), "could not read book file while streaming ZIP");
                    return;
                }
            }
        }
    }
    if let Err(error) = archive.finish() {
        log_zip_error(error, "could not finish read-list ZIP");
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
        tracing::debug!(%error, "read-list ZIP client disconnected");
    } else {
        tracing::warn!(%error, "{message}");
    }
}
