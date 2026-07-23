use axum::{
    Router,
    body::Body,
    http::{Request, Response, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use komga_server::{AppState, REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER, build_app};
use komga_storage::Storage;
use serde_json::Value;
use tower::ServiceExt;

async fn test_app() -> Router {
    build_app(AppState::new(Storage::connect_memory().await.unwrap()))
}

fn basic(email: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{email}:{password}")))
}

async fn body_json(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
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
    let app = build_app(AppState::new(storage));

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
