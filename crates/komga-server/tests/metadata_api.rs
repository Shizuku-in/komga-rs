use std::{collections::BTreeMap, fs};

use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use komga_core::{Book, Library, ScanInterval, Series, TitleSortSource, User, scan_library_files};
use komga_server::{AppState, build_app};
use komga_storage::{Storage, TaskStorage};
use serde_json::Value;
use tower::ServiceExt;
use url::Url;

struct MetadataFixture {
    _temporary: tempfile::TempDir,
    app: Router,
    storage: Storage,
    tasks: TaskStorage,
    admin: String,
    reader: String,
    series: Vec<Series>,
    books: Vec<Vec<Book>>,
}

async fn metadata_fixture() -> MetadataFixture {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    for (series, books) in [
        ("Series A", ["Book 1.cbz", "Book 2.cbz"]),
        ("Series B", ["Book 3.cbz", "Book 4.cbz"]),
    ] {
        let directory = root.join(series);
        fs::create_dir_all(&directory).unwrap();
        for book in books {
            fs::write(directory.join(book), [1_u8]).unwrap();
        }
    }
    let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    library.scan_interval = ScanInterval::Disabled;
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let mut books = Vec::new();
    for item in &series {
        books.push(storage.list_books_by_series_id(&item.id).await.unwrap());
    }

    let app = build_app(AppState::new(storage.clone(), tasks.clone()).await.unwrap());
    let claimed = app
        .clone()
        .oneshot(
            Request::post("/api/v1/claim")
                .header("X-Komga-Email", "admin@example.org")
                .header("X-Komga-Password", "password")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claimed.status(), StatusCode::OK);
    let reader = User::new("reader@example.org".into(), bcrypt::hash("reader-password", 4).unwrap());
    storage.create_user(&reader).await.unwrap();

    MetadataFixture {
        _temporary: temporary,
        app,
        storage,
        tasks,
        admin: basic("admin@example.org", "password"),
        reader: basic("reader@example.org", "reader-password"),
        series,
        books,
    }
}

fn basic(email: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{email}:{password}")))
}

