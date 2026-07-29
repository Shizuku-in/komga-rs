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

pub fn minimal_mobi(html: &[u8], encrypted: bool) -> Vec<u8> {
    let title = "MOBI HTTP 测试".as_bytes();
    let mut exth = Vec::new();
    exth.extend_from_slice(b"EXTH");
    exth.extend_from_slice(&0_u32.to_be_bytes());
    exth.extend_from_slice(&1_u32.to_be_bytes());
    exth.extend_from_slice(&503_u32.to_be_bytes());
    exth.extend_from_slice(&(u32::try_from(title.len() + 8).unwrap()).to_be_bytes());
    exth.extend_from_slice(title);
    let exth_len = u32::try_from(exth.len()).unwrap();
    exth[4..8].copy_from_slice(&exth_len.to_be_bytes());

    let exth_start = 248_usize;
    let title_offset = exth_start + 20;
    let mut record_zero = vec![0_u8; exth_start + exth.len()];
    record_zero[0..2].copy_from_slice(&1_u16.to_be_bytes());
    record_zero[4..8].copy_from_slice(&(u32::try_from(html.len()).unwrap()).to_be_bytes());
    record_zero[8..10].copy_from_slice(&1_u16.to_be_bytes());
    record_zero[10..12].copy_from_slice(&4096_u16.to_be_bytes());
    record_zero[12..14].copy_from_slice(&(u16::from(encrypted)).to_be_bytes());
    record_zero[16..20].copy_from_slice(b"MOBI");
    record_zero[20..24].copy_from_slice(&232_u32.to_be_bytes());
    record_zero[24..28].copy_from_slice(&2_u32.to_be_bytes());
    record_zero[28..32].copy_from_slice(&65001_u32.to_be_bytes());
    record_zero[0x54..0x58].copy_from_slice(&(u32::try_from(title_offset).unwrap()).to_be_bytes());
    record_zero[0x58..0x5c].copy_from_slice(&(u32::try_from(title.len()).unwrap()).to_be_bytes());
    record_zero[0x68..0x6c].copy_from_slice(&6_u32.to_be_bytes());
    for offset in [0x6c, 0x70, 0x74, 0xf4] {
        record_zero[offset..offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
    }
    record_zero[0x80..0x84].copy_from_slice(&0x40_u32.to_be_bytes());
    record_zero[exth_start..].copy_from_slice(&exth);

    let record_count = 2_u16;
    let header_size = 78 + usize::from(record_count) * 8 + 2;
    let record_one_offset = header_size + record_zero.len();
    let mut pdb = vec![0_u8; header_size];
    pdb[..9].copy_from_slice(b"MOBI test");
    pdb[60..68].copy_from_slice(b"BOOKMOBI");
    pdb[76..78].copy_from_slice(&record_count.to_be_bytes());
    pdb[78..82].copy_from_slice(&(u32::try_from(header_size).unwrap()).to_be_bytes());
    pdb[86..90].copy_from_slice(&(u32::try_from(record_one_offset).unwrap()).to_be_bytes());
    pdb.extend_from_slice(&record_zero);
    pdb.extend_from_slice(html);
    pdb
}
