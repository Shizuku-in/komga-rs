use super::*;
use komga_storage::{ServerSettingsPatch, SettingUpdate};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn server_settings_are_admin_only_and_follow_kotlin_patch_semantics() {
    let storage = Storage::connect_memory().await.unwrap();
    storage
        .update_server_settings(&ServerSettingsPatch {
            delete_empty_collections: Some(true),
            delete_empty_read_lists: Some(false),
            remember_me_duration_days: Some(5),
            thumbnail_size: Some(komga_core::ThumbnailSize::Large),
            task_pool_size: Some(4),
            server_port: SettingUpdate::Set(1_234),
            server_context_path: SettingUpdate::Set("/database".into()),
            kobo_proxy: Some(true),
            kobo_port: SettingUpdate::Set(4_321),
            kepubify_path: SettingUpdate::Set("/missing/kepubify".into()),
            ..ServerSettingsPatch::default()
        })
        .await
        .unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let state = AppState::new(storage.clone(), tasks)
        .await
        .unwrap()
        .with_kobo_proxy(false)
        .with_server_bind_settings(ServerBindSettings {
            configuration_port: 25_600,
            configuration_context_path: None,
            effective_port: Some(25_601),
            effective_context_path: String::new(),
        });
    let app = build_app(state);
    assert_eq!(claim(&app).await.status(), StatusCode::OK);
    let admin = basic("admin@example.org", "password");

    let unauthorized = app
        .clone()
        .oneshot(Request::get("/api/v1/settings").body(Body::empty()).unwrap())
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
            Request::get("/api/v1/settings")
                .header(header::AUTHORIZATION, basic("reader@example.org", "reader-password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

    let response = app
        .clone()
        .oneshot(
            Request::get("/api/v1/settings")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["deleteEmptyCollections"], true);
    assert_eq!(body["deleteEmptyReadLists"], false);
    assert_eq!(body["rememberMeDurationDays"], 5);
    assert_eq!(body["thumbnailSize"], "LARGE");
    assert_eq!(body["taskPoolSize"], 4);
    assert_eq!(body["serverPort"]["configurationSource"], 25_600);
    assert_eq!(body["serverPort"]["databaseSource"], 1_234);
    assert_eq!(body["serverPort"]["effectiveValue"], 25_601);
    assert_eq!(
        body["serverContextPath"]["configurationSource"],
        serde_json::Value::Null
    );
    assert_eq!(body["serverContextPath"]["databaseSource"], "/database");
    assert_eq!(body["serverContextPath"]["effectiveValue"], "");
    assert_eq!(body["koboProxy"], true);
    assert_eq!(body["koboPort"], 4_321);
    assert_eq!(body["kepubifyPath"]["databaseSource"], "/missing/kepubify");
    assert_eq!(body["kepubifyPath"]["effectiveValue"], serde_json::Value::Null);

    let updated = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/settings")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{
                        "deleteEmptyCollections":false,
                        "rememberMeDurationDays":15,
                        "thumbnailSize":"MEDIUM",
                        "taskPoolSize":2,
                        "serverPort":5678,
                        "serverContextPath":"/komga-hyphen/subpath123",
                        "koboProxy":false,
                        "koboPort":6789,
                        "kepubifyPath":null
                    }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(updated.status(), StatusCode::NO_CONTENT);
    let persisted = storage.server_settings().await.unwrap();
    assert!(!persisted.delete_empty_collections);
    assert!(!persisted.delete_empty_read_lists);
    assert_eq!(persisted.remember_me_duration_days, 15);
    assert_eq!(persisted.thumbnail_size, komga_core::ThumbnailSize::Medium);
    assert_eq!(persisted.task_pool_size, 2);
    assert_eq!(persisted.server_port, Some(5_678));
    assert_eq!(
        persisted.server_context_path.as_deref(),
        Some("/komga-hyphen/subpath123")
    );
    assert_eq!(persisted.kobo_proxy, Some(false));
    assert_eq!(persisted.kobo_port, Some(6_789));
    assert_eq!(persisted.kepubify_path, None);

    let ignored_and_cleared = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/settings")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{
                        "deleteEmptyCollections":null,
                        "rememberMeDurationDays":null,
                        "thumbnailSize":null,
                        "taskPoolSize":null,
                        "serverPort":null,
                        "serverContextPath":null,
                        "koboPort":null
                    }"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ignored_and_cleared.status(), StatusCode::NO_CONTENT);
    let persisted = storage.server_settings().await.unwrap();
    assert!(!persisted.delete_empty_collections);
    assert_eq!(persisted.remember_me_duration_days, 15);
    assert_eq!(persisted.thumbnail_size, komga_core::ThumbnailSize::Medium);
    assert_eq!(persisted.task_pool_size, 2);
    assert_eq!(persisted.server_port, None);
    assert_eq!(persisted.server_context_path, None);
    assert_eq!(persisted.kobo_port, None);
}

