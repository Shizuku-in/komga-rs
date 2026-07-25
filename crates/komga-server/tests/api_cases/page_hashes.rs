use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use komga_media::{extract_page, hash_page};
use komga_server::{AppState, build_app};
use komga_storage::PageHashUpdate;
use tower::ServiceExt;

use crate::support::{analyzed_media_fixture, basic, body_bytes, body_json, claim, test_app};

#[tokio::test]
async fn page_hash_administration_enforces_admin_and_rule_contract() {
    let app = test_app().await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/page-hashes").body(Body::empty()).unwrap())
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
            Request::get("/api/v1/page-hashes")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let created = app
        .clone()
        .oneshot(
            Request::put("/api/v1/page-hashes")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"hash":"test","size":-1,"action":"DELETE_MANUAL"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::ACCEPTED);

    let page = app
        .clone()
        .oneshot(
            Request::get("/api/v1/page-hashes?action=DELETE_AUTO,DELETE_MANUAL&sort=unknown&sort=hash,desc&size=9999")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page = body_json(page).await;
    assert_eq!(page["size"], 2000);
    assert_eq!(page["sort"]["sorted"], true);
    assert_eq!(page["content"][0]["hash"], "test");
    assert_eq!(page["content"][0]["size"], serde_json::Value::Null);
    assert_eq!(page["content"][0]["deleteCount"], 0);
    assert_eq!(page["content"][0]["matchCount"], 0);

    let updated = app
        .clone()
        .oneshot(
            Request::put("/api/v1/page-hashes")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"hash":"test","size":42,"action":"IGNORE"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::ACCEPTED);
    let page = app
        .clone()
        .oneshot(
            Request::get("/api/v1/page-hashes?action=IGNORE")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let page = body_json(page).await;
    assert_eq!(page["content"][0]["size"], serde_json::Value::Null);
    assert_eq!(page["content"][0]["action"], "IGNORE");

    for path in [
        "/api/v1/page-hashes/test/thumbnail",
        "/api/v1/page-hashes/unknown/test/thumbnail",
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
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    let blank = app
        .clone()
        .oneshot(
            Request::put("/api/v1/page-hashes")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"hash":" ","action":"IGNORE"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);

    let invalid_action = app
        .clone()
        .oneshot(
            Request::get("/api/v1/page-hashes?action=DELETE")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_action.status(), StatusCode::BAD_REQUEST);

    let invalid_resize = app
        .clone()
        .oneshot(
            Request::get("/api/v1/page-hashes/unknown/test/thumbnail?resize=-1")
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_resize.status(), StatusCode::BAD_REQUEST);

    let delete_all = app
        .clone()
        .oneshot(
            Request::post("/api/v1/page-hashes/test/delete-all")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_all.status(), StatusCode::ACCEPTED);
    let delete_match = app
        .oneshot(
            Request::post("/api/v1/page-hashes/test/delete-match")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"bookId":"book","url":"/book.cbz","pageNumber":1,"fileName":"1.png","fileSize":42,"mediaType":"image/png"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_match.status(), StatusCode::ACCEPTED);
}

#[tokio::test]
async fn unknown_preview_and_persisted_known_thumbnail_are_jpeg() {
    let (_temporary, storage, tasks, scanned_book, _) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let book = storage.find_book_by_id(&scanned_book.id).await.unwrap().unwrap();
    let media = storage.find_media_by_book_id(&scanned_book.id).await.unwrap().unwrap();
    let content = extract_page(&book, &media, 1).unwrap();
    let computed = hash_page(&content.bytes, &media.pages[0].media_type).unwrap();
    assert!(
        storage
            .update_page_hashes_if_current(
                &book,
                &media,
                &[PageHashUpdate {
                    number: 0,
                    file_name: media.pages[0].file_name.clone(),
                    media_type: media.pages[0].media_type.clone(),
                    file_hash: computed.hash.clone(),
                    algorithm: computed.algorithm.into(),
                }],
            )
            .await
            .unwrap()
    );
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let preview = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/v1/page-hashes/unknown/{}/thumbnail?resize=50",
                computed.hash
            ))
            .header(header::AUTHORIZATION, &admin)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preview.status(), StatusCode::OK);
    assert_eq!(preview.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(preview).await.starts_with(&[0xff, 0xd8]));

    let create = app
        .clone()
        .oneshot(
            Request::put("/api/v1/page-hashes")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"hash":"{}","size":{},"action":"IGNORE"}}"#,
                    computed.hash,
                    content.bytes.len()
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::ACCEPTED);
    let known = app
        .oneshot(
            Request::get(format!("/api/v1/page-hashes/{}/thumbnail", computed.hash))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(known.status(), StatusCode::OK);
    assert_eq!(known.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(known).await.starts_with(&[0xff, 0xd8]));
}
