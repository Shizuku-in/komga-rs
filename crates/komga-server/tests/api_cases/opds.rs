use super::*;

#[tokio::test]
async fn opds_auth_document_and_challenge_match_the_v2_contract() {
    let app = test_app().await;

    let document = app
        .clone()
        .oneshot(
            Request::get("/opds/v2/auth")
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(document.status(), StatusCode::OK);
    assert_eq!(
        document.headers()[header::CONTENT_TYPE],
        "application/opds-authentication+json"
    );
    let document = body_json(document).await;
    assert_eq!(document["title"], "Komga");
    assert_eq!(document["id"], "http://reader.example/opds/v2/auth");
    assert_eq!(document["authentication"][0]["type"], "http://opds-spec.org/auth/basic");
    assert_eq!(document["authentication"][0]["labels"]["login"], "Email");

    let challenge = app
        .oneshot(
            Request::get("/opds/v2/catalog")
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(challenge.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        challenge.headers()[header::CONTENT_TYPE],
        "application/opds-authentication+json"
    );
    assert_eq!(challenge.headers()[header::WWW_AUTHENTICATE], "Basic realm=\"Realm\"");
    assert!(
        challenge.headers()[header::LINK]
            .to_str()
            .unwrap()
            .contains("http://reader.example/opds/v2/auth")
    );
    assert_eq!(body_json(challenge).await["title"], "Komga");
}

#[tokio::test]
async fn opds_v1_catalog_and_acquisition_feeds_are_valid_atom() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume 1.cbz").await;
    let app = build_app(
        AppState::new(storage, tasks)
            .await
            .unwrap()
            .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]),
    );
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let catalog = app
        .clone()
        .oneshot(through_trusted_proxy(
            Request::get("/opds/v1.2/catalog")
                .header(header::AUTHORIZATION, &admin)
                .header("forwarded", "for=192.0.2.1;proto=https;host=reader.example")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(catalog.status(), StatusCode::OK);
    assert_eq!(
        catalog.headers()[header::CONTENT_TYPE],
        "application/atom+xml;profile=opds-catalog"
    );
    let catalog = String::from_utf8(body_bytes(catalog).await).unwrap();
    let document = roxmltree::Document::parse(&catalog).unwrap();
    assert_eq!(
        document.root_element().tag_name().namespace(),
        Some("http://www.w3.org/2005/Atom")
    );
    assert_eq!(
        document.descendants().filter(|node| node.has_tag_name("entry")).count(),
        9
    );
    assert!(catalog.contains("https://reader.example/komga/opds/v1.2/series/latest"));
    assert!(catalog.contains("https://reader.example/komga/opds/v2/catalog"));

    let series_id = book.series_id.clone();
    let feed = app
        .clone()
        .oneshot(
            Request::get(format!("/opds/v1.2/series/{series_id}"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(feed.status(), StatusCode::OK);
    let feed = String::from_utf8(body_bytes(feed).await).unwrap();
    let document = roxmltree::Document::parse(&feed).unwrap();
    let entry = document.descendants().find(|node| node.has_tag_name("entry")).unwrap();
    assert_eq!(
        entry.children().find(|node| node.has_tag_name("id")).unwrap().text(),
        Some(book.id.as_str())
    );
    let acquisition = entry
        .children()
        .find(|node| node.has_tag_name("link") && node.attribute("rel") == Some("http://opds-spec.org/acquisition"))
        .unwrap();
    assert_eq!(acquisition.attribute("type"), Some("application/zip"));
    let pse = entry
        .children()
        .find(|node| node.has_tag_name("link") && node.attribute("rel") == Some("http://vaemendis.net/opds-pse/stream"))
        .unwrap();
    assert_eq!(pse.attribute(("http://vaemendis.net/opds-pse/ns", "count")), Some("1"));
    assert!(pse.attribute("href").unwrap().contains("{pageNumber}"));

    let first_page = app
        .oneshot(
            Request::get(format!("/opds/v1.2/books/{}/pages/0", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first_page.status(), StatusCode::OK);
    assert!(!body_bytes(first_page).await.is_empty());
}

#[tokio::test]
async fn opds_v2_catalog_publications_and_manifest_use_opds_links() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let app = build_app(
        AppState::new(storage, tasks)
            .await
            .unwrap()
            .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]),
    );
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let catalog = app
        .clone()
        .oneshot(through_trusted_proxy(
            Request::get("/opds/v2/catalog")
                .header(header::AUTHORIZATION, &admin)
                .header("forwarded", "for=192.0.2.1;proto=https;host=reader.example")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(catalog.status(), StatusCode::OK);
    assert_eq!(catalog.headers()[header::CONTENT_TYPE], "application/opds+json");
    let catalog = body_json(catalog).await;
    assert_eq!(catalog["metadata"]["title"], "All libraries - Recommended");
    let latest = catalog["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|group| group["metadata"]["title"] == "Latest Books")
        .unwrap();
    let publication = &latest["publications"][0];
    assert_eq!(publication["metadata"]["title"], "Volume");
    assert_eq!(publication["images"][0]["type"], "image/jpeg");
    assert_eq!(
        publication["metadata"]["belongsTo"]["series"][0]["links"][0]["type"],
        "application/opds+json"
    );
    assert!(
        publication["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|link| link["rel"] == "http://readium.org/webpub-manifest/relations/progression")
    );

    let manifest = app
        .oneshot(
            Request::get(format!("/opds/v2/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(
        manifest.headers()[header::CONTENT_TYPE],
        "application/opds-publication+json"
    );
    let manifest = body_json(manifest).await;
    assert_eq!(manifest["links"][0]["type"], "application/opds-publication+json");
    assert!(
        manifest["links"][0]["href"]
            .as_str()
            .unwrap()
            .contains("/opds/v2/books/")
    );
    assert!(
        manifest["readingOrder"][0]["href"]
            .as_str()
            .unwrap()
            .contains("/opds/v2/books/")
    );
}

#[tokio::test]
async fn opds_routes_enforce_library_visibility_and_serve_reader_resource_aliases() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Restricted.cbz").await;
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);

    let password_hash = bcrypt::hash("reader-password", 4).unwrap();
    let mut restricted = User::new("restricted-opds@example.org".into(), password_hash);
    restricted.shared_all_libraries = false;
    storage.create_user(&restricted).await.unwrap();
    let restricted_auth = basic("restricted-opds@example.org", "reader-password");
    for path in [
        format!("/opds/v1.2/series/{}", book.series_id),
        format!("/opds/v2/series/{}", book.series_id),
        format!("/opds/v2/books/{}/manifest", book.id),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path)
                    .header(header::AUTHORIZATION, &restricted_auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    let admin = basic("admin@example.org", "password");
    for (path, expected) in [
        (format!("/opds/v2/books/{}/file", book.id), StatusCode::OK),
        (format!("/opds/v2/books/{}/pages/1", book.id), StatusCode::OK),
        (
            format!("/opds/v2/books/{}/progression", book.id),
            StatusCode::NO_CONTENT,
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
        assert_eq!(response.status(), expected);
    }
}
