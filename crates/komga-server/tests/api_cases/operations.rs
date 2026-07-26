use super::*;

#[tokio::test]
async fn announcements_and_releases_require_admin_and_read_updates_are_persisted() {
    let storage = Storage::connect_memory().await.unwrap();
    let app = app_with_storage(storage.clone()).await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

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
    let reader = basic("reader@example.org", "reader-password");

    for path in ["/api/v1/announcements", "/api/v1/releases"] {
        let unauthorized = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let forbidden = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(header::AUTHORIZATION, &reader)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    }

    let forbidden = app
        .clone()
        .oneshot(
            Request::put("/api/v1/announcements")
                .header(header::AUTHORIZATION, &reader)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"["one"]"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let marked = app
        .clone()
        .oneshot(
            Request::put("/api/v1/announcements")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"["one","two","one"]"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(marked.status(), StatusCode::NO_CONTENT);
    let admin_user = storage.find_user_by_email("admin@example.org").await.unwrap().unwrap();
    assert_eq!(
        storage.announcement_ids_read(&admin_user.id).await.unwrap(),
        std::collections::BTreeSet::from(["one".into(), "two".into()])
    );
}
