use std::{fs, path::PathBuf};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use komga_core::scan_library_files;
use komga_media::{analyze_book, extract_page, hash_page};
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

#[tokio::test]
async fn rar_matches_preview_and_manual_delete_use_existing_page_hash_contract() {
    let (_temporary, storage, tasks, scanned_book, _) =
        analyzed_media_fixture("archives/rar4-solid.rar", "Volume.cbr").await;
    let book = storage.find_book_by_id(&scanned_book.id).await.unwrap().unwrap();
    let media = storage.find_media_by_book_id(&scanned_book.id).await.unwrap().unwrap();
    let content = extract_page(&book, &media, 1).unwrap();
    let computed = hash_page(&content.bytes, &content.media_type).unwrap();
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
    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let matches = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/page-hashes/{}", computed.hash))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(matches.status(), StatusCode::OK);
    let matched = body_json(matches).await["content"][0].clone();
    assert_eq!(matched["bookId"], book.id);
    assert_eq!(matched["pageNumber"], 1);
    assert_eq!(matched["fileName"], media.pages[0].file_name);
    assert_eq!(matched["fileSize"], media.pages[0].file_size.unwrap());
    assert_eq!(matched["mediaType"], "image/png");

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

    let delete = app
        .oneshot(
            Request::post(format!("/api/v1/page-hashes/{}/delete-match", computed.hash))
                .header(header::AUTHORIZATION, admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(matched.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::ACCEPTED);
    let removal = tasks
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|task| task.simple_type == "RemoveHashedPages")
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&removal.payload).unwrap();
    assert_eq!(payload["bookId"], book.id);
    assert_eq!(payload["pages"][0]["mediaType"], "image/png");
    assert_eq!(payload["pages"][0]["pageNumber"], 1);
}

#[tokio::test]
async fn fixed_layout_epub_matches_preview_and_rejects_manual_removal_atomically() {
    let (temporary, storage, tasks, scanned_book, _) =
        analyzed_media_fixture("archives/epub3.epub", "Fixed.epub").await;
    let book = storage.find_book_by_id(&scanned_book.id).await.unwrap().unwrap();
    let media = storage.find_media_by_book_id(&scanned_book.id).await.unwrap().unwrap();
    let content = extract_page(&book, &media, 1).unwrap();
    let computed = hash_page(&content.bytes, &content.media_type).unwrap();
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

    let library = storage.find_library_by_id(&book.library_id).await.unwrap().unwrap();
    let series = book.url.to_file_path().unwrap().parent().unwrap().to_path_buf();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../komga/komga/src/test/resources/archives/zip.zip"),
        series.join("Supported.cbz"),
    )
    .unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let zip_book = storage
        .list_active_books_by_library_id(&library.id)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.url.path().ends_with("Supported.cbz"))
        .unwrap();
    let zip_media = analyze_book(&zip_book, true, || false).unwrap();
    storage.replace_media_analysis(&zip_media).await.unwrap();
    let zip_book = storage.find_book_by_id(&zip_book.id).await.unwrap().unwrap();
    let zip_media = storage.find_media_by_book_id(&zip_book.id).await.unwrap().unwrap();
    assert!(
        storage
            .update_page_hashes_if_current(
                &zip_book,
                &zip_media,
                &[PageHashUpdate {
                    number: 0,
                    file_name: zip_media.pages[0].file_name.clone(),
                    media_type: zip_media.pages[0].media_type.clone(),
                    file_hash: computed.hash.clone(),
                    algorithm: computed.algorithm.into(),
                }],
            )
            .await
            .unwrap()
    );

    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    claim(&app).await;
    let admin = basic("admin@example.org", "password");
    let matches = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/page-hashes/{}", computed.hash))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(matches.status(), StatusCode::OK);
    let matches = body_json(matches).await["content"].as_array().unwrap().clone();
    assert_eq!(matches.len(), 2);
    let epub_match = matches.iter().find(|matched| matched["bookId"] == book.id).unwrap();
    let zip_match = matches.iter().find(|matched| matched["bookId"] == zip_book.id).unwrap();
    assert_eq!(epub_match["fileName"], media.pages[0].file_name);
    assert_eq!(epub_match["mediaType"], "image/jpeg");

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

    let delete_epub = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/page-hashes/{}/delete-match", computed.hash))
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(epub_match.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_epub.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(delete_epub).await["message"],
        "Page removal is not supported for this media"
    );
    assert!(tasks.list().await.unwrap().is_empty());

    let delete_all = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/page-hashes/{}/delete-all", computed.hash))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_all.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(delete_all).await["message"],
        "Page removal is not supported for one or more matching media"
    );
    assert!(tasks.list().await.unwrap().is_empty());

    let delete_zip = app
        .oneshot(
            Request::post(format!("/api/v1/page-hashes/{}/delete-match", computed.hash))
                .header(header::AUTHORIZATION, admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(zip_match.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_zip.status(), StatusCode::ACCEPTED);
    assert_eq!(
        tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .filter(|task| task.simple_type == "RemoveHashedPages")
            .count(),
        1
    );
    drop(temporary);
}

#[tokio::test]
async fn pdf_matches_preview_and_manual_delete_use_rendered_jpeg_contract() {
    let (_temporary, storage, tasks, scanned_book, _) = analyzed_media_fixture("pdf/komga.pdf", "Volume.pdf").await;
    let book = storage.find_book_by_id(&scanned_book.id).await.unwrap().unwrap();
    let media = storage.find_media_by_book_id(&scanned_book.id).await.unwrap().unwrap();
    let content = extract_page(&book, &media, 1).unwrap();
    let computed = hash_page(&content.bytes, &content.media_type).unwrap();
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
    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let matches = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/page-hashes/{}", computed.hash))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(matches.status(), StatusCode::OK);
    let matches = body_json(matches).await;
    let matched = matches["content"][0].clone();
    assert_eq!(matched["bookId"], book.id);
    assert_eq!(matched["pageNumber"], 1);
    assert_eq!(matched["fileName"], "1");
    assert_eq!(matched["fileSize"], 0);
    assert_eq!(matched["mediaType"], "image/jpeg");

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

    let delete = app
        .oneshot(
            Request::post(format!("/api/v1/page-hashes/{}/delete-match", computed.hash))
                .header(header::AUTHORIZATION, admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(matched.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete.status(), StatusCode::ACCEPTED);
    let removal = tasks
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|task| task.simple_type == "RemoveHashedPages")
        .unwrap();
    let payload: serde_json::Value = serde_json::from_str(&removal.payload).unwrap();
    assert_eq!(payload["pages"][0]["mediaType"], "image/jpeg");
    assert_eq!(payload["pages"][0]["fileSize"], 0);
}
