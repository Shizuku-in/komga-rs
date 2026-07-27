use axum::Router;
use komga_core::{CollectionCreation, ReadListCreation};

use super::*;

#[tokio::test]
async fn search_contract_covers_modern_legacy_containers_sorting_and_visibility() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let mut libraries = Vec::new();
    let mut series_ids = Vec::new();
    let mut book_ids = Vec::new();

    for (library_name, series_name) in [("Allowed", "Batman Alpha"), ("Hidden", "Batman Zulu")] {
        let root = temporary.path().join(library_name);
        let directory = root.join(series_name);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join(format!("{series_name} 01.cbz")), [1_u8]).unwrap();
        let library = Library::new(library_name.into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        libraries.push(library);
        series_ids.push(series.id);
        book_ids.push(book.id);
    }

    storage
        .create_collection(
            "allowed-collection",
            &CollectionCreation {
                name: "Featured Shelf".into(),
                ordered: false,
                series_ids: vec![series_ids[0].clone()],
            },
        )
        .await
        .unwrap();
    storage
        .create_collection(
            "hidden-collection",
            &CollectionCreation {
                name: "Hidden Shelf".into(),
                ordered: false,
                series_ids: vec![series_ids[1].clone()],
            },
        )
        .await
        .unwrap();
    storage
        .create_read_list(
            "reading-list",
            &ReadListCreation {
                name: "Batman Reading".into(),
                summary: String::new(),
                ordered: true,
                book_ids: book_ids.clone(),
            },
        )
        .await
        .unwrap();

    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let modern_series = post_page(
        &app,
        "/api/v1/series/list?page=0&size=1&sort=metadata.titleSort,desc",
        &admin,
        r#"{"fullTextSearch":"batman"}"#,
    )
    .await;
    assert_eq!(modern_series["totalElements"], 2);
    assert_eq!(modern_series["content"][0]["id"], series_ids[1]);
    assert_eq!(modern_series["sort"]["sorted"], true);

    let legacy_series = get_page(&app, "/api/v1/series?search=batman", &admin).await;
    assert_eq!(legacy_series["totalElements"], 2);

    let modern_books = post_page(&app, "/api/v1/books/list", &admin, r#"{"fullTextSearch":"batman"}"#).await;
    let legacy_books = get_page(&app, "/api/v1/books?search=batman", &admin).await;
    assert_eq!(page_ids(&modern_books), page_ids(&legacy_books));
    assert_eq!(modern_books["totalElements"], 2);

    let collections = get_page(&app, "/api/v1/collections?search=shelf&sort=name,asc", &admin).await;
    assert_eq!(collections["totalElements"], 2);
    assert_eq!(collections["content"][0]["name"], "Featured Shelf");
    let read_lists = get_page(&app, "/api/v1/readlists?search=reading", &admin).await;
    assert_eq!(read_lists["totalElements"], 1);
    assert_eq!(read_lists["content"][0]["id"], "reading-list");

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
                        "sharedLibraries": {"all": false, "libraryIds": [libraries[0].id]}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let reader = basic("reader@example.org", "reader-password");
    let visible_series = post_page(&app, "/api/v1/series/list", &reader, r#"{"fullTextSearch":"batman"}"#).await;
    assert_eq!(visible_series["totalElements"], 1);
    assert_eq!(visible_series["content"][0]["id"], series_ids[0]);
    let visible_collections = get_page(&app, "/api/v1/collections?search=shelf", &reader).await;
    assert_eq!(visible_collections["totalElements"], 1);
    assert_eq!(visible_collections["content"][0]["id"], "allowed-collection");
}

async fn post_page(app: &Router, path: &str, authorization: &str, body: &'static str) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::post(path)
                .header(header::AUTHORIZATION, authorization)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    body_json(response).await
}

async fn get_page(app: &Router, path: &str, authorization: &str) -> serde_json::Value {
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
        .map(|value| value["id"].as_str().unwrap())
        .collect()
}
