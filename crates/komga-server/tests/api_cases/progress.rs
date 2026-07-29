use super::*;

#[tokio::test]
async fn traditional_read_progress_validates_persists_completes_and_deletes() {
    let (_temporary, storage, tasks, book, mut media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    media.page_count = 10;
    storage.replace_media_analysis(&media).await.unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    let path = format!("/api/v1/books/{}/read-progress", book.id);

    let unauthorized = app
        .clone()
        .oneshot(
            Request::patch(&path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"page":5}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    for invalid in [r#"{}"#, r#"{"completed":false}"#, r#"{"page":0}"#, r#"{"page":11}"#] {
        let response = app
            .clone()
            .oneshot(
                Request::patch(&path)
                    .header(header::AUTHORIZATION, &admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(invalid))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "payload: {invalid}");
    }

    let missing = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/books/missing/read-progress")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"page":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(missing).await["message"], "Book not found");

    let updated = app
        .clone()
        .oneshot(
            Request::patch(&path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"page":5}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);
    let detail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let detail = body_json(detail).await;
    assert_eq!(detail["readProgress"]["page"], 5);
    assert_eq!(detail["readProgress"]["completed"], false);

    let progression = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/progression", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(progression.status(), StatusCode::OK);
    assert_eq!(
        progression.headers()[header::CONTENT_TYPE],
        "application/vnd.readium.progression+json"
    );
    let progression = body_json(progression).await;
    assert_eq!(progression["locator"]["href"], "");
    assert_eq!(progression["locator"]["type"], "");

    let completed = app
        .clone()
        .oneshot(
            Request::patch(&path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"page":0,"completed":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::NO_CONTENT);
    let saved = storage
        .find_read_progress(
            &book.id,
            &storage
                .find_user_by_email("admin@example.org")
                .await
                .unwrap()
                .unwrap()
                .id,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.page, 10);
    assert!(saved.completed);

    let password_hash = bcrypt::hash("reader-password", 4).unwrap();
    let mut restricted = User::new("restricted-progress@example.org".into(), password_hash);
    restricted.restrictions = ContentRestrictions::new(
        Some(AgeRestriction {
            age: 12,
            restriction: AllowExclude::AllowOnly,
        }),
        Vec::<String>::new(),
        Vec::<String>::new(),
    );
    storage.create_user(&restricted).await.unwrap();
    let forbidden = app
        .clone()
        .oneshot(
            Request::patch(&path)
                .header(
                    header::AUTHORIZATION,
                    basic("restricted-progress@example.org", "reader-password"),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"page":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    for _ in 0..2 {
        let deleted = app
            .clone()
            .oneshot(
                Request::delete(&path)
                    .header(header::AUTHORIZATION, &admin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    }
    let no_progression = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/progression", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(no_progression.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn series_and_tachiyomi_read_progress_contract() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    for number in 1..=3 {
        fs::write(root.join("Series").join(format!("Book {number}.cbz")), [number]).unwrap();
    }
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    storage
        .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
        .await
        .unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
    let books = storage.list_books_by_series_id(&series.id).await.unwrap();
    for (index, book) in books.iter().enumerate() {
        let mut metadata = storage.find_book_metadata(&book.id).await.unwrap().unwrap();
        metadata.number_sort = index as f32 + 1.5;
        storage.save_book_metadata(&metadata).await.unwrap();
        let mut media = storage.find_media_by_book_id(&book.id).await.unwrap().unwrap();
        media.page_count = (index as i32 + 1) * 10;
        storage.replace_media_analysis(&media).await.unwrap();
    }

    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    let v1 = format!("/api/v1/series/{}/read-progress", series.id);
    let tachiyomi = format!("/api/v2/series/{}/read-progress/tachiyomi", series.id);
    assert_eq!(
        app.clone()
            .oneshot(Request::get(&tachiyomi).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let initial = app
        .clone()
        .oneshot(
            Request::get(&tachiyomi)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json(initial).await,
        serde_json::json!({
            "booksCount": 3,
            "booksReadCount": 0,
            "booksUnreadCount": 3,
            "booksInProgressCount": 0,
            "lastReadContinuousNumberSort": 0.0,
            "maxNumberSort": 3.5
        })
    );

    for invalid in [r#"{}"#, r#"{"lastBookNumberSortRead":"bad"}"#, "not-json"] {
        let response = app
            .clone()
            .oneshot(
                Request::put(&tachiyomi)
                    .header(header::AUTHORIZATION, &admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(invalid))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let admin_user = storage.find_user_by_email("admin@example.org").await.unwrap().unwrap();
    assert_eq!(
        storage
            .find_series_read_progress(&series.id, &admin_user.id)
            .await
            .unwrap()
            .books_read_count,
        0
    );

    let partial = app
        .clone()
        .oneshot(
            Request::put(&tachiyomi)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"lastBookNumberSortRead":2.5}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(partial.status(), StatusCode::NO_CONTENT);
    let summary = storage
        .find_series_read_progress(&series.id, &admin_user.id)
        .await
        .unwrap();
    assert_eq!((summary.books_read_count, summary.books_unread_count), (2, 1));
    assert_eq!(summary.last_read_continuous_number_sort, 2.5);

    assert_eq!(
        app.clone()
            .oneshot(
                Request::post(&v1)
                    .header(header::AUTHORIZATION, &admin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        storage
            .find_series_read_progress(&series.id, &admin_user.id)
            .await
            .unwrap()
            .books_read_count,
        3
    );
    for _ in 0..2 {
        assert_eq!(
            app.clone()
                .oneshot(
                    Request::delete(&v1)
                        .header(header::AUTHORIZATION, &admin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    let missing = app
        .clone()
        .oneshot(
            Request::get("/api/v2/series/missing/read-progress/tachiyomi")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let password_hash = bcrypt::hash("reader-password", 4).unwrap();
    let mut restricted = User::new("restricted-series-progress@example.org".into(), password_hash);
    restricted.shared_all_libraries = false;
    storage.create_user(&restricted).await.unwrap();
    let forbidden = app
        .oneshot(
            Request::get(&tachiyomi)
                .header(
                    header::AUTHORIZATION,
                    basic("restricted-series-progress@example.org", "reader-password"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn pdf_progression_enforces_positions_and_newer_timestamps() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("pdf/komga.pdf", "Volume.pdf").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let path = format!("/api/v1/books/{}/progression", book.id);

    let empty = app
        .clone()
        .oneshot(
            Request::get(&path)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(empty.status(), StatusCode::NO_CONTENT);

    let request = serde_json::json!({
        "modified": "2025-01-02T03:04:05Z",
        "device": {"id": "tablet", "name": "Reader"},
        "locator": {
            "href": "",
            "type": "application/pdf",
            "locations": {"position": 1, "progression": 0.5, "totalProgression": 1.0}
        }
    });
    let updated = app
        .clone()
        .oneshot(
            Request::put(&path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(
            Request::get(&path)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/vnd.readium.progression+json"
    );
    let response = body_json(response).await;
    assert_eq!(response["modified"], "2025-01-02T03:04:05Z");
    assert_eq!(response["device"]["id"], "tablet");
    assert_eq!(response["locator"]["locations"]["position"], 1);

    let conflict = app
        .clone()
        .oneshot(
            Request::put(&path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(conflict).await["message"],
        "Progression is older than existing"
    );

    let invalid = app
        .oneshot(
            Request::put(&path)
                .header(header::AUTHORIZATION, admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "modified": "2025-01-02T03:04:06Z",
                        "device": {"id": "tablet", "name": "Reader"},
                        "locator": {"href": "", "type": "application/pdf", "locations": {"position": 2}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn epub_progression_normalizes_resources_and_page_updates_use_positions() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/epub3.epub", "Volume.epub").await;
    let MediaExtension::Epub(extension) = media.extension.as_ref().unwrap();
    let first = extension.positions[0].clone();
    let second = extension.positions[1].clone();
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let path = format!("/api/v1/books/{}/progression", book.id);

    let updated = app
        .clone()
        .oneshot(
            Request::put(&path)
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "modified": "2025-02-03T04:05:06+02:00",
                        "device": {"id": "epub-reader", "name": "Tablet"},
                        "locator": {
                            "href": format!("{}#fragment", first.href),
                            "type": "application/octet-stream",
                            "locations": {"position": 99, "progression": 0.75, "totalProgression": 0.1}
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);

    let response = app
        .clone()
        .oneshot(
            Request::get(&path)
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let response = body_json(response).await;
    assert_eq!(response["modified"], "2025-02-03T02:05:06Z");
    assert_eq!(response["locator"]["type"], first.media_type);
    assert_eq!(
        response["locator"]["locations"]["totalProgression"],
        serde_json::json!(first.locations.as_ref().unwrap().total_progression.unwrap())
    );

    let page_update = app
        .oneshot(
            Request::patch(format!("/api/v1/books/{}/read-progress", book.id))
                .header(header::AUTHORIZATION, admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"page":2}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page_update.status(), StatusCode::NO_CONTENT);
    let admin = storage.find_user_by_email("admin@example.org").await.unwrap().unwrap();
    let progress = storage.find_read_progress(&book.id, &admin.id).await.unwrap().unwrap();
    assert_eq!(progress.page, 2);
    assert!(progress.completed);
    assert_eq!(progress.locator, Some(second));
}
