//! Streaming delivery of original book files.
//!
//! Both REST paths ignore client-supplied filename suffixes and derive response
//! metadata from the persisted book and its current source file. Attachment
//! filenames reproduce Spring's UTF-8 `ContentDisposition` representation so
//! existing clients can decode both the encoded-word and RFC 5987 parameters.

use std::{io, path::Path as FilePath};

use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use komga_core::UserRole;
use tokio::fs::File;
use tokio_util::io::ReaderStream;

use crate::{AppState, auth::AuthContext, content_routes::load_visible_book, error::ApiError};

const STREAM_BUFFER_CAPACITY: usize = 8 * 1024;
const MOVED_FILE_MESSAGE: &str = "File not found, it may have moved";

/// Downloads a book through the canonical REST path.
pub(crate) async fn download_book_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_id): Path<String>,
) -> Result<Response, ApiError> {
    download(&state, &auth, &book_id).await
}

/// Downloads a book while ignoring the compatibility filename suffix.
pub(crate) async fn download_book_file_with_name(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((book_id, _file_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    download(&state, &auth, &book_id).await
}

async fn download(state: &AppState, auth: &AuthContext, book_id: &str) -> Result<Response, ApiError> {
    require_file_download(auth)?;
    load_visible_book(state, &auth.user, book_id).await?;

    let book = state
        .storage
        .find_book_by_id(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let media = state
        .storage
        .find_media_by_book_id(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book analysis failed"))?;
    let file_path = book.url.to_file_path().map_err(|()| {
        tracing::warn!(%book_id, url = %book.url, "book URL is not a local file");
        ApiError::not_found(MOVED_FILE_MESSAGE)
    })?;
    let file_name = file_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| ApiError::not_found(MOVED_FILE_MESSAGE))?;
    let file = File::open(&file_path)
        .await
        .map_err(|error| moved_file_error(book_id, &file_path, error))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| moved_file_error(book_id, &file_path, error))?;
    if !metadata.is_file() {
        tracing::warn!(%book_id, path = %file_path.display(), "book path is not a regular file");
        return Err(ApiError::not_found(MOVED_FILE_MESSAGE));
    }

    let stream = ReaderStream::with_capacity(file, STREAM_BUFFER_CAPACITY);
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        media
            .media_type
            .as_deref()
            .and_then(|value| HeaderValue::from_str(value).ok())
            .unwrap_or_else(|| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string())
            .map_err(|_| ApiError::internal("Could not encode book file length"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&file_name).map_err(|_| ApiError::internal("Could not encode book attachment filename"))?,
    );
    Ok(response)
}

fn require_file_download(auth: &AuthContext) -> Result<(), ApiError> {
    auth.user
        .roles
        .contains(&UserRole::FileDownload)
        .then_some(())
        .ok_or_else(ApiError::forbidden)
}

fn moved_file_error(book_id: &str, path: &FilePath, error: io::Error) -> ApiError {
    tracing::warn!(%error, %book_id, path = %path.display(), "could not open book file");
    ApiError::not_found(MOVED_FILE_MESSAGE)
}

pub(crate) fn content_disposition(file_name: &str) -> Result<HeaderValue, http::header::InvalidHeaderValue> {
    HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        quoted_printable_filename(file_name),
        rfc5987_filename(file_name)
    ))
}

fn quoted_printable_filename(file_name: &str) -> String {
    let mut encoded = String::from("=?UTF-8?Q?");
    for byte in file_name.bytes() {
        match byte {
            b' ' => encoded.push('_'),
            33..=126 if !matches!(byte, b'"' | b'=' | b'?' | b'_') => encoded.push(char::from(byte)),
            _ => push_encoded_byte(&mut encoded, '=', byte),
        }
    }
    encoded.push_str("?=");
    encoded
}

fn rfc5987_filename(file_name: &str) -> String {
    let mut encoded = String::new();
    for byte in file_name.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            )
        {
            encoded.push(char::from(byte));
        } else {
            push_encoded_byte(&mut encoded, '%', byte);
        }
    }
    encoded
}

fn push_encoded_byte(output: &mut String, prefix: char, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    output.push(prefix);
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

#[cfg(test)]
mod tests {
    use super::{quoted_printable_filename, rfc5987_filename};

    #[test]
    fn attachment_filenames_match_spring_utf8_encoding() {
        assert_eq!(quoted_printable_filename("Volume 1.cbz"), "=?UTF-8?Q?Volume_1.cbz?=");
        assert_eq!(rfc5987_filename("Volume 1.cbz"), "Volume%201.cbz");
        assert_eq!(
            quoted_printable_filename("アキラ.cbz"),
            "=?UTF-8?Q?=E3=82=A2=E3=82=AD=E3=83=A9.cbz?="
        );
        assert_eq!(rfc5987_filename("アキラ.cbz"), "%E3%82%A2%E3%82%AD%E3%83%A9.cbz");
        assert_eq!(quoted_printable_filename("a_b\"c?.cbz"), "=?UTF-8?Q?a=5Fb=22c=3F.cbz?=");
        assert_eq!(rfc5987_filename("a_b\"c?.cbz"), "a_b%22c%3F.cbz");
    }
}
