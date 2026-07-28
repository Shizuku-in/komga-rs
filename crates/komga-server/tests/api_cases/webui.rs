use super::*;

#[tokio::test]
async fn embedded_webui_serves_spa_routes_assets_cache_headers_and_compression() {
    let app = test_app().await;

    let index = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(index.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(index.headers()[header::CACHE_CONTROL], "no-store");
    let index = String::from_utf8(body_bytes(index).await).unwrap();
    assert!(index.contains("<base href=\"/\">"));
    assert!(index.contains("window.resourceBaseUrl = '/'"));
    assert!(!index.contains("__KOMGA_BASE_URL__"));
    let script = entry_script(&index);

    let asset = app
        .clone()
        .oneshot(Request::get(format!("/{script}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert!(
        asset.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .contains("javascript")
    );
    assert_eq!(asset.headers()[header::CACHE_CONTROL], "public, max-age=31536000");
    assert!(body_bytes(asset).await.len() > 1_000);

    for encoding in ["br", "gzip"] {
        let compressed = app
            .clone()
            .oneshot(
                Request::get(format!("/{script}"))
                    .header(header::ACCEPT_ENCODING, encoding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(compressed.status(), StatusCode::OK);
        assert_eq!(compressed.headers()[header::CONTENT_ENCODING], encoding);
        assert!(
            compressed.headers()[header::VARY]
                .to_str()
                .unwrap()
                .contains("accept-encoding")
        );
        assert!(!body_bytes(compressed).await.is_empty());
    }

    let head = app
        .clone()
        .oneshot(
            Request::head(format!("/{script}"))
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(!head.headers().contains_key(header::CONTENT_ENCODING));
    assert!(body_bytes(head).await.is_empty());

    for path in ["/favicon.ico", "/manifest.json"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store", "{path}");
    }

    let deep_link = app
        .clone()
        .oneshot(Request::get("/book/BOOK_ID").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(deep_link.status(), StatusCode::OK);
    assert_eq!(deep_link.headers()[header::CACHE_CONTROL], "no-store");

    let health = app
        .clone()
        .oneshot(
            Request::get("/actuator/health")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(!health.headers().contains_key(header::CONTENT_ENCODING));
}

#[tokio::test]
async fn embedded_webui_injects_context_path_into_deep_links_and_assets() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let state = AppState::new(storage, tasks)
        .await
        .unwrap()
        .with_server_bind_settings(ServerBindSettings {
            configuration_port: 25_600,
            configuration_context_path: Some("/komga".into()),
            effective_port: Some(25_600),
            effective_context_path: "/komga".into(),
        });
    let app = build_app(state);

    let outside_context = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(outside_context.status(), StatusCode::NOT_FOUND);

    let context_root = app
        .clone()
        .oneshot(Request::get("/komga/?server_redirect=Y").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(context_root.status(), StatusCode::OK);

    let index = app
        .clone()
        .oneshot(Request::get("/komga/book/BOOK_ID").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let index = String::from_utf8(body_bytes(index).await).unwrap();
    assert!(index.contains("<base href=\"/komga/\">"));
    assert!(index.contains("window.resourceBaseUrl = '/komga/'"));
    let script = entry_script(&index);

    let asset = app
        .clone()
        .oneshot(Request::get(format!("/komga/{script}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
}

#[tokio::test]
async fn embedded_webui_does_not_mask_backend_or_missing_static_routes() {
    let app = test_app().await;

    for path in [
        "/api/v1/missing",
        "/API/v1/missing",
        "/opds/missing",
        "/sse/missing",
        "/actuator/missing",
        "/kobo/missing",
        "/koreader/missing",
        "/oauth2/missing",
        "/login/oauth2/missing",
        "/js/missing.js",
        "/robots.txt",
    ] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(body_bytes(response).await.is_empty(), "{path}");
    }

    let post = app
        .clone()
        .oneshot(Request::post("/book/BOOK_ID").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(post.status(), StatusCode::NOT_FOUND);

    let frontend_prefix = app
        .clone()
        .oneshot(Request::get("/apiary").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(frontend_prefix.status(), StatusCode::OK);
}

fn entry_script(index: &str) -> &str {
    let marker = index
        .find("src=\"./js/")
        .or_else(|| index.find("src=\"js/"))
        .expect("generated index has an entry script");
    let start = marker + "src=\"".len();
    let tail = &index[start..];
    tail.trim_start_matches("./")
        .split_once('"')
        .expect("entry script attribute is terminated")
        .0
}
