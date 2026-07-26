use super::*;

#[tokio::test]
async fn book_import_enforces_admin_and_enqueues_kotlin_tasks_best_effort() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks.clone()).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    storage
        .create_user(&User::new(
            "reader@example.org".into(),
            bcrypt::hash("reader-password", 4).unwrap(),
        ))
        .await
        .unwrap();
    let body = serde_json::json!({
        "books": [
            {"sourceFile":"/imports/a.cbz","seriesId":"series","destinationName":"A"},
            {"sourceFile":"/imports/a.cbz","seriesId":"series","upgradeBookId":"old"},
            {"sourceFile":"/imports/b.cbz","seriesId":"series"}
        ],
        "copyMode":"COPY"
    })
    .to_string();

    let unauthenticated = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/import")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let forbidden = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let accepted = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert!(body_bytes(accepted).await.is_empty());

    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 2);
    let first = queued
        .iter()
        .find(|task| task.id == "IMPORT_BOOK_series_/imports/a.cbz")
        .unwrap();
    assert_eq!(first.priority, 8);
    assert_eq!(first.group_id.as_deref(), Some("series"));
    assert_eq!(first.simple_type, "ImportBook");
    let payload: serde_json::Value = serde_json::from_str(&first.payload).unwrap();
    assert_eq!(payload["upgradeBookId"], "old");
    assert_eq!(payload["destinationName"], serde_json::Value::Null);

    let empty = app
        .oneshot(
            Request::post("/api/v1/books/import")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::from(r#"{"copyMode":"MOVE"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::ACCEPTED);
}
