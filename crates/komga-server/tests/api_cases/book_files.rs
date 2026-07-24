use super::*;

#[tokio::test]
async fn book_file_download_streams_source_bytes_with_kotlin_headers_and_alias() {
    let (_temporary, storage, tasks, book, _media) =
        analyzed_media_fixture("archives/zip.zip", "アキラ 大判.cbz").await;
    let source_path = book.url.to_file_path().unwrap();
    let source = (0..(2 * 1024 * 1024 + 127))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(&source_path, &source).unwrap();

    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let credentials = basic("admin@example.org", "password");
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file", book.id))
                .header(header::AUTHORIZATION, &credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], source.len().to_string());
    let expected_disposition = concat!(
        "attachment; filename=\"=?UTF-8?Q?=E3=82=A2=E3=82=AD=E3=83=A9_=E5=A4=A7=E5=88=A4.cbz?=\"; ",
        "filename*=UTF-8''%E3%82%A2%E3%82%AD%E3%83%A9%20%E5%A4%A7%E5%88%A4.cbz"
    );
    assert_eq!(response.headers()[header::CONTENT_DISPOSITION], expected_disposition);
    let (frame_count, downloaded) = body_frames(response).await;
    assert!(
        frame_count > 1,
        "large source file should be emitted in multiple frames"
    );
    assert_eq!(downloaded, source);

    let alias = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file/client/supplied.cbz", book.id))
                .header(header::AUTHORIZATION, credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alias.status(), StatusCode::OK);
    assert_eq!(alias.headers()[header::CONTENT_DISPOSITION], expected_disposition);
    assert_eq!(body_bytes(alias).await, source);
}

#[tokio::test]
async fn book_file_download_uses_analyzed_media_types_and_unknown_fallback() {
    for (fixture, file_name, expected_media_type) in [
        ("pdf/komga.pdf", "Volume.pdf", "application/pdf"),
        ("archives/epub3.epub", "Volume.epub", "application/epub+zip"),
    ] {
        let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture(fixture, file_name).await;
        let app = build_app(AppState::new(storage, tasks).await.unwrap());
        assert_eq!(claim(&app).await.status(), StatusCode::OK);
        let response = app
            .oneshot(
                Request::get(format!("/api/v1/books/{}/file", book.id))
                    .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], expected_media_type);
        assert!(!body_bytes(response).await.is_empty());
    }

    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let series_directory = root.join("Series");
    fs::create_dir_all(&series_directory).unwrap();
    let source = b"not analyzed yet";
    fs::write(series_directory.join("Volume.cbz"), source).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(body_bytes(response).await, source);
}

#[tokio::test]
async fn book_file_download_enforces_role_library_age_and_label_restrictions() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    storage
        .create_read_list(
            "restricted-list",
            &komga_core::ReadListCreation {
                name: "Restricted list".into(),
                summary: String::new(),
                ordered: true,
                book_ids: vec![book.id.clone()],
            },
        )
        .await
        .unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let password_hash = bcrypt::hash("reader-password", 4).unwrap();

    let no_role = User::new("no-role@example.org".into(), password_hash.clone());
    storage.create_user(&no_role).await.unwrap();

    let mut no_library = User::new("no-library@example.org".into(), password_hash.clone());
    no_library.roles.insert(UserRole::FileDownload);
    no_library.shared_all_libraries = false;
    storage.create_user(&no_library).await.unwrap();

    let mut age_restricted = User::new("age@example.org".into(), password_hash.clone());
    age_restricted.roles.insert(UserRole::FileDownload);
    age_restricted.restrictions = ContentRestrictions::new(
        Some(AgeRestriction {
            age: 12,
            restriction: AllowExclude::AllowOnly,
        }),
        Vec::<String>::new(),
        Vec::<String>::new(),
    );
    storage.create_user(&age_restricted).await.unwrap();

    let mut label_restricted = User::new("label@example.org".into(), password_hash);
    label_restricted.roles.insert(UserRole::FileDownload);
    label_restricted.restrictions = ContentRestrictions::new(None, ["kids".to_owned()], Vec::<String>::new());
    storage.create_user(&label_restricted).await.unwrap();

    let path = format!("/api/v1/books/{}/file", book.id);
    assert_eq!(
        app.clone()
            .oneshot(Request::get(path.as_str()).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for email in [
        "no-role@example.org",
        "no-library@example.org",
        "age@example.org",
        "label@example.org",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path.as_str())
                    .header(header::AUTHORIZATION, basic(email, "reader-password"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "unexpected status for {email}"
        );
    }

    let read_list_path = "/api/v1/readlists/restricted-list/file";
    let no_role_response = app
        .clone()
        .oneshot(
            Request::get(read_list_path)
                .header(header::AUTHORIZATION, basic("no-role@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_role_response.status(), StatusCode::FORBIDDEN);
    for email in ["no-library@example.org", "age@example.org", "label@example.org"] {
        let response = app
            .clone()
            .oneshot(
                Request::get(read_list_path)
                    .header(header::AUTHORIZATION, basic(email, "reader-password"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "unexpected ZIP status for {email}"
        );
    }
}

#[tokio::test]
async fn book_file_download_reports_unknown_and_moved_files() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let source_path = book.url.to_file_path().unwrap();
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let credentials = basic("admin@example.org", "password");

    let unknown = app
        .clone()
        .oneshot(
            Request::get("/api/v1/books/missing/file")
                .header(header::AUTHORIZATION, &credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await["message"], "Book not found");

    fs::remove_file(source_path).unwrap();
    let moved = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file", book.id))
                .header(header::AUTHORIZATION, credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(moved.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(moved).await["message"], "File not found, it may have moved");
}
