use std::num::NonZeroUsize;

use chrono::{Duration as ChronoDuration, SecondsFormat, Utc};
use komga_core::{MediaExtension, ThumbnailBook};

use super::*;

#[tokio::test]
async fn kobo_core_sync_progress_and_resources_form_a_complete_device_loop() {
    let (_temporary, storage, tasks, book, media) =
        analyzed_media_fixture("archives/epub3.epub", "Volume 1.epub").await;
    let source = book.url.to_file_path().unwrap();
    fs::copy(&source, source.with_file_name("Volume 2.epub")).unwrap();
    let library = storage.find_library_by_id(&book.library_id).await.unwrap().unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let books = storage.list_books_by_series_id(&book.series_id).await.unwrap();
    let second = books.iter().find(|candidate| candidate.id != book.id).unwrap();
    let second_media = analyze_book(second, true, || false).unwrap();
    storage.replace_media_analysis(&second_media).await.unwrap();

    let generated = generate_thumbnail(&book, &media, 300).unwrap();
    let thumbnail = ThumbnailBook {
        id: new_id(),
        dimension: image_dimension(&generated.bytes).unwrap(),
        file_size: i64::try_from(generated.bytes.len()).unwrap(),
        thumbnail: Some(generated.bytes),
        url: None,
        selected: false,
        thumbnail_type: "GENERATED".into(),
        media_type: generated.media_type,
        book_id: book.id.clone(),
    };
    storage.replace_generated_thumbnail(&thumbnail).await.unwrap();

    let mut user = User::new("kobo@example.org".into(), "unused".into());
    user.roles.insert(UserRole::KoboSync);
    user.roles.insert(UserRole::FileDownload);
    storage.create_user(&user).await.unwrap();
    let key = "kobo-plain-key";
    storage
        .create_api_key(
            "kobo-key-id",
            &user.id,
            &hex::encode(Sha512::digest(key.as_bytes())),
            "Kobo Libra",
        )
        .await
        .unwrap();

    let mut no_role = User::new("no-kobo@example.org".into(), "unused".into());
    no_role.roles.insert(UserRole::FileDownload);
    storage.create_user(&no_role).await.unwrap();
    storage
        .create_api_key(
            "no-kobo-key-id",
            &no_role.id,
            &hex::encode(Sha512::digest(b"no-kobo-key")),
            "Other",
        )
        .await
        .unwrap();

    let mut sync_only = User::new("sync-only@example.org".into(), "unused".into());
    sync_only.roles.insert(UserRole::KoboSync);
    storage.create_user(&sync_only).await.unwrap();
    storage
        .create_api_key(
            "sync-only-key-id",
            &sync_only.id,
            &hex::encode(Sha512::digest(b"sync-only-key")),
            "Sync only",
        )
        .await
        .unwrap();

    let state = AppState::new(storage.clone(), tasks)
        .await
        .unwrap()
        .with_kobo_sync_item_limit(NonZeroUsize::new(1).unwrap())
        .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]);
    let app = build_app(state);

    assert_eq!(
        app.clone()
            .oneshot(Request::get("/kobo/not-valid/ping").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(Request::get("/kobo/no-kobo-key/ping").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(Request::get("/kobo/sync-only-key/ping").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(
                Request::get(format!("/kobo/sync-only-key/v1/books/{}/file/epub", book.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let catch_all = app
        .clone()
        .oneshot(
            Request::patch(format!("/kobo/{key}/v1/wishlist"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(catch_all.status(), StatusCode::OK);
    assert_eq!(body_json(catch_all).await, serde_json::json!({}));

    let initialization = app
        .clone()
        .oneshot(through_trusted_proxy(
            Request::get(format!("/kobo/{key}/v1/initialization"))
                .header("forwarded", "for=192.0.2.1;proto=https;host=reader.example")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(initialization.status(), StatusCode::OK);
    assert_eq!(initialization.headers()["x-kobo-apitoken"], "e30=");
    let initialization = body_json(initialization).await;
    assert_eq!(
        initialization["Resources"]["image_host"],
        "https://reader.example/komga"
    );
    assert_eq!(
        initialization["Resources"]["image_url_template"],
        format!(
            "https://reader.example/komga/kobo/{key}/v1/books/{{ImageId}}/thumbnail/{{Width}}/{{Height}}/false/image.jpg"
        )
    );
    assert_eq!(
        initialization["Resources"]["account_page_rakuten"],
        "https://my.rakuten.co.jp/"
    );

    let auth = app
        .clone()
        .oneshot(
            Request::post(format!("/kobo/{key}/v1/auth/device"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"UserKey":"device-user"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(auth.status(), StatusCode::OK);
    let auth = body_json(auth).await;
    assert_eq!(auth["UserKey"], "device-user");
    assert_eq!(auth["TokenType"], "Bearer");

    let first = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/sync"))
                .header(header::HOST, "127.0.0.1:4567")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(first.headers()["x-kobo-sync"], "continue");
    let first_token = first.headers()["x-kobo-synctoken"].to_str().unwrap().to_owned();
    assert!(first_token.starts_with("KOMGA."));
    let first = body_json(first).await;
    assert_eq!(first.as_array().unwrap().len(), 1);
    assert!(first[0].get("NewEntitlement").is_some());

    let second_page = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/sync"))
                .header(header::HOST, "127.0.0.1:4567")
                .header("x-kobo-synctoken", first_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second_page.status(), StatusCode::OK);
    assert!(!second_page.headers().contains_key("x-kobo-sync"));
    let final_token = second_page.headers()["x-kobo-synctoken"].to_str().unwrap().to_owned();
    assert!(final_token.starts_with("KOMGA."));
    assert_eq!(body_json(second_page).await.as_array().unwrap().len(), 1);

    let cross_user = app
        .clone()
        .oneshot(
            Request::get("/kobo/sync-only-key/v1/library/sync")
                .header(header::HOST, "127.0.0.1:4567")
                .header("x-kobo-synctoken", final_token.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cross_user.status(), StatusCode::OK);
    assert_eq!(cross_user.headers()["x-kobo-sync"], "continue");
    let cross_user = body_json(cross_user).await;
    assert!(cross_user[0].get("NewEntitlement").is_some());

    let metadata = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/{}/metadata", book.id))
                .header(header::HOST, "127.0.0.1:4567")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata = body_json(metadata).await;
    assert_eq!(metadata[0]["EntitlementId"], book.id);
    assert_eq!(metadata[0]["DownloadUrls"][0]["Format"], "EPUB3FL");
    assert_eq!(
        metadata[0]["DownloadUrls"][0]["Url"],
        format!(
            "http://127.0.0.1:4567/kobo/{key}/v1/books/{}/file/epub?convert_kepub=false",
            book.id
        )
    );

    let state = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/{}/state", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(state.status(), StatusCode::OK);
    assert_eq!(body_json(state).await[0]["StatusInfo"]["Status"], "ReadyToRead");

    let position = match media.extension.as_ref().unwrap() {
        MediaExtension::Epub(extension) => extension.positions.first().unwrap(),
    };
    let modified = (Utc::now() + ChronoDuration::seconds(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let update = serde_json::json!({
        "ReadingStates": [{
            "Created": null,
            "CurrentBookmark": {
                "LastModified": modified,
                "ProgressPercent": 1.0,
                "ContentSourceProgressPercent": 0.0,
                "Location": {"Value": position.kobo_span, "Type": "KoboSpan", "Source": position.href}
            },
            "EntitlementId": book.id,
            "LastModified": modified,
            "PriorityTimestamp": null,
            "Statistics": {"LastModified": modified},
            "StatusInfo": {"LastModified": modified, "Status": "Finished"}
        }]
    });
    let update = app
        .clone()
        .oneshot(
            Request::put(format!("/kobo/{key}/v1/library/{}/state", book.id))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(update.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    assert_eq!(body_json(update).await["RequestResult"], "Success");
    let saved = storage.find_read_progress(&book.id, &user.id).await.unwrap().unwrap();
    assert!(saved.completed);
    assert_eq!(saved.device_id, "kobo-key-id");
    assert_eq!(saved.device_name, "Kobo Libra");

    let progress_page = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/sync"))
                .header(header::HOST, "127.0.0.1:4567")
                .header("x-kobo-synctoken", final_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(progress_page.status(), StatusCode::OK);
    assert_eq!(progress_page.headers()["x-kobo-sync"], "continue");
    let progress_token = progress_page.headers()["x-kobo-synctoken"].to_str().unwrap().to_owned();
    let progress_page = body_json(progress_page).await;
    assert_eq!(progress_page.as_array().unwrap().len(), 1);
    assert!(progress_page[0].get("ChangedReadingState").is_some());

    let on_deck_page = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/sync"))
                .header(header::HOST, "127.0.0.1:4567")
                .header("x-kobo-synctoken", progress_token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(on_deck_page.status(), StatusCode::OK);
    assert!(!on_deck_page.headers().contains_key("x-kobo-sync"));
    let on_deck_page = body_json(on_deck_page).await;
    assert_eq!(on_deck_page[0]["NewTag"]["Tag"]["Id"], "KOMGA-ONDECK");
    assert_eq!(on_deck_page[0]["NewTag"]["Tag"]["Items"][0]["RevisionId"], second.id);

    let unavailable = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/books/{}/file/epub?convert_kepub=true", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    let file = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/books/{}/file/epub", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(file.status(), StatusCode::OK);
    assert_eq!(file.headers()[header::CONTENT_TYPE], "application/epub+zip");
    assert!(body_bytes(file).await.starts_with(b"PK"));

    let cover = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/kobo/{key}/v1/books/{}/thumbnail/300/400/false/image.jpg",
                thumbnail.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cover.status(), StatusCode::OK);
    assert_eq!(cover.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(cover).await.starts_with(&[0xff, 0xd8]));

    let targeted = storage.create_sync_point(&user, Some("kobo-key-id")).await.unwrap();
    let retained = storage.create_sync_point(&user, Some("other-key-id")).await.unwrap();
    let reset = app
        .clone()
        .oneshot(
            Request::delete("/api/v1/syncpoints/me?key_id=missing,kobo-key-id&key_id=also-missing")
                .header("x-api-key", key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::NO_CONTENT);
    assert!(
        storage
            .find_sync_point_for_user(&targeted.id, &user.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        storage
            .find_sync_point_for_user(&retained.id, &user.id)
            .await
            .unwrap()
            .is_some()
    );

    let reset_all = app
        .oneshot(
            Request::delete("/api/v1/syncpoints/me")
                .header("x-api-key", key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reset_all.status(), StatusCode::NO_CONTENT);
    assert!(
        storage
            .find_sync_point_for_user(&retained.id, &user.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn kobo_converts_and_caches_reflowable_epub_delivery_without_external_program() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture(
        "epub/The Incomplete Theft - Ralph Burke.epub",
        "The Incomplete Theft.epub",
    )
    .await;

    let mut user = User::new("kepub@example.org".into(), "unused".into());
    user.roles.insert(UserRole::KoboSync);
    user.roles.insert(UserRole::FileDownload);
    storage.create_user(&user).await.unwrap();
    let key = "kepub-key";
    storage
        .create_api_key(
            "kepub-key-id",
            &user.id,
            &hex::encode(Sha512::digest(key.as_bytes())),
            "Kobo",
        )
        .await
        .unwrap();
    let state = AppState::new(storage, tasks).await.unwrap();
    let app = build_app(state);

    let metadata = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/{}/metadata", book.id))
                .header(header::HOST, "127.0.0.1:4567")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata = body_json(metadata).await;
    assert_eq!(metadata[0]["DownloadUrls"][0]["Format"], "KEPUB");
    assert!(
        metadata[0]["DownloadUrls"][0]["Url"]
            .as_str()
            .unwrap()
            .ends_with("?convert_kepub=true")
    );

    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/books/{}/file/epub?convert_kepub=true", book.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/epub+zip");
        assert!(
            response.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .unwrap()
                .contains(".kepub.epub")
        );
        let bytes = body_bytes(response).await;
        assert!(bytes.starts_with(b"PK"));
        let mut archive = ZipArchive::new(Cursor::new(bytes)).unwrap();
        let chapter = (0..archive.len()).find_map(|index| {
            let mut entry = archive.by_index(index).ok()?;
            if !entry.name().ends_with(".xhtml") || entry.name().contains("titlepage-dummy") {
                return None;
            }
            let mut content = String::new();
            entry.read_to_string(&mut content).ok()?;
            content.contains("koboSpan").then_some(content)
        });
        assert!(chapter.is_some());
    }
}

#[tokio::test]
async fn kobo_delivers_native_kepub_without_reconverting_it() {
    let (_temporary, storage, tasks, book, mut media) =
        analyzed_media_fixture("epub/The Incomplete Theft - Ralph Burke.epub", "Native.kepub.epub").await;
    media.epub_is_kepub = true;
    storage.replace_media_analysis(&media).await.unwrap();

    let mut user = User::new("native-kepub@example.org".into(), "unused".into());
    user.roles.insert(UserRole::KoboSync);
    user.roles.insert(UserRole::FileDownload);
    storage.create_user(&user).await.unwrap();
    let key = "native-kepub-key";
    storage
        .create_api_key(
            "native-kepub-key-id",
            &user.id,
            &hex::encode(Sha512::digest(key.as_bytes())),
            "Kobo",
        )
        .await
        .unwrap();
    let app = build_app(AppState::new(storage, tasks).await.unwrap());

    let metadata = app
        .clone()
        .oneshot(
            Request::get(format!("/kobo/{key}/v1/library/{}/metadata", book.id))
                .header(header::HOST, "127.0.0.1:4567")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    let metadata = body_json(metadata).await;
    assert_eq!(metadata[0]["DownloadUrls"][0]["Format"], "KEPUB");
    assert!(
        metadata[0]["DownloadUrls"][0]["Url"]
            .as_str()
            .unwrap()
            .ends_with("?convert_kepub=false")
    );

    let source = fs::read(book.url.to_file_path().unwrap()).unwrap();
    let response = app
        .oneshot(
            Request::get(format!(
                "/kobo/{key}/v1/books/{}/file/epub?convert_kepub=false",
                book.id
            ))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, source);
}
