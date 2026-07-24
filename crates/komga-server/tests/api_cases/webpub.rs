use super::*;

#[tokio::test]
async fn divina_webpub_manifest_matches_kotlin_contract() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume 1.cbz").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header("forwarded", "for=192.0.2.10;proto=https;host=reader.example")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/divina+json");
    assert!(!manifest.headers().contains_key(header::LAST_MODIFIED));
    let manifest = body_json(manifest).await;
    assert_eq!(
        manifest["context"],
        "https://readium.org/webpub-manifest/context.jsonld"
    );
    assert_eq!(manifest["metadata"]["title"], "Volume 1");
    assert_eq!(
        manifest["metadata"]["conformsTo"],
        "https://readium.org/webpub-manifest/profiles/divina"
    );
    assert_eq!(manifest["metadata"]["numberOfPages"], 1);
    assert_eq!(manifest["metadata"]["belongsTo"]["series"][0]["name"], "Series");
    assert_eq!(
        manifest["links"][0],
        serde_json::json!({
            "rel": "self",
            "href": format!("https://reader.example/komga/api/v1/books/{}/manifest", book.id),
            "type": "application/divina+json"
        })
    );
    assert_eq!(manifest["links"][1]["rel"], "http://opds-spec.org/acquisition");
    assert_eq!(manifest["links"][1]["type"], "application/vnd.comicbook+zip");
    assert_eq!(
        manifest["readingOrder"][0]["href"],
        format!(
            "https://reader.example/komga/api/v1/books/{}/pages/1?contentNegotiation=false",
            book.id
        )
    );
    assert_eq!(manifest["readingOrder"][0]["type"], "image/png");
    assert_eq!(manifest["readingOrder"][0]["width"], 48);
    assert_eq!(manifest["resources"][0]["type"], "image/jpeg");
    assert_eq!(manifest["images"], serde_json::json!([]));
    assert_eq!(manifest["toc"], serde_json::json!([]));

    let mismatch = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/pdf", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn divina_manifest_advertises_jpeg_alternate_for_convertible_pages() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let archive_path = root.join("Series/Volume.cbz");
    let mut writer = ZipWriter::new(File::create(&archive_path).unwrap());
    writer.start_file("page.webp", SimpleFileOptions::default()).unwrap();
    writer
        .write_all(
            &fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../komga/komga/src/test/resources/hashpage/e-sou.webp/1.webp"),
            )
            .unwrap(),
        )
        .unwrap();
    writer.finish().unwrap();

    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, true, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();

    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let manifest = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    let manifest = body_json(manifest).await;
    assert_eq!(manifest["readingOrder"][0]["type"], "image/webp");
    assert_eq!(manifest["readingOrder"][0]["alternate"][0]["type"], "image/jpeg");
    assert!(
        manifest["readingOrder"][0]["alternate"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("?contentNegotiation=false&convert=jpeg")
    );
}

#[tokio::test]
async fn pdf_webpub_and_divina_manifests_match_kotlin_contract() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("pdf/komga.pdf", "Volume.pdf").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example:25600")
                .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2099 07:28:00 GMT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/webpub+json");
    assert!(!manifest.headers().contains_key(header::LAST_MODIFIED));
    let manifest = body_json(manifest).await;
    assert_eq!(
        manifest["metadata"]["conformsTo"],
        "https://readium.org/webpub-manifest/profiles/pdf"
    );
    assert_eq!(manifest["readingOrder"].as_array().unwrap().len(), 1);
    assert_eq!(
        manifest["readingOrder"][0]["href"],
        format!("http://reader.example:25600/api/v1/books/{}/pages/1/raw", book.id)
    );
    assert_eq!(manifest["readingOrder"][0]["type"], "application/pdf");
    assert_eq!(manifest["links"][1]["type"], "application/divina+json");
    assert!(
        manifest["links"][1]["href"]
            .as_str()
            .unwrap()
            .ends_with("/manifest/divina")
    );
    assert_eq!(manifest["links"][2]["type"], "application/pdf");

    let divina = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/divina", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(divina.status(), StatusCode::OK);
    assert_eq!(divina.headers()[header::CONTENT_TYPE], "application/divina+json");
    let divina = body_json(divina).await;
    assert_eq!(divina["readingOrder"][0]["type"], "image/jpeg");
    assert_eq!(divina["readingOrder"][0]["width"], 3200);

    let mismatch = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/epub", book.id))
                .header(header::AUTHORIZATION, admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn epub_webpub_manifest_preserves_spine_navigation_and_resources() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/epub3.epub", "Volume.epub").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/epub", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/webpub+json");
    let manifest = body_json(manifest).await;
    assert_eq!(
        manifest["metadata"]["conformsTo"],
        "https://readium.org/webpub-manifest/profiles/epub"
    );
    assert_eq!(manifest["metadata"]["rendition"]["layout"], "fixed");
    assert!(
        manifest["readingOrder"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/titlepage.xhtml")
    );
    assert!(
        manifest["readingOrder"][1]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/page_1.xhtml")
    );
    assert_eq!(manifest["readingOrder"][0]["type"], "application/xhtml+xml");
    assert_eq!(manifest["resources"][0]["type"], "image/jpeg");
    assert!(
        manifest["resources"][1]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/page_styles.css")
    );
    assert!(
        manifest["toc"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/page_1.xhtml")
    );
    assert_eq!(manifest["toc"][0]["title"], "Page 1");
    assert!(
        manifest["landmarks"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/titlepage.xhtml")
    );
    assert_eq!(manifest["links"][1]["type"], "application/divina+json");
    assert_eq!(manifest["links"][2]["type"], "application/epub+zip");

    let generic = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generic.status(), StatusCode::OK);
    assert_eq!(generic.headers()[header::CONTENT_TYPE], "application/webpub+json");
}

#[tokio::test]
async fn epub_pages_resources_positions_and_cache_match_contract() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/epub3.epub", "Volume.epub").await;
    assert!(media.epub_divina_compatible);
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let unauthorized_resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/nav.xhtml", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized_resource.status(), StatusCode::UNAUTHORIZED);

    let resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/nav.xhtml", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resource.status(), StatusCode::OK);
    assert_eq!(resource.headers()[header::CONTENT_TYPE], "application/xhtml+xml");
    assert_eq!(
        resource.headers()[header::CONTENT_SECURITY_POLICY],
        "script-src 'none'; object-src 'none';"
    );
    assert!(
        resource.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("nav.xhtml")
    );
    let last_modified = resource.headers()[header::LAST_MODIFIED].to_str().unwrap().to_owned();
    assert!(
        body_bytes(resource)
            .await
            .windows(10)
            .any(|bytes| bytes == b"Navigation")
    );

    let cached_resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/nav.xhtml", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached_resource.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        cached_resource.headers()[header::CONTENT_SECURITY_POLICY],
        "script-src 'none'; object-src 'none';"
    );

    let missing_resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/META-INF/container.xml", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_resource.status(), StatusCode::NOT_FOUND);

    let positions = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/positions", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(positions.status(), StatusCode::OK);
    assert_eq!(
        positions.headers()[header::CONTENT_TYPE],
        "application/vnd.readium.position-list+json"
    );
    let positions_last_modified = positions.headers()[header::LAST_MODIFIED].to_str().unwrap().to_owned();
    let positions = body_json(positions).await;
    assert_eq!(positions["total"], 2);
    assert_eq!(positions["positions"][0]["href"], "titlepage.xhtml");
    assert_eq!(positions["positions"][1]["locations"]["totalProgression"], 1.0);

    let cached_positions = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/positions", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_MODIFIED_SINCE, positions_last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached_positions.status(), StatusCode::NOT_MODIFIED);

    let pages = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pages.status(), StatusCode::OK);
    let pages = body_json(pages).await;
    assert_eq!(pages.as_array().unwrap().len(), 2);
    assert_eq!(pages[0]["fileName"], "cover.jpeg");

    let page = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/2", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/png");
    assert!(body_bytes(page).await.starts_with(b"\x89PNG"));
}

