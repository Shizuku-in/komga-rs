use super::*;

#[tokio::test]
async fn task_queue_sse_requires_authentication_and_reports_admin_counts() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    tasks
        .save(&komga_storage::TaskRecord::scan_library("library", false, 4))
        .await
        .unwrap();
    tasks
        .save(&komga_storage::TaskRecord::empty_trash("library", 6))
        .await
        .unwrap();
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    claim(&app).await;

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/sse/v1/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::get("/sse/v1/events")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("SSE did not produce an initial task status")
        .unwrap()
        .unwrap();
    let event = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(event.contains("event: TaskQueueStatus"));
    assert!(event.contains(r#""count":2"#));
    assert!(event.contains(r#""EmptyTrash":1"#));
    assert!(event.contains(r#""ScanLibrary":1"#));
}
