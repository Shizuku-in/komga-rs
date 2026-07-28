use super::*;

#[tokio::test]
async fn actuator_info_is_admin_only_and_exposes_webui_build_fields() {
    let storage = Storage::connect_memory().await.unwrap();
    let app = app_with_storage(storage.clone()).await;

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/actuator/info").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    storage
        .create_user(&User::new(
            "reader-info@example.org".into(),
            bcrypt::hash("reader-password", 4).unwrap(),
        ))
        .await
        .unwrap();
    let forbidden = app
        .clone()
        .oneshot(
            Request::get("/actuator/info")
                .header(
                    header::AUTHORIZATION,
                    basic("reader-info@example.org", "reader-password"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .oneshot(
            Request::get("/actuator/info")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let info = body_json(response).await;
    assert!(
        info["git"]["commit"]["id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        info["git"]["commit"]["time"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(info["git"]["branch"].as_str().is_some_and(|value| !value.is_empty()));
    assert_eq!(info["build"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["build"]["artifact"], "komga");
    assert_eq!(info["build"]["name"], "komga");
    assert_eq!(info["build"]["group"], "org.gotson");
    assert!(info["build"]["time"].as_str().is_some_and(|value| !value.is_empty()));
}

#[tokio::test]
async fn actuator_logfile_is_admin_only_and_streams_the_configured_file() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let log_file = temporary.path().join("logs/komga.log");
    fs::create_dir_all(log_file.parent().unwrap()).unwrap();
    let log_contents = b"startup complete\nrequest handled\n";
    fs::write(&log_file, log_contents).unwrap();
    let app = build_app(
        AppState::new(storage.clone(), tasks)
            .await
            .unwrap()
            .with_log_file(log_file.clone()),
    );

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/actuator/logfile").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    storage
        .create_user(&User::new(
            "reader-logfile@example.org".into(),
            bcrypt::hash("reader-password", 4).unwrap(),
        ))
        .await
        .unwrap();
    let forbidden = app
        .clone()
        .oneshot(
            Request::get("/actuator/logfile")
                .header(
                    header::AUTHORIZATION,
                    basic("reader-logfile@example.org", "reader-password"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .oneshot(
            Request::get("/actuator/logfile")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/plain;charset=UTF-8");
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"komga.log\""
    );
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        log_contents.len().to_string()
    );
    let mut writer = fs::OpenOptions::new().append(true).open(&log_file).unwrap();
    std::io::Write::write_all(&mut writer, b"written after response headers\n").unwrap();
    assert_eq!(body_bytes(response).await, log_contents);

    let missing = build_app(
        AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
            .await
            .unwrap()
            .with_log_file(temporary.path().join("logs/missing.log")),
    )
    .oneshot(
        Request::get("/actuator/logfile")
            .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn actuator_shutdown_is_admin_only_returns_before_requesting_graceful_shutdown() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let state = AppState::new(storage.clone(), tasks).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    let shutdown_requested = runtime.shutdown_handle();
    let app = build_app(state);

    let unauthorized = app
        .clone()
        .oneshot(Request::post("/actuator/shutdown").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    storage
        .create_user(&User::new(
            "reader-shutdown@example.org".into(),
            bcrypt::hash("reader-password", 4).unwrap(),
        ))
        .await
        .unwrap();
    let forbidden = app
        .clone()
        .oneshot(
            Request::post("/actuator/shutdown")
                .header(
                    header::AUTHORIZATION,
                    basic("reader-shutdown@example.org", "reader-password"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .oneshot(
            Request::post("/actuator/shutdown")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["message"], "Shutting down, bye...");
    tokio::time::timeout(Duration::from_secs(1), shutdown_requested.cancelled_owned())
        .await
        .expect("shutdown request should reach the runtime");

    runtime.shutdown().await;
}

#[tokio::test]
async fn library_and_series_analysis_queue_all_books_at_priority_six() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/One.cbz"), b"one").unwrap();
    fs::write(root.join("Series/Two.cbz"), b"two").unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    fs::remove_file(root.join("Series/Two.cbz")).unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), true)
        .await
        .unwrap();
    let books = storage.list_books_by_series_id(&series.id).await.unwrap();
    assert_eq!(books.iter().filter(|book| book.deleted_date.is_some()).count(), 1);

    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    for path in [
        format!("/api/v1/libraries/{}/analyze", library.id),
        format!("/api/v1/libraries/{}/analyze", library.id),
        "/api/v1/libraries/missing/analyze".into(),
        format!("/api/v1/series/{}/analyze", series.id),
        "/api/v1/series/missing/analyze".into(),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header(header::AUTHORIZATION, &admin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), books.len());
    for task in queued {
        assert_eq!(task.simple_type, "AnalyzeBook");
        assert_eq!(task.group_id.as_deref(), Some(series.id.as_str()));
        assert_eq!(task.priority, 6);
    }
}

#[tokio::test]
async fn bulk_analysis_requires_an_administrator() {
    let app = test_app().await;
    let response = app
        .oneshot(
            Request::post("/api/v1/libraries/missing/analyze")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