#[tokio::test]
async fn epub_fonts_are_anonymous_but_document_resources_require_authentication() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let path = root.join("Series/Fonts.epub");
    let mut writer = ZipWriter::new(File::create(&path).unwrap());
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
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/><item id="font" href="font.ttf" media-type="font/ttf"/></manifest><spine><itemref idref="page"/></spine></package>"#,
        )
        .unwrap();
    writer.start_file("page.xhtml", options).unwrap();
    writer
        .write_all(b"<html><body>reflowable document content</body></html>")
        .unwrap();
    writer.start_file("font.ttf", options).unwrap();
    writer.write_all(b"fixture-font").unwrap();
    writer.finish().unwrap();

    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, false, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();
    let app = app_with_storage(storage).await;

    let font = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/font.ttf", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(font.status(), StatusCode::OK);
    assert_eq!(font.headers()[header::CONTENT_TYPE], "font/ttf");
    assert_eq!(body_bytes(font).await, b"fixture-font");

    let document = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/page.xhtml", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(document.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pdf_pages_render_raw_and_negotiate_like_kotlin() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("pdf/komga.pdf", "Volume.pdf").await;

    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let pages = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pages.status(), StatusCode::OK);
    let pages = body_json(pages).await;
    assert_eq!(pages[0]["fileName"], "1");
    assert_eq!(pages[0]["mediaType"], "image/jpeg");
    assert_eq!(pages[0]["width"], 3200);
    assert_eq!(pages[0]["height"], 3200);
    assert!(pages[0]["sizeBytes"].is_null());

    let rendered = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rendered.status(), StatusCode::OK);
    assert_eq!(rendered.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(rendered).await.starts_with(&[0xff, 0xd8]));

    let negotiated = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1?convert=png", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::ACCEPT, "application/pdf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(negotiated.status(), StatusCode::OK);
    assert_eq!(negotiated.headers()[header::CONTENT_TYPE], "application/pdf");
    assert!(body_bytes(negotiated).await.starts_with(b"%PDF-"));

    let negotiation_disabled = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/v1/books/{}/pages/1?contentNegotiation=false&convert=png",
                book.id
            ))
            .header(header::AUTHORIZATION, &admin)
            .header(header::ACCEPT, "application/pdf")
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(negotiation_disabled.status(), StatusCode::OK);
    assert_eq!(negotiation_disabled.headers()[header::CONTENT_TYPE], "image/png");
    assert!(body_bytes(negotiation_disabled).await.starts_with(b"\x89PNG"));

    let raw = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1/raw", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::OK);
    assert_eq!(raw.headers()[header::CONTENT_TYPE], "application/pdf");
    assert!(
        raw.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .ends_with(".pdf\"")
    );
    assert!(body_bytes(raw).await.starts_with(b"%PDF-"));

    let invalid_raw = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/2/raw", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_raw.status(), StatusCode::BAD_REQUEST);
}
