use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use komga_core::{
    AgeRestriction, AllowExclude, Book, ContentRestrictions, Library, Media, ThumbnailBook, User, UserRole, new_id,
    scan_library_files,
};
use komga_media::{analyze_book, generate_thumbnail, image_dimension};
use komga_server::{AppState, REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER, build_app};
use komga_storage::{Storage, TaskStorage};
use serde_json::Value;
use std::{fs, fs::File, io::Write, path::PathBuf, time::Duration};
use tower::ServiceExt;
use url::Url;
use zip::{ZipWriter, write::SimpleFileOptions};

async fn test_app() -> Router {
    app_with_storage(Storage::connect_memory().await.unwrap()).await
}

async fn app_with_storage(storage: Storage) -> Router {
    let tasks = TaskStorage::connect_memory().await.unwrap();
    build_app(AppState::new(storage, tasks).await.unwrap())
}

fn basic(email: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{email}:{password}")))
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn body_bytes(response: Response<Body>) -> Vec<u8> {
    response.into_body().collect().await.unwrap().to_bytes().to_vec()
}

async fn body_frames(response: Response<Body>) -> (usize, Vec<u8>) {
    let mut body = response.into_body();
    let mut frame_count = 0;
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.unwrap().into_data() {
            frame_count += 1;
            bytes.extend_from_slice(&data);
        }
    }
    (frame_count, bytes)
}

async fn claim(app: &Router) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::post("/api/v1/claim")
                .header("X-Komga-Email", "admin@example.org")
                .header("X-Komga-Password", "password")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn session_cookie(response: &Response<Body>) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{SESSION_COOKIE}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

async fn analyzed_media_fixture(
    fixture_path: &str,
    file_name: &str,
) -> (tempfile::TempDir, Storage, TaskStorage, Book, Media) {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../komga/komga/src/test/resources")
        .join(fixture_path);
    fs::copy(fixture, root.join("Series").join(file_name)).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, true, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();
    (temporary, storage, tasks, book, media)
}

