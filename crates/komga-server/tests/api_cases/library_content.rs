use super::*;

#[tokio::test]
async fn library_management_bootstrap_settings_and_tasks_are_compatible() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks.clone()).await.unwrap());
    claim(&app).await;
    let admin = basic("admin@example.org", "password");
    let root = tempfile::tempdir().unwrap();
    let create_body = serde_json::json!({
        "name": "Comics",
        "root": root.path().to_string_lossy(),
        "scanDirectoryExclusions": ["tmp"]
    });
    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v1/libraries")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let library = body_json(created).await;
    let library_id = library["id"].as_str().unwrap();
    assert_eq!(library["scanInterval"], "EVERY_6H");
    assert_eq!(library["root"], root.path().to_string_lossy().as_ref());
    assert_eq!(tasks.list().await.unwrap().len(), 1);

    let patched = app
        .clone()
        .oneshot(
            Request::patch(format!("/api/v1/libraries/{library_id}"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"scanDirectoryExclusions":null,"oneshotsDirectory":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::NO_CONTENT);
    let loaded = storage.find_library_by_id(library_id).await.unwrap().unwrap();
    assert!(loaded.scan_directory_exclusions.is_empty());
    assert_eq!(loaded.oneshots_directory, None);

    let deep_scan = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/libraries/{library_id}/scan?deep=true"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deep_scan.status(), StatusCode::ACCEPTED);
    assert!(
        tasks
            .list()
            .await
            .unwrap()
            .iter()
            .any(|task| { task.id == format!("SCAN_LIBRARY_{library_id}_DEEP_true") && task.priority == 8 })
    );

    let global_patch = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/client-settings/global")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"login.banner":{"value":"hello","allowUnauthorized":true},"private":{"value":"secret","allowUnauthorized":false}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(global_patch.status(), StatusCode::NO_CONTENT);
    let anonymous_settings = app
        .clone()
        .oneshot(
            Request::get("/api/v1/client-settings/global/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let anonymous_settings = body_json(anonymous_settings).await;
    assert!(anonymous_settings.get("login.banner").is_some());
    assert!(anonymous_settings.get("private").is_none());

    let oauth = app
        .clone()
        .oneshot(Request::get("/api/v1/oauth2/providers").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(body_json(oauth).await, serde_json::json!([]));

    let listing = app
        .clone()
        .oneshot(
            Request::post("/api/v1/filesystem")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"path": root.path()}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);

    let reader_created = app
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
    assert_eq!(reader_created.status(), StatusCode::CREATED);
    let reader_libraries = app
        .clone()
        .oneshot(
            Request::get("/api/v1/libraries")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(reader_libraries).await[0]["root"], "");

    let cleared = app
        .clone()
        .oneshot(
            Request::delete("/api/v1/tasks")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(body_json(cleared).await.as_u64().unwrap() >= 2);
    assert!(tasks.list().await.unwrap().is_empty());

    let deleted = app
        .oneshot(
            Request::delete(format!("/api/v1/libraries/{library_id}"))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn enabling_file_hashing_enqueues_missing_books_without_rescanning() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/Book.cbz"), b"hello").unwrap();
    let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    library.hash_files = false;
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let book = storage
        .list_active_books_by_library_id(&library.id)
        .await
        .unwrap()
        .remove(0);
    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    claim(&app).await;

    let response = app
        .oneshot(
            Request::patch(format!("/api/v1/libraries/{}", library.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"hashFiles":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].id, format!("HASH_BOOK_{}", book.id));
    assert_eq!(queued[0].priority, 0);
}

#[tokio::test]
async fn content_queries_page_filter_restrict_and_navigate_books() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let series_directory = root.join("Series");
    fs::create_dir_all(&series_directory).unwrap();
    fs::write(series_directory.join("Volume 1.cbz"), [1_u8]).unwrap();
    fs::write(series_directory.join("Volume 2.cbz"), [1_u8, 2]).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();

    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let series_response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list?page=0&size=1&sort=metadata.titleSort,asc")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(series_response.status(), StatusCode::OK);
    let series_page = body_json(series_response).await;
    assert_eq!(series_page["totalElements"], 1);
    assert_eq!(series_page["totalPages"], 1);
    assert_eq!(series_page["numberOfElements"], 1);
    assert_eq!(series_page["pageable"]["pageSize"], 1);
    assert_eq!(series_page["sort"]["sorted"], true);
    let series_id = series_page["content"][0]["id"].as_str().unwrap().to_owned();
    assert!(series_page["content"][0]["url"].as_str().unwrap().contains("/Series"));

    let books_response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/list?unpaged=true&sort=metadata.numberSort,asc")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "condition": {
                            "allOf": [
                                {"seriesId": {"operator": "is", "value": series_id}},
                                {"mediaStatus": {"operator": "is", "value": "UNKNOWN"}}
                            ]
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(books_response.status(), StatusCode::OK);
    let books_page = body_json(books_response).await;
    assert_eq!(books_page["totalElements"], 2);
    assert_eq!(books_page["size"], 20);
    let first_id = books_page["content"][0]["id"].as_str().unwrap().to_owned();
    let second_id = books_page["content"][1]["id"].as_str().unwrap().to_owned();

    let next = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/next"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(next.status(), StatusCode::OK);
    assert_eq!(body_json(next).await["id"], second_id);
    let previous = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/previous"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(previous.status(), StatusCode::NOT_FOUND);

    for (email, restriction) in [
        ("reader@example.org", serde_json::Value::Null),
        (
            "restricted@example.org",
            serde_json::json!({"age": 12, "restriction": "ALLOW_ONLY"}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v2/users")
                    .header(header::AUTHORIZATION, &admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "email": email,
                            "password": "reader-password",
                            "ageRestriction": restriction
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let reader = basic("reader@example.org", "reader-password");
    let reader_series = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{series_id}"))
                .header(header::AUTHORIZATION, &reader)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reader_series.status(), StatusCode::OK);
    assert_eq!(body_json(reader_series).await["url"], "");
    let reader_book = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}"))
                .header(header::AUTHORIZATION, reader)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reader_book.status(), StatusCode::OK);
    assert_eq!(body_json(reader_book).await["url"], "Volume 1.cbz");

    let restricted = basic("restricted@example.org", "reader-password");
    let restricted_list = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list")
                .header(header::AUTHORIZATION, &restricted)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restricted_list.status(), StatusCode::OK);
    assert_eq!(body_json(restricted_list).await["totalElements"], 0);
    let restricted_detail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{series_id}"))
                .header(header::AUTHORIZATION, &restricted)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restricted_detail.status(), StatusCode::FORBIDDEN);

    let restricted_manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/manifest"))
                .header(header::AUTHORIZATION, restricted)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restricted_manifest.status(), StatusCode::FORBIDDEN);

    let unanalyzed_manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/manifest"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unanalyzed_manifest.status(), StatusCode::NOT_FOUND);

    let searched = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/list")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"fullTextSearch":"volume"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(searched.status(), StatusCode::OK);
    let searched = body_json(searched).await;
    assert!(
        searched["content"]
            .as_array()
            .is_some_and(|content| !content.is_empty())
    );
}

#[tokio::test]
async fn discovery_feeds_groups_and_on_deck_match_modern_contracts() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    for (series, books) in [("Alpha", 2), ("Beta", 1), ("Empty", 1)] {
        let directory = root.join(series);
        fs::create_dir_all(&directory).unwrap();
        for book in 1..=books {
            fs::write(directory.join(format!("Volume {book}.cbz")), [book as u8]).unwrap();
        }
    }
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();

    let mut series = storage.list_series_by_library_id(&library.id).await.unwrap();
    series.sort_by(|left, right| left.name.cmp(&right.name));
    for (item, title_sort) in series.iter().zip(["Alpha", "beta", ""]) {
        let mut metadata = storage.find_series_metadata(&item.id).await.unwrap().unwrap();
        metadata.title = title_sort.to_owned();
        metadata.title_sort = title_sort.to_owned();
        storage.save_series_metadata(&metadata).await.unwrap();
    }

    let app = app_with_storage(storage.clone()).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let groups = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list/alphabetical-groups")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(groups.status(), StatusCode::OK);
    assert_eq!(
        body_json(groups).await,
        serde_json::json!([
            {"group": "", "count": 1},
            {"group": "a", "count": 1},
            {"group": "b", "count": 1}
        ])
    );

    let filtered_groups = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list/alphabetical-groups")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"condition":{"title":{"operator":"beginsWith","value":"alp"}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json(filtered_groups).await,
        serde_json::json!([{"group": "a", "count": 1}])
    );

    let empty_groups = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list/alphabetical-groups")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"fullTextSearch":"definitely-not-present"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(empty_groups).await, serde_json::json!([]));

    for path in [
        "/api/v1/series/latest?page=0&size=1&sort=name,asc&unknown=ignored",
        "/api/v1/series/new?page=0&size=1&sort=name,asc",
        "/api/v1/books/latest?page=0&size=1&sort=name,asc",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(header::AUTHORIZATION, &admin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_json(response).await;
        assert_eq!(page["numberOfElements"], 1);
        assert_eq!(page["sort"]["sorted"], true);
    }

    let unpaged = app
        .clone()
        .oneshot(
            Request::get("/api/v1/series/latest?unpaged=true&library_id=missing&library_id=also-missing")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(unpaged).await["totalElements"], 0);

    let updated = app
        .clone()
        .oneshot(
            Request::get("/api/v1/series/updated")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(updated).await["totalElements"], 0);

    let invalid_boolean = app
        .clone()
        .oneshot(
            Request::get("/api/v1/series/latest?deleted=1")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_boolean.status(), StatusCode::BAD_REQUEST);

    let alpha_books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
    let admin_user = storage.find_user_by_email("admin@example.org").await.unwrap().unwrap();
    storage
        .save_read_progress(&komga_core::ReadProgress {
            book_id: alpha_books[0].id.clone(),
            user_id: admin_user.id,
            page: 1,
            completed: true,
            read_date: chrono::Utc::now().naive_utc(),
            device_id: String::new(),
            device_name: String::new(),
            locator: None,
        })
        .await
        .unwrap();
    let on_deck = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/ondeck?size=1&library_id={}", library.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(on_deck.status(), StatusCode::OK);
    let on_deck = body_json(on_deck).await;
    assert_eq!(on_deck["totalElements"], 1);
    assert_eq!(on_deck["content"][0]["id"], alpha_books[1].id);

    let unauthorized = app
        .oneshot(Request::get("/api/v1/books/latest").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
}