#[tokio::test]
async fn invalid_server_settings_are_rejected_before_any_write() {
    let storage = Storage::connect_memory().await.unwrap();
    let app = app_with_storage(storage.clone()).await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");
    let original = storage.server_settings().await.unwrap();
    for invalid in [
        r#"{"rememberMeDurationDays":0}"#,
        r#"{"rememberMeDurationDays":-1}"#,
        r#"{"thumbnailSize":"HUGE"}"#,
        r#"{"taskPoolSize":0}"#,
        r#"{"taskPoolSize":-1}"#,
        r#"{"serverPort":0}"#,
        r#"{"serverPort":65536}"#,
        r#"{"koboPort":0}"#,
        r#"{"serverContextPath":"noSlashBegin"}"#,
        r#"{"serverContextPath":"/slashEnd/"}"#,
        r#"{"serverContextPath":"/invalid=character"}"#,
        r#"{"serverContextPath":"/invalid/end-"}"#,
        r#"{"serverContextPath":"/invalid/end_"}"#,
        r#"{"serverContextPath":"/日本語"}"#,
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::patch("/api/v1/settings")
                    .header(header::AUTHORIZATION, &admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(invalid))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{invalid}");
        assert_eq!(storage.server_settings().await.unwrap(), original, "{invalid}");
    }
}

#[tokio::test]
async fn remember_me_updates_affect_new_and_rotated_long_lived_sessions_only() {
    let app = test_app().await;
    claim(&app).await;
    let admin = basic("admin@example.org", "password");
    let remembered_login = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me?remember-me=true")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let remembered_cookie = remembered_login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{REMEMBER_ME_COOKIE}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let ordinary_login = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, &admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ordinary_cookie = session_cookie(&ordinary_login);

    let rotated = app
        .clone()
        .oneshot(
            Request::patch("/api/v1/settings")
                .header(header::AUTHORIZATION, &admin)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"rememberMeDurationDays":2,"renewRememberMeKey":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rotated.status(), StatusCode::NO_CONTENT);

    let expired = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, remembered_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    let ordinary = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, ordinary_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ordinary.status(), StatusCode::OK);

    let new_remembered = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me?remember-me=true")
                .header(header::AUTHORIZATION, admin)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        new_remembered
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.starts_with(&format!("{REMEMBER_ME_COOKIE}=")) && value.contains("Max-Age=172800"))
    );
}

