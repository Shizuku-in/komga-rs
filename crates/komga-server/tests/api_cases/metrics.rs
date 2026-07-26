use komga_core::{CollectionCreation, ReadListCreation};

use super::*;

#[tokio::test]
async fn application_metrics_are_admin_only_grouped_and_filterable() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    fs::write(root.join("Series/One.cbz"), [1_u8, 2, 3]).unwrap();
    fs::write(root.join("Series/Two.cbz"), [4_u8, 5, 6, 7, 8]).unwrap();
    fs::write(root.join("Series/series.json"), b"{}").unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let books = storage.list_books_by_series_id(&series[0].id).await.unwrap();
    storage
        .create_collection(
            "collection",
            &CollectionCreation {
                name: "Collection".into(),
                ordered: false,
                series_ids: vec![series[0].id.clone()],
            },
        )
        .await
        .unwrap();
    storage
        .create_read_list(
            "readlist",
            &ReadListCreation {
                name: "Read list".into(),
                summary: String::new(),
                ordered: true,
                book_ids: vec![books[0].id.clone()],
            },
        )
        .await
        .unwrap();

    let app = app_with_storage(storage).await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/actuator/metrics").body(Body::empty()).unwrap())
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
            Request::get("/actuator/metrics/komga.books")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let names = app
        .clone()
        .oneshot(
            Request::get("/actuator/metrics")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(names.status(), StatusCode::OK);
    let names = body_json(names).await;
    assert_eq!(names["names"].as_array().unwrap().len(), 9);
    assert!(
        names["names"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("komga.tasks.execution"))
    );

    let books = metric(&app, "/actuator/metrics/komga.books", &admin).await;
    assert_eq!(
        books["measurements"][0],
        serde_json::json!({"statistic":"VALUE","value":2.0})
    );
    assert_eq!(books["availableTags"][0]["tag"], "library");
    assert_eq!(books["availableTags"][0]["values"], serde_json::json!([library.id]));

    let filtered = metric(
        &app,
        &format!("/actuator/metrics/komga.books?tag=library%3A{}", library.id),
        &admin,
    )
    .await;
    assert_eq!(filtered["measurements"][0]["value"], 2.0);
    assert!(filtered["availableTags"].as_array().unwrap().is_empty());
    assert_eq!(
        metric(&app, "/actuator/metrics/komga.books.filesize", &admin).await["measurements"][0]["value"],
        8.0
    );
    assert_eq!(
        metric(&app, "/actuator/metrics/komga.sidecars", &admin).await["measurements"][0]["value"],
        1.0
    );
    assert_eq!(
        metric(&app, "/actuator/metrics/komga.collections", &admin).await["measurements"][0]["value"],
        1.0
    );
    assert_eq!(
        metric(&app, "/actuator/metrics/komga.readlists", &admin).await["measurements"][0]["value"],
        1.0
    );

    for (path, status) in [
        ("/actuator/metrics/missing", StatusCode::NOT_FOUND),
        ("/actuator/metrics/komga.books?tag=bad", StatusCode::BAD_REQUEST),
        (
            "/actuator/metrics/komga.books?tag=library%3Amissing",
            StatusCode::NOT_FOUND,
        ),
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
        assert_eq!(response.status(), status, "{path}");
    }
}

#[tokio::test]
async fn completed_worker_tasks_are_exposed_as_process_local_timers() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    tasks
        .save(&komga_storage::TaskRecord::empty_trash("missing", 8))
        .await
        .unwrap();
    let state = AppState::new(storage, tasks.clone()).await.unwrap();
    let runtime = state.start_runtime().await.unwrap();
    let app = build_app(state);
    claim(&app).await;

    tokio::time::timeout(Duration::from_secs(5), async {
        while !tasks.list().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let metrics = metric(
        &app,
        "/actuator/metrics/komga.tasks.execution?tag=type%3AEmptyTrash",
        &basic("admin@example.org", "password"),
    )
    .await;
    assert_eq!(metrics["measurements"][0]["statistic"], "COUNT");
    assert_eq!(metrics["measurements"][0]["value"], 1.0);
    assert_eq!(metrics["measurements"][1]["statistic"], "TOTAL_TIME");
    assert!(metrics["measurements"][1]["value"].as_f64().unwrap() >= 0.0);

    runtime.shutdown().await;
}

async fn metric(app: &axum::Router, path: &str, authorization: &str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(path)
                .header(header::AUTHORIZATION, authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    body_json(response).await
}
