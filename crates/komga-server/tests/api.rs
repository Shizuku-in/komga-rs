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
use komga_server::{AppState, REMEMBER_ME_COOKIE, SESSION_HEADER, build_app};
use komga_storage::{Storage, TaskStorage};
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
#[path = "api_cases/library_content.rs"]
mod library_content_cases;
#[path = "api_cases/lifecycle.rs"]
mod lifecycle_cases;
#[path = "api_cases/media.rs"]
mod media_cases;
#[path = "api_cases/progress.rs"]
mod progress_cases;
#[path = "api_cases/sse.rs"]
mod sse_cases;
#[path = "api_cases/webpub.rs"]
mod webpub_cases;

use support::*;
