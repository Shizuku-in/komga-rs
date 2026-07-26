use sha2::{Digest, Sha256};

use super::*;

#[tokio::test]
async fn bundled_font_families_require_auth_but_resources_and_css_are_public() {
    let app = test_app().await;
    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/fonts/families").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let families = app
        .clone()
        .oneshot(
            Request::get("/api/v1/fonts/families")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(families.status(), StatusCode::OK);
    assert_eq!(body_json(families).await, serde_json::json!(["OpenDyslexic"]));

    let font = app
        .clone()
        .oneshot(
            Request::get("/api/v1/fonts/resource/OpenDyslexic/OpenDyslexic-Regular.woff")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(font.status(), StatusCode::OK);
    assert_eq!(font.headers()[header::CONTENT_TYPE], "font/woff");
    assert!(
        font.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("OpenDyslexic-Regular.woff")
    );
    assert_eq!(
        hex::encode(Sha256::digest(body_bytes(font).await)),
        "be2fbf9c1b0dddd39a182e497cb011b43ae588b2ebb80370377d2fffaa3b4c4a"
    );

    let css = app
        .clone()
        .oneshot(
            Request::get("/api/v1/fonts/resource/OpenDyslexic/css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(css.status(), StatusCode::OK);
    assert_eq!(css.headers()[header::CONTENT_TYPE], "text/css");
    let css = String::from_utf8(body_bytes(css).await).unwrap();
    assert_eq!(css.matches("@font-face").count(), 4);
    assert!(css.contains("url('OpenDyslexic-Bold.woff2') format('woff2')"));

    for path in [
        "/api/v1/fonts/resource/Missing/css",
        "/api/v1/fonts/resource/OpenDyslexic/missing.woff",
        "/api/v1/fonts/resource/%2E%2E/OpenDyslexic-Regular.woff",
    ] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn configured_local_fonts_are_served_under_the_effective_context_path() {
    let fonts = tempfile::tempdir().unwrap();
    fs::create_dir_all(fonts.path().join("Custom")).unwrap();
    fs::write(fonts.path().join("Custom/Custom-Bold.otf"), b"custom-font").unwrap();
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let state = AppState::new(storage, tasks)
        .await
        .unwrap()
        .with_fonts_directory(fonts.path().to_path_buf())
        .with_server_bind_settings(ServerBindSettings {
            configuration_port: 25_600,
            configuration_context_path: Some("/komga".into()),
            effective_port: Some(25_600),
            effective_context_path: "/komga".into(),
        });
    let app = build_app(state);

    let response = app
        .clone()
        .oneshot(
            Request::get("/komga/api/v1/fonts/resource/Custom/Custom-Bold.otf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "font/otf");
    assert_eq!(body_bytes(response).await, b"custom-font");
    let root = app
        .oneshot(
            Request::get("/api/v1/fonts/resource/Custom/Custom-Bold.otf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::NOT_FOUND);
}