async fn patch_json(app: &Router, path: &str, authorization: Option<&str>, payload: Value) -> Response<Body> {
    let mut request = Request::patch(path).header(header::CONTENT_TYPE, "application/json");
    if let Some(authorization) = authorization {
        request = request.header(header::AUTHORIZATION, authorization);
    }
    app.clone()
        .oneshot(request.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap()
}

async fn search_ids(app: &Router, authorization: &str, entity: &str, query: &str) -> Vec<String> {
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/{entity}/list"))
                .header(header::AUTHORIZATION, authorization)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({ "fullTextSearch": query }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    body["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

async fn response_json(response: Response<Body>) -> Value {
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .unwrap()
        .to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
    serde_json::to_value(entries.into_iter().collect::<BTreeMap<_, _>>()).unwrap()
}

#[tokio::test]
async fn metadata_routes_require_an_administrator_and_report_missing_ids() {
    let fixture = metadata_fixture().await;
    let book_path = format!("/api/v1/books/{}/metadata", fixture.books[0][0].id);
    let unauthorized = patch_json(&fixture.app, &book_path, None, serde_json::json!({})).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let forbidden = patch_json(&fixture.app, &book_path, Some(&fixture.reader), serde_json::json!({})).await;
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let missing_book = patch_json(
        &fixture.app,
        "/api/v1/books/missing/metadata",
        Some(&fixture.admin),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(missing_book.status(), StatusCode::NOT_FOUND);
    let missing_series = patch_json(
        &fixture.app,
        "/api/v1/series/missing/metadata",
        Some(&fixture.admin),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(missing_series.status(), StatusCode::NOT_FOUND);
    assert!(fixture.tasks.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn book_metadata_patch_normalizes_values_and_preserves_or_clears_nulls() {
    let fixture = metadata_fixture().await;
    let book = &fixture.books[0][0];
    let original_title = fixture
        .storage
        .find_book_metadata(&book.id)
        .await
        .unwrap()
        .unwrap()
        .title;
    assert!(
        search_ids(&fixture.app, &fixture.admin, "books", &format!("\"{original_title}\""))
            .await
            .contains(&book.id)
    );
    let path = format!("/api/v1/books/{}/metadata", book.id);
    let response = patch_json(
        &fixture.app,
        &path,
        Some(&fixture.admin),
        serde_json::json!({
            "title": "  Updated title  ",
            "titleLock": true,
            "summary": "  Summary  ",
            "summaryLock": true,
            "number": " 12 ",
            "numberLock": true,
            "numberSort": 0.1,
            "numberSortLock": true,
            "releaseDate": "2025-02-03",
            "releaseDateLock": true,
            "authors": [{"name": " Jane Doe ", "role": " Writer "}],
            "authorsLock": true,
            "tags": [" Fantasy ", "FANTASY", ""],
            "tagsLock": true,
            "isbn": "978-161-729-045-9abc",
            "isbnLock": true,
            "links": [{"label": "Source", "url": "https://example.org/book"}],
            "linksLock": true
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        search_ids(&fixture.app, &fixture.admin, "books", "\"Updated title\"")
            .await
            .contains(&book.id)
    );
    assert!(
        !search_ids(&fixture.app, &fixture.admin, "books", &format!("\"{original_title}\""))
            .await
            .contains(&book.id)
    );

    let metadata = fixture.storage.find_book_metadata(&book.id).await.unwrap().unwrap();
    assert_eq!(metadata.title, "Updated title");
    assert_eq!(metadata.summary, "Summary");
    assert_eq!(metadata.number, "12");
    assert_eq!(metadata.number_sort, 0.1_f32);
    assert_eq!(metadata.release_date.unwrap().to_string(), "2025-02-03");
    assert_eq!(metadata.authors[0].name, "Jane Doe");
    assert_eq!(metadata.authors[0].role, "writer");
    assert_eq!(metadata.tags.iter().cloned().collect::<Vec<_>>(), ["fantasy"]);
    assert_eq!(metadata.isbn, "9781617290459");
    assert_eq!(metadata.links[0].url, "https://example.org/book");
    assert!(metadata.title_lock && metadata.summary_lock && metadata.links_lock);

    let tasks = fixture.tasks.list().await.unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, format!("AGGREGATE_SERIES_METADATA_{}", book.series_id));
    assert_eq!(tasks[0].simple_type, "AggregateSeriesMetadata");
    assert_eq!(tasks[0].group_id.as_deref(), Some(book.series_id.as_str()));
    assert_eq!(tasks[0].priority, 4);

    let cleared = patch_json(
        &fixture.app,
        &path,
        Some(&fixture.admin),
        serde_json::json!({
            "title": null,
            "titleLock": null,
            "summary": null,
            "summaryLock": null,
            "number": null,
            "numberSort": null,
            "releaseDate": null,
            "authors": null,
            "tags": null,
            "isbn": null,
            "links": null
        }),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
    let metadata = fixture.storage.find_book_metadata(&book.id).await.unwrap().unwrap();
    assert_eq!(metadata.title, "Updated title");
    assert_eq!(metadata.number, "12");
    assert_eq!(metadata.number_sort, 0.1_f32);
    assert!(metadata.title_lock && metadata.summary_lock);
    assert!(metadata.summary.is_empty());
    assert!(metadata.release_date.is_none());
    assert!(metadata.authors.is_empty());
    assert!(metadata.tags.is_empty());
    assert!(metadata.isbn.is_empty());
    assert!(metadata.links.is_empty());
    assert_eq!(fixture.tasks.list().await.unwrap().len(), 1);

    let preserved = metadata;
    for invalid in [
        serde_json::json!({"title": "  "}),
        serde_json::json!({"number": ""}),
        serde_json::json!({"releaseDate": "not-a-date"}),
        serde_json::json!({"authors": [{"name": "", "role": "writer"}]}),
        serde_json::json!({"isbn": "1617290459"}),
        serde_json::json!({"links": [{"label": "Source", "url": "/relative"}]}),
    ] {
        let response = patch_json(&fixture.app, &path, Some(&fixture.admin), invalid).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        fixture.storage.find_book_metadata(&book.id).await.unwrap().unwrap(),
        preserved
    );
    assert_eq!(fixture.tasks.list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn bulk_book_metadata_validates_before_writes_and_deduplicates_series_tasks() {
    let fixture = metadata_fixture().await;
    let first = &fixture.books[0][0];
    let second = &fixture.books[0][1];
    let other_series = &fixture.books[1][0];
    let first_original = fixture.storage.find_book_metadata(&first.id).await.unwrap().unwrap();
    let second_original = fixture.storage.find_book_metadata(&second.id).await.unwrap().unwrap();

    let invalid = patch_json(
        &fixture.app,
        "/api/v1/books/metadata",
        Some(&fixture.admin),
        object([
            (first.id.clone(), serde_json::json!({"title": "Would change"})),
            (second.id.clone(), serde_json::json!({"number": "  "})),
        ]),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        fixture.storage.find_book_metadata(&first.id).await.unwrap().unwrap(),
        first_original
    );
    assert_eq!(
        fixture.storage.find_book_metadata(&second.id).await.unwrap().unwrap(),
        second_original
    );
    assert!(fixture.tasks.list().await.unwrap().is_empty());

    let updated = patch_json(
        &fixture.app,
        "/api/v1/books/metadata",
        Some(&fixture.admin),
        object([
            (first.id.clone(), serde_json::json!({"title": "First"})),
            (second.id.clone(), serde_json::json!({"title": "Second"})),
            (other_series.id.clone(), serde_json::json!({"title": "Other"})),
            ("missing".into(), serde_json::json!({"title": "Ignored"})),
        ]),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fixture
            .storage
            .find_book_metadata(&first.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "First"
    );
    assert_eq!(
        fixture
            .storage
            .find_book_metadata(&second.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Second"
    );
    assert_eq!(
        fixture
            .storage
            .find_book_metadata(&other_series.id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Other"
    );
    let tasks = fixture.tasks.list().await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks
            .iter()
            .map(|task| task.group_id.clone().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        [first.series_id.clone(), other_series.series_id.clone()].into()
    );

    let empty = patch_json(
        &fixture.app,
        "/api/v1/books/metadata",
        Some(&fixture.admin),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(empty.status(), StatusCode::NO_CONTENT);
    assert_eq!(fixture.tasks.list().await.unwrap().len(), 2);
}

#[tokio::test]
async fn series_metadata_patch_handles_nulls_normalization_and_validation() {
    let fixture = metadata_fixture().await;
    let series = &fixture.series[0];
    let original_title = fixture
        .storage
        .find_series_metadata(&series.id)
        .await
        .unwrap()
        .unwrap()
        .title;
    let path = format!("/api/v1/series/{}/metadata", series.id);
    let response = patch_json(
        &fixture.app,
        &path,
        Some(&fixture.admin),
        serde_json::json!({
            "status": "ENDED",
            "statusLock": true,
            "title": "  Updated series  ",
            "titleLock": true,
            "titleSort": "  Series updated  ",
            "titleSortLock": true,
            "summary": "  Series summary  ",
            "summaryLock": true,
            "publisher": "  Publisher  ",
            "publisherLock": true,
            "readingDirection": "RIGHT_TO_LEFT",
            "readingDirectionLock": true,
            "ageRating": 16,
            "ageRatingLock": true,
            "language": "fra",
            "languageLock": true,
            "genres": [" Fantasy ", "FANTASY"],
            "genresLock": true,
            "tags": [" Action "],
            "tagsLock": true,
            "totalBookCount": 12,
            "totalBookCountLock": true,
            "sharingLabels": [" Adults "],
            "sharingLabelsLock": true,
            "links": [{"label": "Source", "url": "https://example.org/series"}],
            "linksLock": true,
            "alternateTitles": [{"label": "fr", "title": "Titre"}],
            "alternateTitlesLock": true
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        search_ids(&fixture.app, &fixture.admin, "series", "\"Updated series\"")
            .await
            .contains(&series.id)
    );
    assert!(
        !search_ids(&fixture.app, &fixture.admin, "series", &format!("\"{original_title}\""))
            .await
            .contains(&series.id)
    );
    let metadata = fixture.storage.find_series_metadata(&series.id).await.unwrap().unwrap();
    assert_eq!(metadata.status.as_str(), "ENDED");
    assert_eq!(metadata.title, "Updated series");
    assert_eq!(metadata.title_sort, "Series updated");
    assert_eq!(metadata.title_sort_source, TitleSortSource::Custom);
    assert_eq!(metadata.summary, "Series summary");
    assert_eq!(metadata.publisher, "Publisher");
    assert_eq!(metadata.reading_direction.unwrap().as_str(), "RIGHT_TO_LEFT");
    assert_eq!(metadata.age_rating, Some(16));
    assert_eq!(metadata.language, "fr");
    assert_eq!(metadata.genres.iter().cloned().collect::<Vec<_>>(), ["fantasy"]);
    assert_eq!(metadata.tags.iter().cloned().collect::<Vec<_>>(), ["action"]);
    assert_eq!(metadata.total_book_count, Some(12));
    assert_eq!(metadata.sharing_labels.iter().cloned().collect::<Vec<_>>(), ["adults"]);
    assert!(metadata.status_lock && metadata.summary_lock && metadata.alternate_titles_lock);

    let cleared = patch_json(
        &fixture.app,
        &path,
        Some(&fixture.admin),
        serde_json::json!({
            "status": null,
            "statusLock": null,
            "title": null,
            "titleSort": null,
            "summary": null,
            "publisher": null,
            "language": null,
            "readingDirection": null,
            "ageRating": null,
            "genres": null,
            "tags": null,
            "totalBookCount": null,
            "sharingLabels": null,
            "links": null,
            "alternateTitles": null
        }),
    )
    .await;
    assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
    let preserved = fixture.storage.find_series_metadata(&series.id).await.unwrap().unwrap();
    assert_eq!(preserved.status.as_str(), "ENDED");
    assert_eq!(preserved.title, "Updated series");
    assert_eq!(preserved.title_sort, "Series updated");
    assert_eq!(preserved.summary, "Series summary");
    assert_eq!(preserved.publisher, "Publisher");
    assert_eq!(preserved.language, "fr");
    assert!(preserved.status_lock);
    assert!(preserved.reading_direction.is_none());
    assert!(preserved.age_rating.is_none());
    assert!(preserved.genres.is_empty());
    assert!(preserved.tags.is_empty());
    assert!(preserved.total_book_count.is_none());
    assert!(preserved.sharing_labels.is_empty());
    assert!(preserved.links.is_empty());
    assert!(preserved.alternate_titles.is_empty());
    assert!(fixture.tasks.list().await.unwrap().is_empty());

    for invalid in [
        serde_json::json!({"title": "  "}),
        serde_json::json!({"titleSort": ""}),
        serde_json::json!({"status": "UNKNOWN"}),
        serde_json::json!({"ageRating": -1}),
        serde_json::json!({"totalBookCount": 0}),
        serde_json::json!({"language": "english"}),
        serde_json::json!({"links": [{"label": "Source", "url": "/relative"}]}),
        serde_json::json!({"alternateTitles": [{"label": "", "title": "Title"}]}),
    ] {
        let response = patch_json(&fixture.app, &path, Some(&fixture.admin), invalid).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        fixture.storage.find_series_metadata(&series.id).await.unwrap().unwrap(),
        preserved
    );
}

#[tokio::test]
async fn language_aware_title_sort_updates_series_books_and_alphabetical_groups() {
    let fixture = metadata_fixture().await;
    let chinese_series = &fixture.series[0];
    let japanese_series = &fixture.series[1];

    for (book, title) in fixture.books[0].iter().zip(["重庆故事", "北京故事"]) {
        let response = patch_json(
            &fixture.app,
            &format!("/api/v1/books/{}/metadata", book.id),
            Some(&fixture.admin),
            serde_json::json!({"title": title}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    for (series, title, language) in [
        (chinese_series, "重庆森林", "zh-Hant"),
        (japanese_series, "安達としまむら", "ja-JP"),
    ] {
        let response = patch_json(
            &fixture.app,
            &format!("/api/v1/series/{}/metadata", series.id),
            Some(&fixture.admin),
            serde_json::json!({"title": title, "language": language}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    let chinese_metadata = fixture
        .storage
        .find_series_metadata(&chinese_series.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chinese_metadata.title_sort, "chongqing senlin");
    assert_eq!(chinese_metadata.title_sort_source, TitleSortSource::Generated);
    let japanese_metadata = fixture
        .storage
        .find_series_metadata(&japanese_series.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(japanese_metadata.title_sort, "adachi to shimamura");
    assert_eq!(
        japanese_metadata
            .alternate_titles
            .iter()
            .map(|title| (title.label.as_str(), title.title.as_str()))
            .collect::<Vec<_>>(),
        [("ja-Kana", "アダチトシマムラ"), ("ja-Hira", "あだちとしまむら"),]
    );
    for reading in ["あだち", "アダチ"] {
        assert!(
            search_ids(&fixture.app, &fixture.admin, "series", reading)
                .await
                .contains(&japanese_series.id),
            "{reading} should match the generated alternate title"
        );
    }

    let books = fixture
        .app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/v1/series/{}/books?sort=metadata.title,asc",
                chinese_series.id
            ))
            .header(header::AUTHORIZATION, &fixture.admin)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(books.status(), StatusCode::OK);
    let books = response_json(books).await;
    let titles = books["content"]
        .as_array()
        .unwrap()
        .iter()
        .map(|book| book["metadata"]["title"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(titles, ["北京故事", "重庆故事"]);
    assert!(
        books["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|book| book["metadata"].get("titleSort").is_none())
    );

    let series = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list?sort=metadata.titleSort,asc")
                .header(header::AUTHORIZATION, &fixture.admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let series = response_json(series).await;
    assert_eq!(series["content"][0]["metadata"]["title"], "安達としまむら");
    assert_eq!(series["content"][1]["metadata"]["title"], "重庆森林");

    let groups = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list/alphabetical-groups")
                .header(header::AUTHORIZATION, &fixture.admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response_json(groups).await,
        serde_json::json!([
            {"group": "a", "count": 1},
            {"group": "c", "count": 1}
        ])
    );
}
