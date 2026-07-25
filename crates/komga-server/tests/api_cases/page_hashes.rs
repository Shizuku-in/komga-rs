use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use tower::ServiceExt;

use crate::support::{basic, body_json, claim, test_app};

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
        .oneshot(
            Request::get("/api/v1/page-hashes/unknown/test/thumbnail?resize=-1")
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_resize.status(), StatusCode::BAD_REQUEST);
}