#[tokio::test]
async fn server_can_be_claimed_only_once() {
    let app = test_app().await;
    let status = app
        .clone()
        .oneshot(Request::get("/api/v1/claim").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(body_json(status).await, serde_json::json!({"isClaimed": false}));

    let response = claim(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let user = body_json(response).await;
    assert_eq!(user["email"], "admin@example.org");
    assert!(user["roles"].as_array().unwrap().iter().any(|role| role == "ADMIN"));

    assert_eq!(claim(&app).await.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn basic_login_issues_cookie_and_session_authenticates_follow_up_requests() {
    let app = test_app().await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v2/users/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized.headers()[header::WWW_AUTHENTICATE],
        "Basic realm=\"Komga\""
    );

    let login = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("ADMIN@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = session_cookie(&login);

    let me = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    assert_eq!(body_json(me).await["email"], "admin@example.org");
}

#[tokio::test]
async fn header_sessions_can_be_exchanged_for_cookies_and_logged_out() {
    let app = test_app().await;
    claim(&app).await;
    let login = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me?remember-me=true")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(SESSION_HEADER, "")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let session_id = login.headers()[SESSION_HEADER].to_str().unwrap().to_owned();
    assert!(
        login
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value.to_str().unwrap().starts_with(REMEMBER_ME_COOKIE))
    );

    let exchange = app
        .clone()
        .oneshot(
            Request::get("/api/v1/login/set-cookie")
                .header(SESSION_HEADER, &session_id)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(exchange.status(), StatusCode::NO_CONTENT);
    let cookie = session_cookie(&exchange);

    let logout = app
        .clone()
        .oneshot(
            Request::post("/api/logout")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert!(
        logout
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value.to_str().unwrap().contains("Max-Age=0"))
    );

    let expired = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_keys_are_returned_once_then_work_as_credentials() {
    let app = test_app().await;
    claim(&app).await;
    let login = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = session_cookie(&login);

    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v2/users/me/api-keys")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"comment":"test client"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let plain_key = body_json(created).await["key"].as_str().unwrap().to_owned();
    assert_eq!(plain_key.len(), 32);

    let listed = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me/api-keys")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(listed).await[0]["key"], "******");

    let with_key = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header("X-API-Key", plain_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(with_key.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_can_create_and_restrict_users_but_cannot_mutate_itself() {
    let app = test_app().await;
    let claimed = claim(&app).await;
    let admin_id = body_json(claimed).await["id"].as_str().unwrap().to_owned();
    let credentials = basic("admin@example.org", "password");

    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v2/users")
                .header(header::AUTHORIZATION, &credentials)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{
                        "email":"reader@example.org",
                        "password":"reader-password",
                        "roles":["PAGE_STREAMING","UNKNOWN"],
                        "labelsAllow":[" Kids "],
                        "labelsExclude":["ADULT"],
                        "ageRestriction":{"age":12,"restriction":"ALLOW_ONLY"}
                    }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let reader = body_json(created).await;
    assert_eq!(reader["labelsAllow"], serde_json::json!(["kids"]));
    assert_eq!(reader["labelsExclude"], serde_json::json!(["adult"]));
    assert_eq!(reader["ageRestriction"]["restriction"], "ALLOW_ONLY");

    let self_patch = app
        .clone()
        .oneshot(
            Request::patch(format!("/api/v2/users/{admin_id}"))
                .header(header::AUTHORIZATION, credentials)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"roles":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(self_patch.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn basic_auth_accepts_spring_bcrypt_2a_hashes() {
    let storage = Storage::connect_memory().await.unwrap();
    let hash = bcrypt::hash("password", 10).unwrap().replacen("$2b$", "$2a$", 1);
    let user = komga_core::User::new("kotlin@example.org".into(), hash);
    storage.create_user(&user).await.unwrap();
    let app = app_with_storage(storage).await;

    let response = app
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("kotlin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn library_management_bootstrap_settings_and_tasks_are_compatible() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let app = build_app(AppState::new(storage.clone(), tasks.clone()).await.unwrap());
    claim(&app).await;
    let admin = basic("admin@example.org", "password");
    let root = tempfile::tempdir().unwrap();
    let create_body = serde_json::json!({
        "name": "Comics",
        "root": root.path().to_string_lossy(),
        "scanDirectoryExclusions": ["tmp"]
    });
    let created = app
        .clone()
        .oneshot(
            Request::post("/api/v1/libraries")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let library = body_json(created).await;
    let library_id = library["id"].as_str().unwrap();
    assert_eq!(library["scanInterval"], "EVERY_6H");
    assert_eq!(library["root"], root.path().to_string_lossy().as_ref());
    assert_eq!(tasks.list().await.unwrap().len(), 1);

    let patched = app
        .clone()
        .oneshot(
            Request::patch(format!("/api/v1/libraries/{library_id}"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"scanDirectoryExclusions":null,"oneshotsDirectory":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patched.status(), StatusCode::NO_CONTENT);
    let loaded = storage.find_library_by_id(library_id).await.unwrap().unwrap();
    assert!(loaded.scan_directory_exclusions.is_empty());
    assert_eq!(loaded.oneshots_directory, None);

    let deep_scan = app
        .clone()
        .oneshot(
            Request::post(format!("/api/v1/libraries/{library_id}/scan?deep=true"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deep_scan.status(), StatusCode::ACCEPTED);
    assert!(
        tasks
            .list()
            .await
            .unwrap()
            .iter()
            .any(|task| { task.id == format!("SCAN_LIBRARY_{library_id}_DEEP_true") && task.priority == 8 })
    );

    let global_patch = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/client-settings/global")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"login.banner":{"value":"hello","allowUnauthorized":true},"private":{"value":"secret","allowUnauthorized":false}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(global_patch.status(), StatusCode::NO_CONTENT);
    let anonymous_settings = app
        .clone()
        .oneshot(
            Request::get("/api/v1/client-settings/global/list")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let anonymous_settings = body_json(anonymous_settings).await;
    assert!(anonymous_settings.get("login.banner").is_some());
    assert!(anonymous_settings.get("private").is_none());

    let oauth = app
        .clone()
        .oneshot(Request::get("/api/v1/oauth2/providers").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(body_json(oauth).await, serde_json::json!([]));

    let listing = app
        .clone()
        .oneshot(
            Request::post("/api/v1/filesystem")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"path": root.path()}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);

    let reader_created = app
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
    assert_eq!(reader_created.status(), StatusCode::CREATED);
    let reader_libraries = app
        .clone()
        .oneshot(
            Request::get("/api/v1/libraries")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(reader_libraries).await[0]["root"], "");

    let cleared = app
        .clone()
        .oneshot(
            Request::delete("/api/v1/tasks")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(body_json(cleared).await.as_u64().unwrap() >= 2);
    assert!(tasks.list().await.unwrap().is_empty());

    let deleted = app
        .oneshot(
            Request::delete(format!("/api/v1/libraries/{library_id}"))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn content_queries_page_filter_restrict_and_navigate_books() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let series_directory = root.join("Series");
    fs::create_dir_all(&series_directory).unwrap();
    fs::write(series_directory.join("Volume 1.cbz"), [1_u8]).unwrap();
    fs::write(series_directory.join("Volume 2.cbz"), [1_u8, 2]).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();

    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let series_response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list?page=0&size=1&sort=metadata.titleSort,asc")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(series_response.status(), StatusCode::OK);
    let series_page = body_json(series_response).await;
    assert_eq!(series_page["totalElements"], 1);
    assert_eq!(series_page["totalPages"], 1);
    assert_eq!(series_page["numberOfElements"], 1);
    assert_eq!(series_page["pageable"]["pageSize"], 1);
    assert_eq!(series_page["sort"]["sorted"], true);
    let series_id = series_page["content"][0]["id"].as_str().unwrap().to_owned();
    assert!(series_page["content"][0]["url"].as_str().unwrap().contains("/Series"));

    let books_response = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/list?unpaged=true&sort=metadata.numberSort,asc")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "condition": {
                            "allOf": [
                                {"seriesId": {"operator": "is", "value": series_id}},
                                {"mediaStatus": {"operator": "is", "value": "UNKNOWN"}}
                            ]
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(books_response.status(), StatusCode::OK);
    let books_page = body_json(books_response).await;
    assert_eq!(books_page["totalElements"], 2);
    assert_eq!(books_page["size"], 20);
    let first_id = books_page["content"][0]["id"].as_str().unwrap().to_owned();
    let second_id = books_page["content"][1]["id"].as_str().unwrap().to_owned();

    let next = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/next"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(next.status(), StatusCode::OK);
    assert_eq!(body_json(next).await["id"], second_id);
    let previous = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/previous"))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(previous.status(), StatusCode::NOT_FOUND);

    for (email, restriction) in [
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
                            "ageRestriction": restriction
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
    let reader_series = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{series_id}"))
                .header(header::AUTHORIZATION, &reader)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reader_series.status(), StatusCode::OK);
    assert_eq!(body_json(reader_series).await["url"], "");
    let reader_book = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}"))
                .header(header::AUTHORIZATION, reader)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reader_book.status(), StatusCode::OK);
    assert_eq!(body_json(reader_book).await["url"], "Volume 1.cbz");

    let restricted = basic("restricted@example.org", "reader-password");
    let restricted_list = app
        .clone()
        .oneshot(
            Request::post("/api/v1/series/list")
                .header(header::AUTHORIZATION, &restricted)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restricted_list.status(), StatusCode::OK);
    assert_eq!(body_json(restricted_list).await["totalElements"], 0);
    let restricted_detail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/series/{series_id}"))
                .header(header::AUTHORIZATION, &restricted)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restricted_detail.status(), StatusCode::FORBIDDEN);

    let restricted_manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/manifest"))
                .header(header::AUTHORIZATION, restricted)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(restricted_manifest.status(), StatusCode::FORBIDDEN);

    let unanalyzed_manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{first_id}/manifest"))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unanalyzed_manifest.status(), StatusCode::NOT_FOUND);

    let unsupported = app
        .clone()
        .oneshot(
            Request::post("/api/v1/books/list")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"fullTextSearch":"volume"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unsupported.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn book_file_download_streams_source_bytes_with_kotlin_headers_and_alias() {
    let (_temporary, storage, tasks, book, _media) =
        analyzed_media_fixture("archives/zip.zip", "アキラ 大判.cbz").await;
    let source_path = book.url.to_file_path().unwrap();
    let source = (0..(2 * 1024 * 1024 + 127))
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    fs::write(&source_path, &source).unwrap();

    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let credentials = basic("admin@example.org", "password");
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file", book.id))
                .header(header::AUTHORIZATION, &credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/zip");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], source.len().to_string());
    let expected_disposition = concat!(
        "attachment; filename=\"=?UTF-8?Q?=E3=82=A2=E3=82=AD=E3=83=A9_=E5=A4=A7=E5=88=A4.cbz?=\"; ",
        "filename*=UTF-8''%E3%82%A2%E3%82%AD%E3%83%A9%20%E5%A4%A7%E5%88%A4.cbz"
    );
    assert_eq!(response.headers()[header::CONTENT_DISPOSITION], expected_disposition);
    let (frame_count, downloaded) = body_frames(response).await;
    assert!(
        frame_count > 1,
        "large source file should be emitted in multiple frames"
    );
    assert_eq!(downloaded, source);

    let alias = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file/client/supplied.cbz", book.id))
                .header(header::AUTHORIZATION, credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alias.status(), StatusCode::OK);
    assert_eq!(alias.headers()[header::CONTENT_DISPOSITION], expected_disposition);
    assert_eq!(body_bytes(alias).await, source);
}

#[tokio::test]
async fn book_file_download_uses_analyzed_media_types_and_unknown_fallback() {
    for (fixture, file_name, expected_media_type) in [
        ("pdf/komga.pdf", "Volume.pdf", "application/pdf"),
        ("archives/epub3.epub", "Volume.epub", "application/epub+zip"),
    ] {
        let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture(fixture, file_name).await;
        let app = build_app(AppState::new(storage, tasks).await.unwrap());
        assert_eq!(claim(&app).await.status(), StatusCode::OK);
        let response = app
            .oneshot(
                Request::get(format!("/api/v1/books/{}/file", book.id))
                    .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], expected_media_type);
        assert!(!body_bytes(response).await.is_empty());
    }

    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let series_directory = root.join("Series");
    fs::create_dir_all(&series_directory).unwrap();
    let source = b"not analyzed yet";
    fs::write(series_directory.join("Volume.cbz"), source).unwrap();
    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let response = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(body_bytes(response).await, source);
}

#[tokio::test]
async fn book_file_download_enforces_role_library_age_and_label_restrictions() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let app = build_app(AppState::new(storage.clone(), tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let password_hash = bcrypt::hash("reader-password", 4).unwrap();

    let no_role = User::new("no-role@example.org".into(), password_hash.clone());
    storage.create_user(&no_role).await.unwrap();

    let mut no_library = User::new("no-library@example.org".into(), password_hash.clone());
    no_library.roles.insert(UserRole::FileDownload);
    no_library.shared_all_libraries = false;
    storage.create_user(&no_library).await.unwrap();

    let mut age_restricted = User::new("age@example.org".into(), password_hash.clone());
    age_restricted.roles.insert(UserRole::FileDownload);
    age_restricted.restrictions = ContentRestrictions::new(
        Some(AgeRestriction {
            age: 12,
            restriction: AllowExclude::AllowOnly,
        }),
        Vec::<String>::new(),
        Vec::<String>::new(),
    );
    storage.create_user(&age_restricted).await.unwrap();

    let mut label_restricted = User::new("label@example.org".into(), password_hash);
    label_restricted.roles.insert(UserRole::FileDownload);
    label_restricted.restrictions = ContentRestrictions::new(None, ["kids".to_owned()], Vec::<String>::new());
    storage.create_user(&label_restricted).await.unwrap();

    let path = format!("/api/v1/books/{}/file", book.id);
    assert_eq!(
        app.clone()
            .oneshot(Request::get(path.as_str()).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    for email in [
        "no-role@example.org",
        "no-library@example.org",
        "age@example.org",
        "label@example.org",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(path.as_str())
                    .header(header::AUTHORIZATION, basic(email, "reader-password"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "unexpected status for {email}"
        );
    }
}

#[tokio::test]
async fn book_file_download_reports_unknown_and_moved_files() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume.cbz").await;
    let source_path = book.url.to_file_path().unwrap();
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let credentials = basic("admin@example.org", "password");

    let unknown = app
        .clone()
        .oneshot(
            Request::get("/api/v1/books/missing/file")
                .header(header::AUTHORIZATION, &credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(unknown).await["message"], "Book not found");

    fs::remove_file(source_path).unwrap();
    let moved = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/file", book.id))
                .header(header::AUTHORIZATION, credentials)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(moved.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(moved).await["message"], "File not found, it may have moved");
}

#[tokio::test]
async fn cbz_pages_conversion_thumbnail_cache_and_manual_analysis_match_contract() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/zip.zip", "Volume 1.cbz").await;
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

    let app = build_app(AppState::new(storage, tasks.clone()).await.unwrap());
    let unauthorized = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let pages = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pages.status(), StatusCode::OK);
    let pages = body_json(pages).await;
    assert_eq!(pages[0]["number"], 1);
    assert_eq!(pages[0]["fileName"], "komga.png");
    assert_eq!(pages[0]["mediaType"], "image/png");
    assert_eq!(pages[0]["width"], 48);
    assert_eq!(pages[0]["height"], 48);
    assert_eq!(pages[0]["sizeBytes"], 3108);

    let page = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/png");
    assert!(page.headers().contains_key(header::CONTENT_DISPOSITION));
    let last_modified = page.headers()[header::LAST_MODIFIED].to_str().unwrap().to_owned();
    assert!(body_bytes(page).await.starts_with(b"\x89PNG"));

    let cached = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);

    let converted = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/v1/books/{}/pages/0?zero_based=true&convert=jpeg",
                book.id
            ))
            .header(header::AUTHORIZATION, &admin)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(converted.status(), StatusCode::OK);
    assert_eq!(converted.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(converted).await.starts_with(&[0xff, 0xd8]));

    let page_thumbnail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1/thumbnail", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page_thumbnail.status(), StatusCode::OK);
    assert_eq!(page_thumbnail.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(page_thumbnail).await.starts_with(&[0xff, 0xd8]));

    let book_thumbnail = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/thumbnail", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(book_thumbnail.status(), StatusCode::OK);
    assert_eq!(book_thumbnail.headers()[header::CONTENT_TYPE], "image/jpeg");

    let invalid_page = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/0", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_page.status(), StatusCode::BAD_REQUEST);

    let raw = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1/raw", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::BAD_REQUEST);

    let analyze = app
        .oneshot(
            Request::post(format!("/api/v1/books/{}/analyze", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(analyze.status(), StatusCode::ACCEPTED);
    let queued = tasks.list().await.unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].simple_type, "AnalyzeBook");
    assert_eq!(queued[0].group_id.as_deref(), Some(book.series_id.as_str()));
    assert_eq!(queued[0].priority, 6);
}

#[tokio::test]
async fn divina_webpub_manifest_matches_kotlin_contract() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/zip.zip", "Volume 1.cbz").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());

    let unauthorized = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header("forwarded", "for=192.0.2.10;proto=https;host=reader.example")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/divina+json");
    assert!(!manifest.headers().contains_key(header::LAST_MODIFIED));
    let manifest = body_json(manifest).await;
    assert_eq!(
        manifest["context"],
        "https://readium.org/webpub-manifest/context.jsonld"
    );
    assert_eq!(manifest["metadata"]["title"], "Volume 1");
    assert_eq!(
        manifest["metadata"]["conformsTo"],
        "https://readium.org/webpub-manifest/profiles/divina"
    );
    assert_eq!(manifest["metadata"]["numberOfPages"], 1);
    assert_eq!(manifest["metadata"]["belongsTo"]["series"][0]["name"], "Series");
    assert_eq!(
        manifest["links"][0],
        serde_json::json!({
            "rel": "self",
            "href": format!("https://reader.example/komga/api/v1/books/{}/manifest", book.id),
            "type": "application/divina+json"
        })
    );
    assert_eq!(manifest["links"][1]["rel"], "http://opds-spec.org/acquisition");
    assert_eq!(manifest["links"][1]["type"], "application/vnd.comicbook+zip");
    assert_eq!(
        manifest["readingOrder"][0]["href"],
        format!(
            "https://reader.example/komga/api/v1/books/{}/pages/1?contentNegotiation=false",
            book.id
        )
    );
    assert_eq!(manifest["readingOrder"][0]["type"], "image/png");
    assert_eq!(manifest["readingOrder"][0]["width"], 48);
    assert_eq!(manifest["resources"][0]["type"], "image/jpeg");
    assert_eq!(manifest["images"], serde_json::json!([]));
    assert_eq!(manifest["toc"], serde_json::json!([]));

    let mismatch = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/pdf", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn divina_manifest_advertises_jpeg_alternate_for_convertible_pages() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let archive_path = root.join("Series/Volume.cbz");
    let mut writer = ZipWriter::new(File::create(&archive_path).unwrap());
    writer.start_file("page.webp", SimpleFileOptions::default()).unwrap();
    writer
        .write_all(
            &fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../komga/komga/src/test/resources/hashpage/e-sou.webp/1.webp"),
            )
            .unwrap(),
        )
        .unwrap();
    writer.finish().unwrap();

    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, true, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();

    let app = app_with_storage(storage).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let manifest = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    let manifest = body_json(manifest).await;
    assert_eq!(manifest["readingOrder"][0]["type"], "image/webp");
    assert_eq!(manifest["readingOrder"][0]["alternate"][0]["type"], "image/jpeg");
    assert!(
        manifest["readingOrder"][0]["alternate"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("?contentNegotiation=false&convert=jpeg")
    );
}

#[tokio::test]
async fn pdf_webpub_and_divina_manifests_match_kotlin_contract() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("pdf/komga.pdf", "Volume.pdf").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example:25600")
                .header(header::IF_MODIFIED_SINCE, "Wed, 21 Oct 2099 07:28:00 GMT")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/webpub+json");
    assert!(!manifest.headers().contains_key(header::LAST_MODIFIED));
    let manifest = body_json(manifest).await;
    assert_eq!(
        manifest["metadata"]["conformsTo"],
        "https://readium.org/webpub-manifest/profiles/pdf"
    );
    assert_eq!(manifest["readingOrder"].as_array().unwrap().len(), 1);
    assert_eq!(
        manifest["readingOrder"][0]["href"],
        format!("http://reader.example:25600/api/v1/books/{}/pages/1/raw", book.id)
    );
    assert_eq!(manifest["readingOrder"][0]["type"], "application/pdf");
    assert_eq!(manifest["links"][1]["type"], "application/divina+json");
    assert!(
        manifest["links"][1]["href"]
            .as_str()
            .unwrap()
            .ends_with("/manifest/divina")
    );
    assert_eq!(manifest["links"][2]["type"], "application/pdf");

    let divina = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/divina", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(divina.status(), StatusCode::OK);
    assert_eq!(divina.headers()[header::CONTENT_TYPE], "application/divina+json");
    let divina = body_json(divina).await;
    assert_eq!(divina["readingOrder"][0]["type"], "image/jpeg");
    assert_eq!(divina["readingOrder"][0]["width"], 3200);

    let mismatch = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/epub", book.id))
                .header(header::AUTHORIZATION, admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn epub_webpub_manifest_preserves_spine_navigation_and_resources() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("archives/epub3.epub", "Volume.epub").await;
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let manifest = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest/epub", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(manifest.status(), StatusCode::OK);
    assert_eq!(manifest.headers()[header::CONTENT_TYPE], "application/webpub+json");
    let manifest = body_json(manifest).await;
    assert_eq!(
        manifest["metadata"]["conformsTo"],
        "https://readium.org/webpub-manifest/profiles/epub"
    );
    assert_eq!(manifest["metadata"]["rendition"]["layout"], "fixed");
    assert!(
        manifest["readingOrder"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/titlepage.xhtml")
    );
    assert!(
        manifest["readingOrder"][1]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/page_1.xhtml")
    );
    assert_eq!(manifest["readingOrder"][0]["type"], "application/xhtml+xml");
    assert_eq!(manifest["resources"][0]["type"], "image/jpeg");
    assert!(
        manifest["resources"][1]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/page_styles.css")
    );
    assert!(
        manifest["toc"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/page_1.xhtml")
    );
    assert_eq!(manifest["toc"][0]["title"], "Page 1");
    assert!(
        manifest["landmarks"][0]["href"]
            .as_str()
            .unwrap()
            .ends_with("/resource/titlepage.xhtml")
    );
    assert_eq!(manifest["links"][1]["type"], "application/divina+json");
    assert_eq!(manifest["links"][2]["type"], "application/epub+zip");

    let generic = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/manifest", book.id))
                .header(header::AUTHORIZATION, admin)
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generic.status(), StatusCode::OK);
    assert_eq!(generic.headers()[header::CONTENT_TYPE], "application/webpub+json");
}

#[tokio::test]
async fn epub_pages_resources_positions_and_cache_match_contract() {
    let (_temporary, storage, tasks, book, media) = analyzed_media_fixture("archives/epub3.epub", "Volume.epub").await;
    assert!(media.epub_divina_compatible);
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let unauthorized_resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/nav.xhtml", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized_resource.status(), StatusCode::UNAUTHORIZED);

    let resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/nav.xhtml", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resource.status(), StatusCode::OK);
    assert_eq!(resource.headers()[header::CONTENT_TYPE], "application/xhtml+xml");
    assert_eq!(
        resource.headers()[header::CONTENT_SECURITY_POLICY],
        "script-src 'none'; object-src 'none';"
    );
    assert!(
        resource.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains("nav.xhtml")
    );
    let last_modified = resource.headers()[header::LAST_MODIFIED].to_str().unwrap().to_owned();
    assert!(
        body_bytes(resource)
            .await
            .windows(10)
            .any(|bytes| bytes == b"Navigation")
    );

    let cached_resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/nav.xhtml", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_MODIFIED_SINCE, &last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached_resource.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        cached_resource.headers()[header::CONTENT_SECURITY_POLICY],
        "script-src 'none'; object-src 'none';"
    );

    let missing_resource = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/META-INF/container.xml", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing_resource.status(), StatusCode::NOT_FOUND);

    let positions = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/positions", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(positions.status(), StatusCode::OK);
    assert_eq!(
        positions.headers()[header::CONTENT_TYPE],
        "application/vnd.readium.position-list+json"
    );
    let positions_last_modified = positions.headers()[header::LAST_MODIFIED].to_str().unwrap().to_owned();
    let positions = body_json(positions).await;
    assert_eq!(positions["total"], 2);
    assert_eq!(positions["positions"][0]["href"], "titlepage.xhtml");
    assert_eq!(positions["positions"][1]["locations"]["totalProgression"], 1.0);

    let cached_positions = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/positions", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::IF_MODIFIED_SINCE, positions_last_modified)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cached_positions.status(), StatusCode::NOT_MODIFIED);

    let pages = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pages.status(), StatusCode::OK);
    let pages = body_json(pages).await;
    assert_eq!(pages.as_array().unwrap().len(), 2);
    assert_eq!(pages[0]["fileName"], "cover.jpeg");

    let page = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/2", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CONTENT_TYPE], "image/png");
    assert!(body_bytes(page).await.starts_with(b"\x89PNG"));
}

#[tokio::test]
async fn epub_fonts_are_anonymous_but_document_resources_require_authentication() {
    let storage = Storage::connect_memory().await.unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("Series")).unwrap();
    let path = root.join("Series/Fonts.epub");
    let mut writer = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default();
    writer.start_file("mimetype", options).unwrap();
    writer.write_all(b"application/epub+zip").unwrap();
    writer.start_file("META-INF/container.xml", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
        )
        .unwrap();
    writer.start_file("content.opf", options).unwrap();
    writer
        .write_all(
            br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="page" href="page.xhtml" media-type="application/xhtml+xml"/><item id="font" href="font.ttf" media-type="font/ttf"/></manifest><spine><itemref idref="page"/></spine></package>"#,
        )
        .unwrap();
    writer.start_file("page.xhtml", options).unwrap();
    writer
        .write_all(b"<html><body>reflowable document content</body></html>")
        .unwrap();
    writer.start_file("font.ttf", options).unwrap();
    writer.write_all(b"fixture-font").unwrap();
    writer.finish().unwrap();

    let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
    storage.create_library(&library).await.unwrap();
    let scan = scan_library_files(&library, || false).unwrap();
    storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
    let series = storage.list_series_by_library_id(&library.id).await.unwrap();
    let book = storage.list_books_by_series_id(&series[0].id).await.unwrap().remove(0);
    let media = analyze_book(&book, false, || false).unwrap();
    storage.replace_media_analysis(&media).await.unwrap();
    let app = app_with_storage(storage).await;

    let font = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/font.ttf", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(font.status(), StatusCode::OK);
    assert_eq!(font.headers()[header::CONTENT_TYPE], "font/ttf");
    assert_eq!(body_bytes(font).await, b"fixture-font");

    let document = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/resource/page.xhtml", book.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(document.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pdf_pages_render_raw_and_negotiate_like_kotlin() {
    let (_temporary, storage, tasks, book, _media) = analyzed_media_fixture("pdf/komga.pdf", "Volume.pdf").await;

    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let pages = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pages.status(), StatusCode::OK);
    let pages = body_json(pages).await;
    assert_eq!(pages[0]["fileName"], "1");
    assert_eq!(pages[0]["mediaType"], "image/jpeg");
    assert_eq!(pages[0]["width"], 3200);
    assert_eq!(pages[0]["height"], 3200);
    assert!(pages[0]["sizeBytes"].is_null());

    let rendered = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rendered.status(), StatusCode::OK);
    assert_eq!(rendered.headers()[header::CONTENT_TYPE], "image/jpeg");
    assert!(body_bytes(rendered).await.starts_with(&[0xff, 0xd8]));

    let negotiated = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1?convert=png", book.id))
                .header(header::AUTHORIZATION, &admin)
                .header(header::ACCEPT, "application/pdf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(negotiated.status(), StatusCode::OK);
    assert_eq!(negotiated.headers()[header::CONTENT_TYPE], "application/pdf");
    assert!(body_bytes(negotiated).await.starts_with(b"%PDF-"));

    let negotiation_disabled = app
        .clone()
        .oneshot(
            Request::get(format!(
                "/api/v1/books/{}/pages/1?contentNegotiation=false&convert=png",
                book.id
            ))
            .header(header::AUTHORIZATION, &admin)
            .header(header::ACCEPT, "application/pdf")
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(negotiation_disabled.status(), StatusCode::OK);
    assert_eq!(negotiation_disabled.headers()[header::CONTENT_TYPE], "image/png");
    assert!(body_bytes(negotiation_disabled).await.starts_with(b"\x89PNG"));

    let raw = app
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/1/raw", book.id))
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::OK);
    assert_eq!(raw.headers()[header::CONTENT_TYPE], "application/pdf");
    assert!(
        raw.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .ends_with(".pdf\"")
    );
    assert!(body_bytes(raw).await.starts_with(b"%PDF-"));

    let invalid_raw = app
        .oneshot(
            Request::get(format!("/api/v1/books/{}/pages/2/raw", book.id))
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_raw.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn task_queue_sse_requires_authentication_and_reports_admin_counts() {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    tasks
        .save(&komga_storage::TaskRecord::scan_library("library", false, 4))
        .await
        .unwrap();
    tasks
        .save(&komga_storage::TaskRecord::empty_trash("library", 6))
        .await
        .unwrap();
    let app = build_app(AppState::new(storage, tasks).await.unwrap());
    claim(&app).await;

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/sse/v1/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::get("/sse/v1/events")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .expect("SSE did not produce an initial task status")
        .unwrap()
        .unwrap();
    let event = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(event.contains("event: TaskQueueStatus"));
    assert!(event.contains(r#""count":2"#));
    assert!(event.contains(r#""EmptyTrash":1"#));
    assert!(event.contains(r#""ScanLibrary":1"#));
}
