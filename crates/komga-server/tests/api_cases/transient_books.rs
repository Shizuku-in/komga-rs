use pdf_writer::{Rect, Ref};

use super::*;

const PNG_1X1: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0, 0, 0, 181, 28, 12,
    2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69,
    78, 68, 174, 66, 96, 130,
];

#[tokio::test]
async fn transient_scan_enforces_admin_boundaries_filters_and_stable_paths() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let library_root = temporary.path().join("library");
    fs::create_dir_all(&library_root).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&library_root).unwrap());
    storage.create_library(&library).await.unwrap();

    let import_root = temporary.path().join("imports");
    fs::create_dir_all(import_root.join("nested")).unwrap();
    fs::create_dir_all(import_root.join(".hidden")).unwrap();
    write_cbz(&import_root.join("z.cbz"), None);
    fs::write(import_root.join("nested/a.PDF"), blank_pdf()).unwrap();
    write_cbz(&import_root.join(".hidden/hidden.cbz"), None);
    fs::write(import_root.join("ignored.txt"), b"ignored").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(import_root.join("z.cbz"), import_root.join("linked.cbz")).unwrap();

    let app = app_with_storage(storage.clone()).await;
    let body = serde_json::json!({"path": import_root}).to_string();
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/v1/transient-books")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    storage
        .create_user(&User::new(
            "reader-transient@example.org".into(),
            bcrypt::hash("reader-password", 4).unwrap(),
        ))
        .await
        .unwrap();
    let reader = basic("reader-transient@example.org", "reader-password");
    let forbidden = app
        .clone()
        .oneshot(
            Request::post("/api/v1/transient-books")
                .header(header::AUTHORIZATION, &reader)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/transient-books")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let scanned = body_json(response).await;
    assert_eq!(scanned.as_array().unwrap().len(), 2);
    assert_eq!(scanned[0]["name"], "a");
    assert_eq!(scanned[1]["name"], "z");
    assert_eq!(scanned[0]["status"], "UNKNOWN");
    assert_eq!(scanned[0]["mediaType"], "");
    assert_eq!(scanned[0]["pages"], serde_json::json!([]));
    assert_eq!(scanned[0]["files"], serde_json::json!([]));
    assert_eq!(scanned[0]["comment"], "");
    assert_eq!(
        scanned[0]["url"],
        import_root.join("nested/a.PDF").to_string_lossy().as_ref()
    );
    let id = scanned[0]["id"].as_str().unwrap();
    let forbidden = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/transient-books/{id}/analyze"))
                .header(header::AUTHORIZATION, &reader)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let forbidden = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/transient-books/{id}/pages/1"))
                .header(header::AUTHORIZATION, &reader)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let hidden_root = temporary.path().join(".explicit-import-root");
    fs::create_dir_all(&hidden_root).unwrap();
    write_cbz(&hidden_root.join("visible.cbz"), None);
    let scanned_hidden_root = scan_books(&app, &basic("admin@example.org", "password"), &hidden_root).await;
    assert_eq!(scanned_hidden_root.as_array().unwrap().len(), 1);

    for invalid in [
        library_root.clone(),
        temporary.path().to_path_buf(),
        temporary.path().join("missing"),
        import_root.join("z.cbz"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v1/transient-books")
                    .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"path": invalid}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["message"], "ERR_1017");
    }
}

