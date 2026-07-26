use super::*;

#[tokio::test]
async fn source_deletion_endpoints_require_admin_accept_unknown_ids_and_deduplicate() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let series = storage.find_series_by_id(&book.series_id).await.unwrap().unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks.clone()).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);

    let password_hash = bcrypt::hash("member-password", 4).unwrap();
    let member = User::new("member@example.org".into(), password_hash.clone());
    storage.create_user(&member).await.unwrap();
    let mut admin_only = User::new("admin-only@example.org".into(), password_hash);
    admin_only.roles.insert(UserRole::Admin);
    storage.create_user(&admin_only).await.unwrap();

    let book_path = format!("/api/v1/books/{}/file", book.id);
    let unauthorized = app
        .clone()
        .oneshot(Request::delete(&book_path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let forbidden = app
        .clone()
        .oneshot(
            Request::delete(&book_path)
                .header(header::AUTHORIZATION, basic("member@example.org", "member-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    for path in [
        book_path.clone(),
        book_path.clone(),
        "/api/v1/books/missing/file".into(),
        format!("/api/v1/series/{}/file", series.id),
        "/api/v1/series/missing/file".into(),
    ] {
        let accepted = app
            .clone()
            .oneshot(
                Request::delete(path)
                    .header(
                        header::AUTHORIZATION,
                        basic("admin-only@example.org", "member-password"),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        assert!(body_bytes(accepted).await.is_empty());
    }

    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 4);
    assert_eq!(
        queued
            .iter()
            .filter(|task| task.id == format!("DELETE_BOOK_{}", book.id))
            .count(),
        1
    );
    assert!(queued.iter().all(|task| task.priority == 8 && task.group_id.is_none()));

    let download_without_role = app
        .clone()
        .oneshot(
            Request::get(&book_path)
                .header(
                    header::AUTHORIZATION,
                    basic("admin-only@example.org", "member-password"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(download_without_role.status(), StatusCode::FORBIDDEN);

    let existing_download = app
        .oneshot(
            Request::get(&book_path)
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(existing_download.status(), StatusCode::OK);
}
