use super::*;
use std::collections::BTreeSet;

use chrono::NaiveDate;
use komga_core::{Author, CollectionCreation, SeriesStatus};

#[tokio::test]
async fn deprecated_content_queries_match_modern_search_and_enforce_legacy_contracts() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    for name in ["Alpha", "Beta"] {
        let directory = root.join(name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Volume 1.cbz"), [1_u8]).unwrap();
    }
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();

    let mut series = storage.list_series_by_library_id(&library.id).await.unwrap();
    series.sort_by(|left, right| left.name.cmp(&right.name));
    let alpha = &series[0];
    let beta = &series[1];
    let alpha_book = storage.list_books_by_series_id(&alpha.id).await.unwrap().remove(0);

    let mut metadata = storage.find_series_metadata(&alpha.id).await.unwrap().unwrap();
    metadata.status = SeriesStatus::Ended;
    metadata.title = "Alpha Adventures".into();
    metadata.title_sort = "Alpha Adventures".into();
    metadata.publisher = "Publisher".into();
    metadata.language = "en".into();
    metadata.age_rating = Some(18);
    metadata.genres = BTreeSet::from(["Fantasy".into()]);
    metadata.tags = BTreeSet::from(["SeriesTag".into()]);
    metadata.sharing_labels = BTreeSet::from(["adult".into()]);
    metadata.total_book_count = Some(1);
    storage.save_series_metadata(&metadata).await.unwrap();

    let mut book_metadata = storage.find_book_metadata(&alpha_book.id).await.unwrap().unwrap();
    book_metadata.authors = vec![Author {
        name: "Doe, Jane".into(),
        role: "writer".into(),
    }];
    book_metadata.tags = BTreeSet::from(["BookTag".into()]);
    book_metadata.release_date = NaiveDate::from_ymd_opt(2024, 5, 1);
    storage.save_book_metadata(&book_metadata).await.unwrap();
    storage.rebuild_book_metadata_aggregation(&alpha.id).await.unwrap();
    storage
        .create_collection(
            "featured",
            &CollectionCreation {
                name: "Featured".into(),
                ordered: false,
                series_ids: vec![alpha.id.clone()],
            },
        )
        .await
        .unwrap();

    let app = app_with_storage(storage.clone()).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    for path in ["/api/v1/series", "/api/v1/books", "/api/v1/series/alphabetical-groups"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    let filters = format!(
        "library_id={},missing&collection_id=featured&status=ENDED&read_status=UNREAD&publisher=Missing,Publisher&language=en&genre=Fantasy&tag=SeriesTag&age_rating=18&release_year=nope,2024&sharing_label=adult&deleted=no&complete=yes&oneshot=0&author=Doe%2C+Jane%2Cwriter",
        library.id
    );
    let filtered = get_page(&app, &format!("/api/v1/series?{filters}"), &admin).await;
    assert_eq!(filtered["totalElements"], 1);
    assert_eq!(filtered["content"][0]["id"], alpha.id);
    assert!(
        filtered["content"][0]["url"]
            .as_str()
            .is_some_and(|url| !url.is_empty())
    );

    let null_age = get_page(&app, "/api/v1/series?age_rating=not-a-number", &admin).await;
    assert_eq!(null_age["totalElements"], 1);
    assert_eq!(null_age["content"][0]["id"], beta.id);

    let searched = get_page(
        &app,
        "/api/v1/series?search=adventures&page=-1&size=9999&sort=unknown,asc",
        &admin,
    )
    .await;
    assert_eq!(searched["totalElements"], 1);
    assert_eq!(searched["content"][0]["id"], alpha.id);
    assert_eq!(searched["size"], 2_000);
    assert_eq!(searched["number"], 0);
    assert_eq!(searched["sort"]["sorted"], false);

    let modern = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list?sort=metadata.titleSort,asc")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "condition": {"libraryId": {"operator": "is", "value": library.id}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let modern = body_json(modern).await;
    let legacy = get_page(
        &app,
        &format!("/api/v1/series?library_id={}&sort=metadata.titleSort,asc", library.id),
        &admin,
    )
    .await;
    assert_eq!(page_ids(&legacy), page_ids(&modern));
    assert_eq!(legacy["totalElements"], modern["totalElements"]);

    let groups = get_json(
        &app,
        &format!(
            "/api/v1/series/alphabetical-groups?library_id={}&publisher=Publisher",
            library.id
        ),
        &admin,
    )
    .await;
    assert_eq!(groups, serde_json::json!([{"group": "a", "count": 1}]));

    let books = get_page(
        &app,
        &format!(
            "/api/v1/books?library_id={}&media_status=UNKNOWN&read_status=UNREAD&released_after=2024-04-30&tag=BookTag&sort=metadata.releaseDate,desc",
            library.id
        ),
        &admin,
    )
    .await;
    assert_eq!(books["totalElements"], 1);
    assert_eq!(books["content"][0]["id"], alpha_book.id);
    assert_eq!(books["sort"]["sorted"], true);

    let strict_date = get_page(&app, "/api/v1/books?released_after=2024-05-01", &admin).await;
    assert_eq!(strict_date["totalElements"], 0);

    let unpaged = get_page(
        &app,
        &format!("/api/v1/books?library_id={}&unpaged=on", library.id),
        &admin,
    )
    .await;
    assert_eq!(unpaged["numberOfElements"], 2);
    assert_eq!(unpaged["size"], 20);

    let random = get_page(&app, "/api/v1/series?sort=random,desc", &admin).await;
    assert_eq!(random["totalElements"], 2);
    assert_eq!(random["sort"]["sorted"], true);

    for path in [
        "/api/v1/series?status=INVALID",
        "/api/v1/series?deleted=maybe",
        "/api/v1/books?media_status=INVALID",
        "/api/v1/books?released_after=2024-02-30",
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
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }

    for (email, age_restriction) in [
        ("reader@example.org", serde_json::Value::Null),
        (
            "restricted@example.org",
            serde_json::json!({"age": 12, "restriction": "ALLOW_ONLY"}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/v2/users")
                    .header(header::AUTHORIZATION, &admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "email": email,
                            "password": "reader-password",
                            "ageRestriction": age_restriction
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    let reader = basic("reader@example.org", "reader-password");
    let reader_series = get_page(&app, &format!("/api/v1/series?library_id={}", library.id), &reader).await;
    assert_eq!(reader_series["content"][0]["url"], "");
    let reader_books = get_page(&app, &format!("/api/v1/books?library_id={}", library.id), &reader).await;
    assert!(reader_books["content"].as_array().unwrap().iter().all(|book| {
        book["url"]
            .as_str()
            .is_some_and(|url| !url.contains('/') && !url.contains('\\'))
    }));

    let restricted = basic("restricted@example.org", "reader-password");
    let restricted_series = get_page(&app, &format!("/api/v1/series?library_id={}", library.id), &restricted).await;
    assert_eq!(restricted_series["totalElements"], 0);
    let restricted_groups = get_json(
        &app,
        &format!("/api/v1/series/alphabetical-groups?library_id={}", library.id),
        &restricted,
    )
    .await;
    assert_eq!(restricted_groups, serde_json::json!([]));
}

async fn get_page(app: &axum::Router, path: &str, authorization: &str) -> serde_json::Value {
    get_json(app, path, authorization).await
}

async fn get_json(app: &axum::Router, path: &str, authorization: &str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(path)
                .header(header::AUTHORIZATION, authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    body_json(response).await
}

fn page_ids(page: &serde_json::Value) -> Vec<&str> {
    page["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect()
}
