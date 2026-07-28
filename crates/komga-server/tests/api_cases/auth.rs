use super::*;

#[tokio::test]
async fn cors_is_disabled_by_default_and_uses_an_explicit_origin_allowlist() {
    let default_app = test_app().await;
    let default_preflight = default_app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/claim")
                .header(header::ORIGIN, "https://reader.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "x-komga-email,x-komga-password")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !default_preflight
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );

    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let app = build_app(
        AppState::new(storage, tasks)
            .await
            .unwrap()
            .with_cors_allowed_origins(vec!["https://reader.example".parse().unwrap()]),
    );
    let allowed = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/claim")
                .header(header::ORIGIN, "https://reader.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "x-komga-email,x-komga-password")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert_eq!(
        allowed.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://reader.example"
    );
    assert_eq!(allowed.headers()[header::ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");

    let denied = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v1/claim")
                .header(header::ORIGIN, "https://attacker.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(!denied.headers().contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
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

    let webui_unauthorized = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header("X-Requested-With", "XMLHttpRequest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(webui_unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert!(!webui_unauthorized.headers().contains_key(header::WWW_AUTHENTICATE));
    assert_eq!(body_json(webui_unauthorized).await["status"], 401);

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
async fn authentication_cookies_follow_forwarded_https_and_prefix() {
    let app = app_with_storage_trusting_loopback(Storage::connect_memory().await.unwrap()).await;
    assert_eq!(claim(&app).await.status(), StatusCode::OK);

    let login = app
        .clone()
        .oneshot(through_trusted_proxy(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header("forwarded", "for=192.0.2.10;proto=https;host=reader.example")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let set_cookie = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .find_map(|value| {
            let value = value.to_str().unwrap();
            value
                .starts_with(komga_server::SESSION_COOKIE)
                .then_some(value.to_owned())
        })
        .unwrap();
    assert!(set_cookie.contains("Path=/komga"));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
    assert!(set_cookie.contains("Secure"));

    let cookie = set_cookie.split(';').next().unwrap();
    let logout = app
        .oneshot(through_trusted_proxy(
            Request::post("/api/logout")
                .header(header::COOKIE, cookie)
                .header("x-forwarded-proto", "https")
                .header("x-forwarded-prefix", "/komga")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert!(
        logout.headers().get_all(header::SET_COOKIE).iter().all(|value| value
            .to_str()
            .unwrap()
            .contains("Path=/komga")
            && value.to_str().unwrap().contains("Secure"))
    );
}

#[tokio::test]
async fn untrusted_peers_cannot_spoof_forwarded_cookie_or_audit_metadata() {
    let storage = Storage::connect_memory().await.unwrap();
    let app = app_with_storage(storage.clone()).await;
    claim(&app).await;

    let login = app
        .oneshot(through_trusted_proxy(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .header("forwarded", "for=198.51.100.9;proto=https;host=attacker.example")
                .header("x-forwarded-prefix", "/spoofed")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();

    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(komga_server::SESSION_COOKIE))
        .unwrap();
    assert!(cookie.contains("Path=/;"));
    assert!(!cookie.contains("Secure"));
    let activity = storage
        .list_authentication_activity(None, &komga_core::PageRequest::default())
        .await
        .unwrap();
    assert_eq!(activity.content[0].ip.as_deref(), Some("127.0.0.1"));
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
async fn remember_me_cookie_survives_application_state_recreation() {
    let storage = Storage::connect_memory().await.unwrap();
    let first_app = app_with_storage(storage.clone()).await;
    assert_eq!(claim(&first_app).await.status(), StatusCode::OK);
    let login = first_app
        .oneshot(
            Request::get("/api/v2/users/me?remember-me=true")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let remember_cookie = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(REMEMBER_ME_COOKIE))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let restarted_app = app_with_storage(storage).await;
    let remembered = restarted_app
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, remember_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(remembered.status(), StatusCode::OK);
    assert!(
        remembered
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.starts_with(komga_server::SESSION_COOKIE))
    );
    assert_eq!(body_json(remembered).await["email"], "admin@example.org");
}

#[tokio::test]
async fn repeated_password_failures_are_rate_limited_without_blocking_other_accounts() {
    let app = test_app().await;
    claim(&app).await;
    for _ in 0..5 {
        let failed = app
            .clone()
            .oneshot(
                Request::get("/api/v2/users/me")
                    .header(header::AUTHORIZATION, basic("missing@example.org", "wrong"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
    }

    let limited = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("missing@example.org", "wrong"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers()[header::RETRY_AFTER], "300");

    let valid = app
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(valid.status(), StatusCode::OK);
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
async fn authentication_activity_records_attempts_without_recounting_ordinary_sessions() {
    let storage = Storage::connect_memory().await.unwrap();
    let app = app_with_storage_trusting_loopback(storage.clone()).await;
    let admin_id = body_json(claim(&app).await).await["id"].as_str().unwrap().to_owned();

    let missing = app
        .clone()
        .oneshot(Request::get("/api/v2/users/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        storage
            .list_authentication_activity(None, &komga_core::PageRequest::default())
            .await
            .unwrap()
            .total_elements,
        0
    );

    let failed = app
        .clone()
        .oneshot(through_trusted_proxy(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("ADMIN@example.org", "wrong-secret"))
                .header("forwarded", "for=198.51.100.7;proto=https")
                .header(header::USER_AGENT, "activity-test")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::UNAUTHORIZED);
    let failed_activity = storage
        .list_authentication_activity(None, &komga_core::PageRequest::default())
        .await
        .unwrap();
    assert_eq!(failed_activity.total_elements, 1);
    assert_eq!(failed_activity.content[0].user_id.as_deref(), Some(admin_id.as_str()));
    assert_eq!(failed_activity.content[0].ip.as_deref(), Some("198.51.100.7"));
    assert_eq!(failed_activity.content[0].source.as_deref(), Some("Password"));
    assert!(!failed_activity.content[0].success);
    assert!(!format!("{:?}", failed_activity.content[0]).contains("wrong-secret"));

    let login = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me?remember-me=true")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let ordinary_cookie = session_cookie(&login);
    let remember_cookie = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(REMEMBER_ME_COOKIE))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let ordinary = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, &ordinary_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ordinary.status(), StatusCode::OK);
    assert_eq!(
        storage
            .list_authentication_activity(None, &komga_core::PageRequest::default())
            .await
            .unwrap()
            .total_elements,
        2
    );

    let remembered = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::COOKIE, remember_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(remembered.status(), StatusCode::OK);
    assert_eq!(
        storage
            .list_authentication_activity(None, &komga_core::PageRequest::default())
            .await
            .unwrap()
            .content[0]
            .source
            .as_deref(),
        Some("RememberMe")
    );

    let plain_key = "activity-api-key";
    let key_id = komga_core::new_id();
    storage
        .create_api_key(
            &key_id,
            &admin_id,
            &komga_server_test_sha512(plain_key),
            "activity client",
        )
        .await
        .unwrap();
    let api_key_response = app
        .clone()
        .oneshot(
            Request::get("/api/v2/users/me")
                .header("X-API-Key", plain_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(api_key_response.status(), StatusCode::OK);
    let latest_key = storage
        .latest_authentication_activity(
            &storage.find_user_by_id(&admin_id).await.unwrap().unwrap(),
            Some(&key_id),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest_key.api_key_comment.as_deref(), Some("activity client"));
    assert!(!format!("{latest_key:?}").contains(plain_key));

    let own_page = app
        .clone()
        .oneshot(
            Request::get("/api/v1/users/me/authentication-activity?size=2&sort=dateTime,desc")
                .header(header::COOKIE, &ordinary_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(own_page.status(), StatusCode::OK);
    let own_page = body_json(own_page).await;
    assert_eq!(own_page["size"], 2);
    assert_eq!(own_page["totalElements"], 4);
    assert!(own_page["content"][0]["dateTime"].as_str().unwrap().ends_with('Z'));

    let latest_endpoint = app
        .oneshot(
            Request::get(format!(
                "/api/v1/users/{admin_id}/authentication-activity/latest?apikey_id={key_id}"
            ))
            .header(header::COOKIE, ordinary_cookie)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(latest_endpoint.status(), StatusCode::OK);
    assert_eq!(body_json(latest_endpoint).await["apiKeyId"], key_id);
}

#[tokio::test]
async fn authentication_audit_failure_does_not_block_a_valid_login() {
    let temporary = tempfile::tempdir().unwrap();
    let database = temporary.path().join("database.sqlite");
    let storage = Storage::connect_path(&database).await.unwrap();
    let app = app_with_storage(storage).await;
    claim(&app).await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    sqlx::query("DROP TABLE AUTHENTICATION_ACTIVITY")
        .execute(&pool)
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::get("/api/v2/users/me")
                .header(header::AUTHORIZATION, basic("admin@example.org", "password"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(header::SET_COOKIE));
}

fn komga_server_test_sha512(value: &str) -> String {
    hex::encode(Sha512::digest(value.as_bytes()))
}