#[tokio::test]
async fn context_path_and_kobo_public_port_apply_without_overriding_forwarded_origins() {
    let storage = Storage::connect_memory().await.unwrap();
    let mut admin_user = User::new("admin@example.org".into(), bcrypt::hash("password", 4).unwrap());
    admin_user.roles.insert(UserRole::Admin);
    storage.create_user(&admin_user).await.unwrap();
    let mut kobo_user = User::new("kobo-settings@example.org".into(), "unused".into());
    kobo_user.roles.insert(UserRole::KoboSync);
    storage.create_user(&kobo_user).await.unwrap();
    let key = "settings-kobo-key";
    storage
        .create_api_key(
            "settings-kobo-key-id",
            &kobo_user.id,
            &hex::encode(Sha512::digest(key.as_bytes())),
            "Kobo",
        )
        .await
        .unwrap();
    let state = AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
        .await
        .unwrap()
        .with_server_bind_settings(ServerBindSettings {
            configuration_port: 25_600,
            configuration_context_path: None,
            effective_port: Some(25_601),
            effective_context_path: "/komga".into(),
        })
        .with_trusted_proxies(vec!["127.0.0.0/8".parse().unwrap()]);
    let app = build_app(state);
    assert_eq!(
        app.clone()
            .oneshot(Request::get("/actuator/health").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.clone()
            .oneshot(Request::get("/komga/actuator/health").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let admin = basic("admin@example.org", "password");
    assert_eq!(
        app.clone()
            .oneshot(
                Request::patch("/komga/api/v1/settings")
                    .header(header::AUTHORIZATION, admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"koboPort":34567}"#))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );

    let direct = app
        .clone()
        .oneshot(
            Request::get(format!("/komga/kobo/{key}/v1/initialization"))
                .header(header::HOST, "reader.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(direct.status(), StatusCode::OK);
    assert_eq!(
        body_json(direct).await["Resources"]["image_host"],
        "http://reader.example:34567/komga"
    );

    let forwarded = app
        .clone()
        .oneshot(through_trusted_proxy(
            Request::get(format!("/komga/kobo/{key}/v1/initialization"))
                .header(header::HOST, "internal.example:25601")
                .header("forwarded", "for=192.0.2.1;proto=https;host=public.example")
                .header("x-forwarded-prefix", "/reader")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(forwarded.status(), StatusCode::OK);
    assert_eq!(
        body_json(forwarded).await["Resources"]["image_host"],
        "https://public.example/reader"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn kepubify_database_path_reconfigures_immediately_and_falls_back_to_configuration() {
    let temporary = tempfile::tempdir().unwrap();
    let configuration_path = temporary.path().join("configured-kepubify");
    let database_path = temporary.path().join("database-kepubify");
    for path in [&configuration_path, &database_path] {
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let storage = Storage::connect_memory().await.unwrap();
    storage
        .update_server_settings(&ServerSettingsPatch {
            kepubify_path: SettingUpdate::Set("/missing/kepubify".into()),
            ..ServerSettingsPatch::default()
        })
        .await
        .unwrap();
    let state = AppState::new(storage, TaskStorage::connect_memory().await.unwrap())
        .await
        .unwrap()
        .with_kobo_kepubify_path(Some(configuration_path.clone()))
        .await;
    let app = build_app(state);
    claim(&app).await;
    let admin = basic("admin@example.org", "password");

    let get = |app: &axum::Router, admin: &str| {
        let app = app.clone();
        let admin = admin.to_owned();
        async move {
            body_json(
                app.oneshot(
                    Request::get("/api/v1/settings")
                        .header(header::AUTHORIZATION, admin)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
            )
            .await
        }
    };
    let initial = get(&app, &admin).await;
    assert_eq!(
        initial["kepubifyPath"]["configurationSource"],
        configuration_path.to_string_lossy().as_ref()
    );
    assert_eq!(
        initial["kepubifyPath"]["effectiveValue"],
        configuration_path.to_string_lossy().as_ref()
    );

    for (value, effective) in [
        (
            serde_json::Value::String(database_path.to_string_lossy().into_owned()),
            database_path.to_string_lossy().into_owned(),
        ),
        (
            serde_json::Value::String("/still/missing".into()),
            configuration_path.to_string_lossy().into_owned(),
        ),
        (
            serde_json::Value::Null,
            configuration_path.to_string_lossy().into_owned(),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::patch("/api/v1/settings")
                    .header(header::AUTHORIZATION, &admin)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({"kepubifyPath": value}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(get(&app, &admin).await["kepubifyPath"]["effectiveValue"], effective);
    }
}