#[tokio::test]
async fn transient_cbz_analysis_extracts_metadata_matches_series_and_serves_pages() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let library_root = temporary.path().join("library");
    fs::create_dir_all(library_root.join("Batman")).unwrap();
    write_cbz(&library_root.join("Batman/Existing.cbz"), None);
    let library = Library::new("Library".into(), Url::from_directory_path(&library_root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);

    let import_root = temporary.path().join("imports");
    fs::create_dir_all(&import_root).unwrap();
    let source = import_root.join("Candidate.cbz");
    write_cbz(
        &source,
        Some("<ComicInfo><Series>BATMAN</Series><Number>15.5</Number></ComicInfo>"),
    );
    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let scanned = scan_books(&app, &admin, &import_root).await;
    let id = scanned[0]["id"].as_str().unwrap();

    let before_analysis = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/transient-books/{id}/pages/1"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(before_analysis.status(), StatusCode::NOT_FOUND);

    let analyzed = analyze_transient(&app, &admin, id).await;
    assert_eq!(analyzed["status"], "READY");
    assert_eq!(analyzed["mediaType"], "application/zip");
    assert_eq!(analyzed["number"], 15.5);
    assert_eq!(analyzed["seriesId"], series.id);
    assert_eq!(analyzed["pages"][0]["number"], 1);
    assert_eq!(analyzed["pages"][0]["fileName"], "page.png");
    assert_eq!(analyzed["pages"][0]["mediaType"], "image/png");
    assert_eq!(analyzed["pages"][0]["width"], 1);
    assert_eq!(analyzed["pages"][0]["height"], 1);
    assert_eq!(analyzed["files"][0], "ComicInfo.xml");

    let page = get_transient_page(&app, &admin, id, 1).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(body_bytes(page).await, PNG_1X1);
    assert_eq!(
        get_transient_page(&app, &admin, id, 2).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        analyze_transient_response(&app, &admin, "missing").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get_transient_page(&app, &admin, "missing", 1).await.status(),
        StatusCode::NOT_FOUND
    );

    fs::remove_file(source).unwrap();
    assert_eq!(
        get_transient_page(&app, &admin, id, 1).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn transient_pdf_and_epub_analysis_render_preview_pages_and_epub_metadata() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let library_root = temporary.path().join("library");
    fs::create_dir_all(library_root.join("Novel Series")).unwrap();
    write_cbz(&library_root.join("Novel Series/Existing.cbz"), None);
    let library = Library::new("Library".into(), Url::from_directory_path(&library_root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);

    let import_root = temporary.path().join("imports");
    fs::create_dir_all(&import_root).unwrap();
    fs::write(import_root.join("Document.pdf"), blank_pdf()).unwrap();
    write_epub(&import_root.join("Novel.epub"));
    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let scanned = scan_books(&app, &admin, &import_root).await;
    let pdf_id = scanned
        .as_array()
        .unwrap()
        .iter()
        .find(|book| book["name"] == "Document")
        .unwrap()["id"]
        .as_str()
        .unwrap();
    let epub_id = scanned
        .as_array()
        .unwrap()
        .iter()
        .find(|book| book["name"] == "Novel")
        .unwrap()["id"]
        .as_str()
        .unwrap();

    let pdf = analyze_transient(&app, &admin, pdf_id).await;
    assert_eq!(pdf["status"], "READY");
    assert_eq!(pdf["mediaType"], "application/pdf");
    assert_eq!(pdf["pages"][0]["mediaType"], "image/jpeg");
    let page = get_transient_page(&app, &admin, pdf_id, 1).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(page).await.starts_with(&[0xff, 0xd8]));

    let epub = analyze_transient(&app, &admin, epub_id).await;
    assert_eq!(epub["status"], "READY");
    assert_eq!(epub["mediaType"], "application/epub+zip");
    assert_eq!(epub["number"], 3.0);
    assert_eq!(epub["seriesId"], series.id);
    assert_eq!(epub["pages"][0]["mediaType"], "image/png");
    let page = get_transient_page(&app, &admin, epub_id, 1).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(body_bytes(page).await, PNG_1X1);
}

async fn scan_books(app: &axum::Router, admin: &str, path: &std::path::Path) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/transient-books")
                .header(header::AUTHORIZATION, admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"path": path}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn analyze_transient(app: &axum::Router, admin: &str, id: &str) -> serde_json::Value {
    let response = analyze_transient_response(app, admin, id).await;
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

async fn analyze_transient_response(app: &axum::Router, admin: &str, id: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::post(format!("/api/v1/transient-books/{id}/analyze"))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_transient_page(app: &axum::Router, admin: &str, id: &str, page: i32) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::get(format!("/api/v1/transient-books/{id}/pages/{page}"))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn write_cbz(path: &std::path::Path, comic_info: Option<&str>) {
    let mut writer = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default();
    writer.start_file("page.png", options).unwrap();
    writer.write_all(PNG_1X1).unwrap();
    if let Some(comic_info) = comic_info {
        writer.start_file("ComicInfo.xml", options).unwrap();
        writer.write_all(comic_info.as_bytes()).unwrap();
    }
    writer.finish().unwrap();
}

fn blank_pdf() -> Vec<u8> {
    let mut pdf = pdf_writer::Pdf::new();
    let catalog = Ref::new(1);
    let pages = Ref::new(2);
    let page = Ref::new(3);
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    let mut output_page = pdf.page(page);
    output_page.parent(pages);
    output_page.media_box(Rect::new(0.0, 0.0, 200.0, 300.0));
    output_page.resources();
    drop(output_page);
    pdf.finish()
}

fn write_epub(path: &std::path::Path) {
    let mut writer = ZipWriter::new(File::create(path).unwrap());
    let options = SimpleFileOptions::default();
    writer.start_file("mimetype", options).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();
    writer.start_file("META-INF/container.xml", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        )
        .unwrap();
    writer.start_file("content.opf", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" version="3.0"><metadata><dc:title>Novel</dc:title><meta name="calibre:series" content="NOVEL SERIES"/><meta name="calibre:series_index" content="3"/><meta property="rendition:layout">pre-paginated</meta></metadata><manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/><item id="image" href="image.png" media-type="image/png"/></manifest><spine><itemref idref="page"/></spine></package>"#,
        )
        .unwrap();
    writer.start_file("page.xhtml", options).unwrap();
    writer
        .write_all(br#"<html><body><img src="image.png"/></body></html>"#)
        .unwrap();
    writer.start_file("image.png", options).unwrap();
    writer.write_all(PNG_1X1).unwrap();
    writer.finish().unwrap();
}
