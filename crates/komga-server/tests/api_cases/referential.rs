use super::*;
use std::collections::BTreeSet;

use chrono::NaiveDate;
use komga_core::{Author, CollectionCreation, ReadListCreation};

#[tokio::test]
async fn referential_routes_enforce_sources_scope_priority_authorization_and_order() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let mut libraries = Vec::new();
    for (library_name, series_names) in [("Allowed", vec!["Primary", "Empty"]), ("Hidden", vec!["Hidden"])] {
        let root = temporary.path().join(library_name);
        for series_name in series_names {
            let directory = root.join(series_name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("Volume 1.cbz"), [1_u8]).unwrap();
        }
        let library = Library::new(library_name.into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        libraries.push(library);
    }

    let allowed_id = libraries[0].id.clone();
    let hidden_id = libraries[1].id.clone();
    let mut allowed_series = storage.list_series_by_library_id(&allowed_id).await.unwrap();
    allowed_series.sort_by(|left, right| left.name.cmp(&right.name));
    let primary = allowed_series.iter().find(|series| series.name == "Primary").unwrap();
    let primary_book = storage.list_books_by_series_id(&primary.id).await.unwrap().remove(0);
    let hidden = storage.list_series_by_library_id(&hidden_id).await.unwrap().remove(0);
    let hidden_book = storage.list_books_by_series_id(&hidden.id).await.unwrap().remove(0);

    let mut metadata = storage.find_series_metadata(&primary.id).await.unwrap().unwrap();
    metadata.publisher = "Publisher".into();
    metadata.language = "en".into();
    metadata.age_rating = Some(18);
    metadata.genres = BTreeSet::from(["Fantasy".into()]);
    metadata.tags = BTreeSet::from(["SeriesTag".into()]);
    metadata.sharing_labels = BTreeSet::from(["adult".into()]);
    storage.save_series_metadata(&metadata).await.unwrap();

    let mut book_metadata = storage.find_book_metadata(&primary_book.id).await.unwrap().unwrap();
    book_metadata.authors = vec![Author {
        name: "José Alvarez".into(),
        role: "writer".into(),
    }];
    book_metadata.tags = BTreeSet::from(["BookTag".into()]);
    book_metadata.release_date = NaiveDate::from_ymd_opt(2024, 5, 1);
    storage.save_book_metadata(&book_metadata).await.unwrap();
    storage.rebuild_book_metadata_aggregation(&primary.id).await.unwrap();
    book_metadata.authors = vec![Author {
        name: "Direct Only".into(),
        role: "editor".into(),
    }];
    storage.save_book_metadata(&book_metadata).await.unwrap();

    let mut hidden_metadata = storage.find_book_metadata(&hidden_book.id).await.unwrap().unwrap();
    hidden_metadata.authors = vec![Author {
        name: "Hidden Author".into(),
        role: "artist".into(),
    }];
    storage.save_book_metadata(&hidden_metadata).await.unwrap();
    storage.rebuild_book_metadata_aggregation(&hidden.id).await.unwrap();

    storage
        .create_collection(
            "collection",
            &CollectionCreation {
                name: "Collection".into(),
                ordered: false,
                series_ids: vec![primary.id.clone()],
            },
        )
        .await
        .unwrap();
    storage
        .create_read_list(
            "readlist",
            &ReadListCreation {
                name: "Read List".into(),
                summary: String::new(),
                ordered: true,
                book_ids: vec![primary_book.id.clone()],
            },
        )
        .await
        .unwrap();

    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");
    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v2/users")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "email": "reader@example.org",
                        "password": "reader-password",
                        "sharedLibraries": {"all": false, "libraryIds": [allowed_id]},
                        "ageRestriction": {"age": 10, "restriction": "EXCLUDE"},
                        "labelsExclude": ["adult"]
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let reader = basic("reader@example.org", "reader-password");

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/genres").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let v1_global = get_json(&app, "/api/v1/authors?search=direct", &reader).await;
    assert_eq!(
        v1_global,
        serde_json::json!([{"name": "Direct Only", "role": "editor"}])
    );
    let v1_scoped = get_json(
        &app,
        &format!(
            "/api/v1/authors?collection_id=collection&series_id={}&search=jose",
            hidden.id
        ),
        &reader,
    )
    .await;
    assert_eq!(
        v1_scoped,
        serde_json::json!([{"name": "José Alvarez", "role": "writer"}])
    );
    let libraries_win = get_json(
        &app,
        &format!("/api/v1/authors?library_id={hidden_id}&collection_id=collection"),
        &reader,
    )
    .await;
    assert_eq!(libraries_win, serde_json::json!([]));

    let v2 = get_json(&app, "/api/v2/authors?search=direct&role=editor&page=0&size=1", &admin).await;
    assert_eq!(v2["totalElements"], 1);
    assert_eq!(v2["totalPages"], 1);
    assert_eq!(v2["numberOfElements"], 1);
    assert_eq!(v2["sort"]["sorted"], true);
    assert_eq!(v2["content"][0]["name"], "Direct Only");
    let v2_unpaged = get_json(&app, "/api/v2/authors?readlist_id=readlist&unpaged=true", &admin).await;
    assert_eq!(v2_unpaged["size"], 20);
    assert_eq!(v2_unpaged["content"][0]["name"], "Direct Only");
    assert_eq!(
        get_json(&app, "/api/v2/authors?search=direct", &reader).await["totalElements"],
        0
    );

    for (path, expected) in [
        (
            "/api/v2/authors/names?search=direct",
            serde_json::json!(["Direct Only"]),
        ),
        ("/api/v2/authors/roles", serde_json::json!(["artist", "editor"])),
        ("/api/v2/genres?search=fant", serde_json::json!(["Fantasy"])),
        ("/api/v2/sharing-labels", serde_json::json!(["adult"])),
        ("/api/v2/languages", serde_json::json!(["en"])),
        ("/api/v2/publishers", serde_json::json!(["Publisher"])),
        ("/api/v2/tags?include=SERIES", serde_json::json!(["SeriesTag"])),
        ("/api/v2/tags?include=BOOK", serde_json::json!(["BookTag"])),
        ("/api/v2/tags?include=BOTH", serde_json::json!(["BookTag", "SeriesTag"])),
        ("/api/v2/series/release-years", serde_json::json!(["2024"])),
        ("/api/v2/age-ratings", serde_json::json!([18])),
    ] {
        let page = get_json(&app, path, &admin).await;
        assert_eq!(page["content"], expected, "{path}");
        assert_eq!(page["sort"]["sorted"], true, "{path}");
    }

    let restricted_genres = get_json(&app, "/api/v2/genres", &reader).await;
    assert_eq!(restricted_genres["content"], serde_json::json!([]));
    let multi_collection = get_json(
        &app,
        "/api/v2/genres?collection_id=missing&collection_id=collection",
        &admin,
    )
    .await;
    assert_eq!(multi_collection["content"], serde_json::json!(["Fantasy"]));
    let v2_libraries_win = get_json(
        &app,
        &format!("/api/v2/genres?library_id={hidden_id}&collection_id=collection"),
        &admin,
    )
    .await;
    assert_eq!(v2_libraries_win["content"], serde_json::json!([]));
    let read_list_tags = get_json(&app, "/api/v2/tags?readlist_id=readlist&include=BOOK", &admin).await;
    assert_eq!(read_list_tags["content"], serde_json::json!(["BookTag"]));

    let invalid_include = app
        .clone()
        .oneshot(
            Request::get("/api/v2/tags?include=UNKNOWN")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_include.status(), StatusCode::BAD_REQUEST);

    assert_eq!(
        get_json(&app, "/api/v1/authors/names?search=direct", &reader).await,
        serde_json::json!(["Direct Only"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/authors/roles", &reader).await,
        serde_json::json!(["editor"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/genres", &reader).await,
        serde_json::json!(["Fantasy"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/sharing-labels", &reader).await,
        serde_json::json!(["adult"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/tags", &reader).await,
        serde_json::json!(["BookTag", "SeriesTag"])
    );
    assert_eq!(
        get_json(
            &app,
            &format!(
                "/api/v1/tags/book?series_id={}&readlist_id=missing&library_id={hidden_id}",
                primary.id
            ),
            &reader,
        )
        .await,
        serde_json::json!(["BookTag"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/tags/series?collection_id=collection", &reader).await,
        serde_json::json!(["SeriesTag"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/languages", &reader).await,
        serde_json::json!(["en"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/publishers", &reader).await,
        serde_json::json!(["Publisher"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/age-ratings", &reader).await,
        serde_json::json!(["None", "18"])
    );
    assert_eq!(
        get_json(&app, "/api/v1/series/release-dates", &reader).await,
        serde_json::json!(["2024"])
    );

    let genres_priority = get_json(
        &app,
        &format!("/api/v1/genres?library_id={hidden_id}&collection_id=collection"),
        &reader,
    )
    .await;
    assert_eq!(genres_priority, serde_json::json!([]));
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
