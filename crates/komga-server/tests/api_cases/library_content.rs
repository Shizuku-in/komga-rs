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
