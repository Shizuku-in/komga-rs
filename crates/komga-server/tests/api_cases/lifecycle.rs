use super::*;

#[tokio::test]
async fn deleting_a_non_empty_library_cascades_database_content_but_preserves_files() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let source = root.join("Series/Volume.cbz");
    fs::write(&source, [1_u8]).unwrap();
    fs::write(root.join("Series/series.json"), b"{}").unwrap();
    let library = Library::new("Delete cascade".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    assert_eq!(storage.list_series_by_library_id(&library.id).await.unwrap().len(), 1);

    let app = app_with_storage(storage.clone()).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(
            Request::delete(format!("/api/v1/libraries/{}", library.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(storage.find_library_by_id(&library.id).await.unwrap().is_none());
    assert!(storage.list_series_by_library_id(&library.id).await.unwrap().is_empty());
    assert!(source.exists());

    let missing = app
        .oneshot(
            Request::delete(format!("/api/v1/libraries/{}", library.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn comicrack_match_requires_admin_and_returns_complete_preview() {
    let storage = Storage::connect_memory().await.unwrap();
    let app = app_with_storage(storage.clone()).await;
    let xml =
        br#"<ReadingList><Name>Unicode list</Name><Books><Book Series="Missing" Number="01"/></Books></ReadingList>"#;
    let (content_type, body) = multipart_file(xml);
    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/v1/readlists/match/comicrack")
                .header(header::CONTENT_TYPE, &content_type)
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let reader = User::new("reader@example.org".into(), bcrypt::hash("reader-password", 4).unwrap());
    storage.create_user(&reader).await.unwrap();
    let forbidden = app
        .clone()
        .oneshot(
            Request::post("/api/v1/readlists/match/comicrack")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .header(header::CONTENT_TYPE, &content_type)
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let missing = app
        .clone()
        .oneshot(
            Request::post("/api/v1/readlists/match/comicrack")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::CONTENT_TYPE, "multipart/form-data; boundary=empty")
                .body(Body::from("--empty--\r\n"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
    let (invalid_type, invalid_body) = multipart_file(b"not XML");
    let invalid = app
        .clone()
        .oneshot(
            Request::post("/api/v1/readlists/match/comicrack")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::CONTENT_TYPE, invalid_type)
                .body(Body::from(invalid_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(body_json(invalid).await["message"], "ERR_1015");
    let response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/readlists/match/comicrack")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "readListMatch": {"name": "Unicode list", "errorCode": ""},
            "requests": [{
                "request": {"series": ["Missing"], "number": "01"},
                "matches": []
            }],
            "errorCode": ""
        })
    );
}

#[tokio::test]
async fn read_list_file_is_a_streamed_stored_zip64_archive() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "source.cbz").await;
    storage
        .create_read_list(
            "list",
            &komga_core::ReadListCreation {
                name: "Reading list".into(),
                summary: String::new(),
                ordered: true,
                book_ids: vec![book.id.clone()],
            },
        )
        .await
        .unwrap();
    storage
        .create_read_list(
            "duplicate",
            &komga_core::ReadListCreation {
                name: "Cafe\u{301}".into(),
                summary: String::new(),
                ordered: true,
                book_ids: vec![book.id.clone()],
            },
        )
        .await
        .unwrap();
    let series = storage.find_series_metadata(&book.series_id).await.unwrap().unwrap();
    let metadata = storage.find_book_metadata(&book.id).await.unwrap().unwrap();
    let matched = storage
        .match_read_list_request(komga_core::ReadListRequest {
            name: "Café".into(),
            books: vec![komga_core::ReadListRequestBook {
                series: vec![series.title.to_ascii_uppercase()],
                number: format!("00{}", metadata.number),
            }],
        })
        .await
        .unwrap();
    assert_eq!(matched.read_list_match.error_code, "ERR_1009");
    assert_eq!(matched.requests[0].matches.len(), 1);
    assert_eq!(matched.requests[0].matches[0].series.series_id, book.series_id);
    assert_eq!(matched.requests[0].matches[0].books[0].book_id, book.id);
    let source_path = book.url.to_file_path().unwrap();
    let source = fs::read(&source_path).unwrap();
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let response = app
        .clone()
        .oneshot(
            Request::get("/api/v1/readlists/list/file")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("Reading_list.zip")
    );
    let (frames, bytes) = body_frames(response).await;
    assert!(frames > 1);
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert_eq!(archive.len(), 1);
    let mut entry = archive.by_index(0).unwrap();
    assert_eq!(entry.name(), "1 - source.cbz");
    assert_eq!(entry.compression(), zip::CompressionMethod::Stored);
    let mut actual = Vec::new();
    entry.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, source);

    fs::remove_file(source_path).unwrap();
    let response = app
        .oneshot(
            Request::get("/api/v1/readlists/list/file")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        ZipArchive::new(Cursor::new(body_bytes(response).await)).unwrap().len(),
        0
    );
}

#[tokio::test]
async fn series_file_is_a_streamed_deflate_zip64_archive() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/source.cbz"), vec![7_u8; 128 * 1024]).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
    let source_path = book.url.to_file_path().unwrap();
    let source = fs::read(&source_path).unwrap();
    let series_metadata = storage.find_series_metadata(&book.series_id).await.unwrap().unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let reader = User::new("reader@example.org".into(), bcrypt::hash("reader-password", 4).unwrap());
    storage.create_user(&reader).await.unwrap();

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{}/file", book.series_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let forbidden = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{}/file", book.series_id))
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{}/file", book.series_id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert!(
        response.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(&format!("{}.zip", series_metadata.title))
    );
    assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
    let bytes = body_bytes(response).await;
    assert!(bytes.windows(4).any(|window| window == [0x50, 0x4b, 0x06, 0x06]));
    assert!(bytes.windows(4).any(|window| window == [0x50, 0x4b, 0x06, 0x07]));
    let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
    assert_eq!(archive.len(), 1);
    let mut entry = archive.by_index(0).unwrap();
    assert_eq!(entry.name(), "source.cbz");
    assert_eq!(entry.compression(), zip::CompressionMethod::Deflated);
    let mut actual = Vec::new();
    entry.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, source);
    drop(entry);
    drop(archive);

    let missing = app
        .clone()
        .oneshot(
            Request::get("/api/v1/series/missing/file")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    fs::remove_file(source_path).unwrap();
    let response = app
        .oneshot(
            Request::get(format!("/api/v1/series/{}/file", book.series_id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        ZipArchive::new(Cursor::new(body_bytes(response).await)).unwrap().len(),
        0
    );
}
