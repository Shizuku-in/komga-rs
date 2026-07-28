use std::{fs, net::SocketAddr, path::PathBuf};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, Response, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use komga_core::{Book, Library, Media, scan_library_files};
use komga_media::analyze_book;
use komga_server::{AppState, SESSION_COOKIE, build_app};
use komga_storage::{Storage, TaskStorage};
use serde_json::Value;
use tower::ServiceExt;
use url::Url;

pub async fn test_app() -> Router {
    app_with_storage(Storage::connect_memory().await.unwrap()).await
}

pub async fn app_with_storage(storage: Storage) -> Router {
    let tasks = TaskStorage::connect_memory().await.unwrap();
    build_app(AppState::new(storage, tasks).await.unwrap())
}

pub async fn app_with_storage_trusting_loopback(storage: Storage) -> Router {
    let tasks = TaskStorage::connect_memory().await.unwrap();
    build_app(
        AppState::new(storage, tasks)
            .await
            .unwrap()
            .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]),
    )
}

pub fn through_trusted_proxy(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41_000))));
    request
}

pub fn basic(email: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{email}:{password}")))
}

pub async fn body_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

pub async fn body_bytes(response: Response<Body>) -> Vec<u8> {
    response.into_body().collect().await.unwrap().to_bytes().to_vec()
}

pub async fn body_frames(response: Response<Body>) -> (usize, Vec<u8>) {
    let mut body = response.into_body();
    let mut frame_count = 0;
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.unwrap().into_data() {
            frame_count += 1;
            bytes.extend_from_slice(&data);
        }
    }
    (frame_count, bytes)
}

pub fn multipart_file(bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "komga-rs-test-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"cover.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

pub async fn claim(app: &Router) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::post("/api/v1/claim")
                .header("X-Komga-Email", "admin@example.org")
                .header("X-Komga-Password", "password")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

pub fn session_cookie(response: &Response<Body>) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{SESSION_COOKIE}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

pub async fn analyzed_media_fixture(
    fixture_path: &str,
    file_name: &str,
) -> (tempfile::TempDir, Storage, TaskStorage, Book, Media) {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../komga/komga/src/test/resources")
        .join(fixture_path);
    fs::copy(fixture, root.join("Series").join(file_name)).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, true, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();
    (temporary, storage, tasks, book, media)
}

pub async fn analyzed_media_bytes_fixture(
    source: &[u8],
    file_name: &str,
) -> (tempfile::TempDir, Storage, TaskStorage, Book, Media) {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series").join(file_name), source).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, true, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();
    (temporary, storage, tasks, book, media)
}
