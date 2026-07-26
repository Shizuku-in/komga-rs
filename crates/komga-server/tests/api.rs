use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use komga_core::{
    AgeRestriction, AllowExclude, ContentRestrictions, Library, MediaExtension, ThumbnailBook, User, UserRole, new_id,
    scan_library_files,
};
use komga_media::{analyze_book, generate_thumbnail, image_dimension};
use komga_server::{AppState, REMEMBER_ME_COOKIE, SESSION_HEADER, ServerBindSettings, build_app};
use komga_storage::{Storage, TaskStorage};
use sha2::{Digest, Sha512};
use std::{
    fs,
    fs::File,
    io::{Cursor, Read, Write},
    path::PathBuf,
    time::Duration,
};
use tower::ServiceExt;
use url::Url;
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

mod support;

#[path = "api_cases/auth.rs"]
mod auth_cases;
#[path = "api_cases/book_files.rs"]
mod book_file_cases;
#[path = "api_cases/duplicates.rs"]
mod duplicate_cases;
#[path = "api_cases/fonts.rs"]
mod font_cases;
#[path = "api_cases/history.rs"]
mod history_cases;
#[path = "api_cases/kobo.rs"]
mod kobo_cases;
#[path = "api_cases/koreader.rs"]
mod koreader_cases;
#[path = "api_cases/legacy_content.rs"]
mod legacy_content_cases;
#[path = "api_cases/library_content.rs"]
mod library_content_cases;
#[path = "api_cases/lifecycle.rs"]
mod lifecycle_cases;
#[path = "api_cases/media.rs"]
mod media_cases;
#[path = "api_cases/metrics.rs"]
mod metrics_cases;
#[path = "api_cases/opds.rs"]
mod opds_cases;
#[path = "api_cases/operations.rs"]
mod operations_cases;
#[path = "api_cases/page_hashes.rs"]
mod page_hash_cases;
#[path = "api_cases/progress.rs"]
mod progress_cases;
#[path = "api_cases/referential.rs"]
mod referential_cases;
#[path = "api_cases/settings.rs"]
mod settings_cases;
#[path = "api_cases/source_deletion.rs"]
mod source_deletion_cases;
#[path = "api_cases/source_import.rs"]
mod source_import_cases;
#[path = "api_cases/sse.rs"]
mod sse_cases;
#[path = "api_cases/webpub.rs"]
mod webpub_cases;

use support::*;
