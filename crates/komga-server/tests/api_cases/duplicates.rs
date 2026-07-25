use super::*;
use komga_core::ScanInterval;

#[tokio::test]
async fn duplicate_books_require_admin_and_follow_spring_paging_contract() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/A.cbz"), [1_u8]).unwrap();
    fs::write(root.join("Series/B.cbz"), [2_u8]).unwrap();
    fs::write(root.join("Series/C.cbz"), [3_u8, 3]).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let mut scan = scan_library_files(&library, || false).unwrap();
    for book in &mut scan.series[0].books {
        book.file_hash = "shared".into();
    }
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let app = app_with_storage(storage).await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/books/duplicates").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let reader = app
        .clone()
        .oneshot(
            Request::post("/api/v2/users")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"email":"reader@example.org","password":"reader-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reader.status(), StatusCode::CREATED);
    let forbidden = app
        .clone()
        .oneshot(
            Request::get("/api/v1/books/duplicates")
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
            Request::get("/api/v1/books/duplicates?page=bad&size=1&sort=name,desc&unknown=value")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_json(response).await;
    assert_eq!(page["totalElements"], 2);
    assert_eq!(page["number"], 0);
    assert_eq!(page["size"], 1);
    assert_eq!(page["content"][0]["name"], "B");
    assert_eq!(page["sort"]["sorted"], true);
    assert!(page["content"][0]["url"].as_str().unwrap().contains("/Series/B.cbz"));

    let unknown_sort = app
        .clone()
        .oneshot(
            Request::get("/api/v1/books/duplicates?sort=unknown,asc")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let unknown_sort = body_json(unknown_sort).await;
    assert_eq!(unknown_sort["sort"]["sorted"], false);

    let malformed_sort = app
        .clone()
        .oneshot(
            Request::get("/api/v1/books/duplicates?sort=name,sideways&size=9999")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let malformed_sort = body_json(malformed_sort).await;
    assert_eq!(malformed_sort["size"], 2000);
    assert_eq!(malformed_sort["sort"]["sorted"], true);

    let unpaged = app
        .clone()
        .oneshot(
            Request::get("/api/v1/books/duplicates?unpaged=on&page=-2&size=0")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let unpaged = body_json(unpaged).await;
    assert_eq!(unpaged["number"], 0);
    assert_eq!(unpaged["size"], 20);
    assert_eq!(unpaged["numberOfElements"], 2);

    let invalid_boolean = app
        .oneshot(
            Request::get("/api/v1/books/duplicates?unpaged=maybe")
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_boolean.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn scanned_identical_files_become_duplicate_books_after_background_hashing() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/A.cbz"), b"hello").unwrap();
    fs::write(root.join("Series/B.cbz"), b"hello").unwrap();
    let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    tasks
        .save(&komga_storage::TaskRecord::scan_library(&library.id, false, 8))
        .await
        .unwrap();
    let state = AppState::new(storage.clone(), tasks.clone()).await.unwrap();
    let app = build_app(state.clone());
    claim(&app).await;
    let runtime = state.start_runtime().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let books = storage.list_active_books_by_library_id(&library.id).await.unwrap();
            if books.len() == 2
                && books
                    .iter()
                    .all(|book| book.file_hash == "b5e9c1ad071b3e7fc779cfaa5e523818")
                && tasks.list().await.unwrap().is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background hashing did not complete");

    let response = app
        .oneshot(
            Request::get("/api/v1/books/duplicates")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_json(response).await;
    assert_eq!(page["totalElements"], 2);
    assert_eq!(page["content"][0]["fileHash"], "b5e9c1ad071b3e7fc779cfaa5e523818");
    assert_eq!(page["content"][1]["fileHash"], "b5e9c1ad071b3e7fc779cfaa5e523818");
    runtime.shutdown().await;
}
