use sha2::{Digest, Sha512};

use super::*;

const KOREADER_MEDIA_TYPE: &str = "application/vnd.koreader.v1+json";

async fn create_koreader_user(storage: &Storage, email: &str, shared_all_libraries: bool) -> (User, String) {
    let key = format!("key-{email}");
    let mut user = User::new(email.into(), "unused".into());
    user.roles.insert(UserRole::KoreaderSync);
    user.shared_all_libraries = shared_all_libraries;
    storage.create_user(&user).await.unwrap();
    storage
        .create_api_key(
            &new_id(),
            &user.id,
            &hex::encode(Sha512::digest(key.as_bytes())),
            "KOReader",
        )
        .await
        .unwrap();
    (user, key)
}

async fn enable_koreader_hash(storage: &Storage, book: &komga_core::Book, hash: &str) {
    let mut library = storage.find_library_by_id(&book.library_id).await.unwrap().unwrap();
    library.hash_koreader = true;
    storage.update_library(&library).await.unwrap();
    assert!(storage.update_book_koreader_hash_if_current(book, hash).await.unwrap());
}

#[tokio::test]
async fn koreader_authentication_and_fixed_page_progress_match_the_protocol() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let book_hash = "0123456789abcdef0123456789abcdef";
    enable_koreader_hash(&storage, &book, book_hash).await;
    let (user, key) = create_koreader_user(&storage, "koreader@example.org", true).await;
    let (_wrong_user, wrong_key) = create_koreader_user(&storage, "restricted@example.org", false).await;
    let no_role_key = "no-role-key";
    let no_role = User::new("no-role@example.org".into(), "unused".into());
    storage.create_user(&no_role).await.unwrap();
    storage
        .create_api_key(
            &new_id(),
            &no_role.id,
            &hex::encode(Sha512::digest(no_role_key.as_bytes())),
            "KOReader",
        )
        .await
        .unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());

    assert_eq!(
        app.clone()
            .oneshot(Request::post("/koreader/users/create").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    for request in [
        Request::get("/koreader/users/auth").body(Body::empty()).unwrap(),
        Request::get("/koreader/users/auth")
            .header("X-API-Key", &key)
            .body(Body::empty())
            .unwrap(),
        Request::get("/koreader/users/auth")
            .header("X-Auth-User", no_role_key)
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let authorized = app
        .clone()
        .oneshot(
            Request::get("/koreader/users/auth")
                .header("X-Auth-User", &key)
                .header(header::ACCEPT, KOREADER_MEDIA_TYPE)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    assert_eq!(authorized.headers()[header::CONTENT_TYPE], KOREADER_MEDIA_TYPE);
    assert_eq!(body_json(authorized).await["authorized"], "OK");

    let path = format!("/koreader/syncs/progress/{book_hash}");
    let missing = app
        .clone()
        .oneshot(
            Request::get(&path)
                .header("X-Auth-User", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::OK);
    let missing = body_json(missing).await;
    assert!(missing.get("percentage").is_none());
    assert_eq!(missing["message"], "No progress found for this book");

    let hidden = app
        .clone()
        .oneshot(
            Request::get(&path)
                .header("X-Auth-User", wrong_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);

    let payload = serde_json::json!({
        "document": book_hash,
        "percentage": 0.5,
        "progress": "1",
        "device": "Kobo Libra",
        "device_id": "device-1"
    });
    let updated = app
        .clone()
        .oneshot(
            Request::put("/koreader/syncs/progress")
                .header("X-Auth-User", &key)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::OK);
    let saved = storage.find_read_progress(&book.id, &user.id).await.unwrap().unwrap();
    assert_eq!(saved.page, 1);
    assert_eq!(saved.device_id, "device-1");

    let pulled = app
        .clone()
        .oneshot(
            Request::get(&path)
                .header("X-Auth-User", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pulled = body_json(pulled).await;
    assert_eq!(pulled["document"], book_hash);
    assert_eq!(pulled["percentage"], 0.5);
    assert_eq!(pulled["progress"], "1");
    assert_eq!(pulled["device_id"], "device-1");

    for (content_type, accept, expected) in [
        ("text/plain", None, StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ("application/json", Some("text/plain"), StatusCode::NOT_ACCEPTABLE),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::put("/koreader/syncs/progress")
                    .header("X-Auth-User", &key)
                    .header(header::CONTENT_TYPE, content_type)
                    .header(header::ACCEPT, accept.unwrap_or("*/*"))
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(
        storage.find_read_progress(&book.id, &user.id).await.unwrap().unwrap(),
        saved
    );
}

#[tokio::test]
async fn koreader_hash_collisions_are_scoped_to_visible_books() {
    let (temporary, storage, tasks, first, _media) = analyzed_media_fixture("archives/zip.zip", "First.cbz").await;
    let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    enable_koreader_hash(&storage, &first, hash).await;
    let source = first.url.to_file_path().unwrap();
    fs::copy(&source, source.with_file_name("Second.cbz")).unwrap();
    let library = storage.find_library_by_id(&first.library_id).await.unwrap().unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), true)
        .await
        .unwrap();
    let second = storage
        .list_active_books_by_library_id(&library.id)
        .await
        .unwrap()
        .into_iter()
        .find(|book| book.id != first.id)
        .unwrap();
    assert!(
        storage
            .update_book_koreader_hash_if_current(&second, hash)
            .await
            .unwrap()
    );
    let (_user, key) = create_koreader_user(&storage, "collision@example.org", true).await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    let response = app
        .oneshot(
            Request::get(format!("/koreader/syncs/progress/{hash}"))
                .header("X-Auth-User", key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    drop(temporary);
}

#[tokio::test]
async fn koreader_epub_progress_accepts_both_resource_index_formats() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/epub3.epub", "Volume.epub").await;
    let hash = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    enable_koreader_hash(&storage, &book, hash).await;
    let (user, key) = create_koreader_user(&storage, "epub-koreader@example.org", true).await;
    let MediaExtension::Epub(extension) = media.extension.unwrap();
    let expected_href = extension.positions[0].href.clone();
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());

    for progress in ["/body/DocFragment[1]/body/p[1]", "#_doc_fragment_0_ c37"] {
        let response = app
            .clone()
            .oneshot(
                Request::put("/koreader/syncs/progress")
                    .header("X-Auth-User", &key)
                    .header(header::CONTENT_TYPE, KOREADER_MEDIA_TYPE)
                    .header(header::ACCEPT, KOREADER_MEDIA_TYPE)
                    .body(Body::from(
                        serde_json::json!({
                            "document": hash,
                            "percentage": 0.25,
                            "progress": progress,
                            "device": "KOReader",
                            "device_id": "epub-device"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "progress: {progress}");
    }
    let saved = storage.find_read_progress(&book.id, &user.id).await.unwrap().unwrap();
    assert_eq!(saved.locator.unwrap().href, expected_href);

    let pulled = app
        .clone()
        .oneshot(
            Request::get(format!("/koreader/syncs/progress/{hash}"))
                .header("X-Auth-User", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(pulled).await["progress"], "/body/DocFragment[1].0");

    for (percentage, progress) in [(1.1, "DocFragment[1]"), (0.5, "DocFragment[999]")] {
        let invalid = app
            .clone()
            .oneshot(
                Request::put("/koreader/syncs/progress")
                    .header("X-Auth-User", &key)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "document": hash,
                            "percentage": percentage,
                            "progress": progress,
                            "device": "KOReader",
                            "device_id": "epub-device"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    }
}
