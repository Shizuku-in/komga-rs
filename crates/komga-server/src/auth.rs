//! Compatibility authentication and process-local session management.
//!
//! Authentication is attempted in this order: `X-API-Key`, an existing header
//! or cookie session, then HTTP Basic credentials. Supplying an API key makes it
//! authoritative; an invalid key does not fall back to another credential.
//! Successful Basic authentication creates a session and returns it through
//! `X-Auth-Token` when that header was present on the request, otherwise through
//! `KOMGA-SESSION`.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, HeaderValue, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use komga_core::{ApiKey, AuthenticationActivity, User, UserRole};
use moka::{Expiry, policy::EvictionPolicy, sync::Cache};
use rand::RngCore;
use sha2::{Digest, Sha512};
use url::Url;

use crate::{AppState, error::ApiError};

/// Cookie name used for ordinary Komga sessions.
pub const SESSION_COOKIE: &str = "KOMGA-SESSION";
/// Header used to transport a Komga session without a cookie.
pub const SESSION_HEADER: &str = "X-Auth-Token";
/// Cookie name used for extended login sessions.
pub const REMEMBER_ME_COOKIE: &str = "komga-remember-me";

const MAX_SESSION_COUNT: u64 = 16_384;
const ORDINARY_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CookieSettings {
    path: String,
    secure: bool,
}

impl CookieSettings {
    pub(crate) fn from_request(context_path: &str, headers: &HeaderMap, uri: &Uri) -> Self {
        let path = first_header_value(headers, "x-forwarded-prefix")
            .and_then(|value| normalized_cookie_path(&value))
            .or_else(|| normalized_cookie_path(context_path))
            .unwrap_or_else(|| "/".to_owned());
        let forwarded_scheme = forwarded_parameter_value(headers, "proto")
            .or_else(|| first_header_value(headers, "x-forwarded-proto"))
            .or_else(|| uri.scheme_str().map(str::to_owned));
        let secure = forwarded_scheme.map_or_else(
            || first_header_value(headers, "x-forwarded-ssl").is_some_and(|value| value.eq_ignore_ascii_case("on")),
            |scheme| scheme.eq_ignore_ascii_case("https"),
        );
        Self { path, secure }
    }

    pub(crate) fn from_public_url(url: &Url) -> Self {
        Self {
            path: normalized_cookie_path(url.path()).unwrap_or_else(|| "/".to_owned()),
            secure: url.scheme() == "https",
        }
    }

    pub(crate) fn attributes(&self) -> String {
        format!(
            "Path={}; HttpOnly; SameSite=Lax{}",
            self.path,
            if self.secure { "; Secure" } else { "" }
        )
    }
}

fn normalized_cookie_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "/" {
        return Some("/".to_owned());
    }
    let path = format!("/{}", value.trim_matches('/'));
    komga_core::valid_server_context_path(&path).then_some(path)
}

fn first_header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn forwarded_parameter_value(headers: &HeaderMap, name: &str) -> Option<String> {
    first_header_value(headers, "forwarded")?
        .split(';')
        .find_map(|parameter| {
            let (key, value) = parameter.trim().split_once('=')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().trim_matches('"').to_owned())
        })
}

#[derive(Debug, Clone)]
pub(crate) struct AuthContext {
    /// Fresh user aggregate loaded for this request.
    pub(crate) user: User,
    /// Session used or issued for this request; absent for API key requests.
    pub(crate) session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct Session {
    user_id: String,
    remember_me: bool,
    lifetime: Duration,
}

#[derive(Debug, Clone, Copy)]
struct SessionExpiry;

impl Expiry<String, Session> for SessionExpiry {
    fn expire_after_create(&self, _key: &String, value: &Session, _created_at: Instant) -> Option<Duration> {
        Some(value.lifetime)
    }

    fn expire_after_read(
        &self,
        _key: &String,
        value: &Session,
        _read_at: Instant,
        duration_until_expiry: Option<Duration>,
        _last_modified_at: Instant,
    ) -> Option<Duration> {
        if value.remember_me {
            duration_until_expiry
        } else {
            Some(value.lifetime)
        }
    }
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    sessions: Cache<String, Session>,
    remember_me_duration_seconds: Arc<AtomicU64>,
    ordinary_session_idle_timeout: Duration,
}

impl SessionStore {
    pub(crate) fn new(remember_me_duration_days: u64) -> Self {
        Self::with_policy(
            remember_me_duration_days,
            MAX_SESSION_COUNT,
            ORDINARY_SESSION_IDLE_TIMEOUT,
        )
    }

    fn with_policy(remember_me_duration_days: u64, max_sessions: u64, ordinary_idle_timeout: Duration) -> Self {
        Self {
            sessions: Cache::builder()
                .max_capacity(max_sessions)
                .eviction_policy(EvictionPolicy::lru())
                .expire_after(SessionExpiry)
                .build(),
            remember_me_duration_seconds: Arc::new(AtomicU64::new(duration_seconds(remember_me_duration_days))),
            ordinary_session_idle_timeout: ordinary_idle_timeout,
        }
    }

    /// Issues a cryptographically random session with the selected lifetime.
    pub(crate) fn create(&self, user_id: &str, remember_me: bool) -> String {
        let mut bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let duration = if remember_me {
            Duration::from_secs(self.remember_me_duration_seconds.load(Ordering::Relaxed))
        } else {
            self.ordinary_session_idle_timeout
        };
        self.sessions.insert(
            id.clone(),
            Session {
                user_id: user_id.to_owned(),
                remember_me,
                lifetime: duration,
            },
        );
        id
    }

    /// Resolves a live session and refreshes the idle timeout for ordinary sessions.
    pub(crate) fn user_id(&self, session_id: &str) -> Option<String> {
        self.sessions.get(session_id).map(|session| session.user_id)
    }

    /// Invalidates one session.
    pub(crate) fn remove(&self, session_id: &str) {
        self.sessions.invalidate(session_id);
    }

    /// Invalidates every session owned by a user after security policy changes.
    pub(crate) fn remove_user(&self, user_id: &str) {
        let session_ids = self
            .sessions
            .iter()
            .filter_map(|(session_id, session)| (session.user_id == user_id).then_some(session_id))
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.sessions.invalidate(session_id.as_ref());
        }
    }

    /// Updates the lifetime used by subsequently issued remember-me sessions.
    pub(crate) fn set_remember_me_duration_days(&self, days: u64) {
        self.remember_me_duration_seconds
            .store(duration_seconds(days), Ordering::Relaxed);
    }

    /// Invalidates long-lived sessions while preserving ordinary sessions.
    pub(crate) fn remove_remembered(&self) {
        let session_ids = self
            .sessions
            .iter()
            .filter_map(|(session_id, session)| session.remember_me.then_some(session_id))
            .collect::<Vec<_>>();
        for session_id in session_ids {
            self.sessions.invalidate(session_id.as_ref());
        }
    }

    pub(crate) fn remember_me_max_age(&self) -> u64 {
        self.remember_me_duration_seconds.load(Ordering::Relaxed)
    }
}

fn duration_seconds(days: u64) -> u64 {
    days.saturating_mul(24 * 60 * 60)
}

enum AuthSource {
    ApiKey(ApiKey),
    Session(String),
    RememberMe(String),
    Basic { remember_me: bool },
}

#[derive(Debug, Clone)]
pub(crate) struct AuthenticationRequestMetadata {
    ip: Option<String>,
    user_agent: Option<String>,
}

/// Authenticates protected requests and attaches [`AuthContext`] for handlers.
pub(crate) async fn require_auth(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let opds_v2 = request.uri().path().starts_with("/opds/v2/");
    let request_headers = request.headers().clone();
    let request_uri = request.uri().clone();
    let cookie_settings =
        CookieSettings::from_request(&state.effective_server_context_path, request.headers(), request.uri());
    let request_metadata = authentication_request_metadata(&request);
    match authenticate(&state, request.headers(), request.uri().query()).await {
        Ok((user, source)) => {
            record_authentication_success(&state, &request_metadata, &user, &source).await;
            let (session_id, issue_session, remember_me) = match source {
                AuthSource::ApiKey(_) => (None, false, false),
                AuthSource::Session(id) => (Some(id), false, false),
                AuthSource::RememberMe(id) => (Some(id), false, true),
                AuthSource::Basic { remember_me } => {
                    (Some(state.sessions.create(&user.id, remember_me)), true, remember_me)
                }
            };
            request.extensions_mut().insert(AuthContext {
                user,
                session_id: session_id.clone(),
            });
            let return_session_in_header = request.headers().contains_key(SESSION_HEADER);
            let mut response = next.run(request).await;
            if issue_session && let Some(session_id) = session_id {
                if return_session_in_header {
                    if let Ok(value) = HeaderValue::from_str(&session_id) {
                        response.headers_mut().insert(SESSION_HEADER, value);
                    }
                } else {
                    append_cookie(response.headers_mut(), &session_cookie(&session_id, &cookie_settings));
                }
                if remember_me {
                    append_cookie(
                        response.headers_mut(),
                        &remember_me_cookie(&session_id, state.sessions.remember_me_max_age(), &cookie_settings),
                    );
                }
            }
            response
        }
        Err(error) => {
            record_authentication_failure(&state, &request_headers, &request_metadata).await;
            if opds_v2 {
                crate::opds_v2::unauthorized(&request_headers, &request_uri)
            } else {
                error.into_response()
            }
        }
    }
}

/// Authenticates KOReader protocol requests exclusively through `X-Auth-User`.
pub(crate) async fn require_koreader_auth(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let Some(key) = request
        .headers()
        .get("X-Auth-User")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
    else {
        return ApiError::forbidden().into_response();
    };
    let digest = hex::encode(Sha512::digest(key.as_bytes()));
    let user = match state.storage.find_user_by_api_key_hash(&digest).await {
        Ok(Some(user)) if user.roles.contains(&UserRole::KoreaderSync) => user,
        Ok(_) => return ApiError::forbidden().into_response(),
        Err(error) => return ApiError::from(error).into_response(),
    };
    request.extensions_mut().insert(AuthContext { user, session_id: None });
    next.run(request).await
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<(User, AuthSource), ApiError> {
    if let Some(key) = headers.get("X-API-Key") {
        let key = key.to_str().map_err(|_| ApiError::unauthorized())?;
        let digest = hex::encode(Sha512::digest(key.as_bytes()));
        let api_key = state
            .storage
            .find_api_key_by_hash(&digest)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        let user = state
            .storage
            .find_user_by_id(&api_key.user_id)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        return Ok((user, AuthSource::ApiKey(api_key)));
    }

    let header_session = headers.get(SESSION_HEADER).and_then(|value| value.to_str().ok());
    let cookie_session = cookie_value(headers, SESSION_COOKIE);
    if let Some(session_id) = header_session
        .filter(|value| !value.is_empty())
        .or(cookie_session.as_deref())
        && let Some(user_id) = state.sessions.user_id(session_id)
        && let Some(user) = state.storage.find_user_by_id(&user_id).await?
    {
        return Ok((user, AuthSource::Session(session_id.to_owned())));
    }
    if let Some(session_id) = cookie_value(headers, REMEMBER_ME_COOKIE)
        && let Some(user_id) = state.sessions.user_id(&session_id)
        && let Some(user) = state.storage.find_user_by_id(&user_id).await?
    {
        return Ok((user, AuthSource::RememberMe(session_id)));
    }

    let Some((email, password)) = basic_credentials(headers) else {
        return Err(ApiError::unauthorized());
    };
    let Some(user) = state.storage.find_user_by_email(&email).await? else {
        return Err(ApiError::unauthorized());
    };
    let password_hash = user.password_hash.clone();
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(password, &password_hash))
        .await
        .map_err(|error| {
            tracing::error!(%error, "password verification task failed");
            ApiError::internal("Authentication failed")
        })?
        .unwrap_or(false);
    if !valid {
        return Err(ApiError::unauthorized());
    }

    Ok((
        user,
        AuthSource::Basic {
            remember_me: query_has_true(query, "remember-me"),
        },
    ))
}

async fn record_authentication_success(
    state: &AppState,
    metadata: &AuthenticationRequestMetadata,
    user: &User,
    source: &AuthSource,
) {
    let (source, api_key_id, api_key_comment) = match source {
        AuthSource::ApiKey(key) => (
            Some("ApiKey".to_owned()),
            Some(key.id.clone()),
            Some(key.comment.clone()),
        ),
        AuthSource::RememberMe(_) => (Some("RememberMe".to_owned()), None, None),
        AuthSource::Basic { .. } => (Some("Password".to_owned()), None, None),
        AuthSource::Session(_) => return,
    };
    record_authentication_activity(
        state,
        AuthenticationActivity {
            user_id: Some(user.id.clone()),
            email: Some(user.email.clone()),
            api_key_id,
            api_key_comment,
            ip: metadata.ip.clone(),
            user_agent: metadata.user_agent.clone(),
            success: true,
            error: None,
            date_time: chrono::Utc::now().naive_utc(),
            source,
        },
    )
    .await;
}

async fn record_authentication_failure(
    state: &AppState,
    headers: &HeaderMap,
    metadata: &AuthenticationRequestMetadata,
) {
    let (source, email) = if headers.contains_key("X-API-Key") {
        ("ApiKey", None)
    } else if has_basic_authorization(headers) {
        ("Password", basic_credentials(headers).map(|(email, _)| email))
    } else if cookie_value(headers, REMEMBER_ME_COOKIE).is_some() {
        ("RememberMe", None)
    } else {
        return;
    };
    let user_id = if let Some(email) = email.as_deref() {
        match state.storage.find_user_by_email(email).await {
            Ok(user) => user.map(|user| user.id),
            Err(error) => {
                tracing::error!(%error, "failed to resolve authentication activity principal");
                None
            }
        }
    } else {
        None
    };
    record_authentication_activity(
        state,
        AuthenticationActivity {
            user_id,
            email,
            api_key_id: None,
            api_key_comment: None,
            ip: metadata.ip.clone(),
            user_agent: metadata.user_agent.clone(),
            success: false,
            error: Some(truncate("Authentication failed", 512)),
            date_time: chrono::Utc::now().naive_utc(),
            source: Some(source.to_owned()),
        },
    )
    .await;
}

async fn record_authentication_activity(state: &AppState, activity: AuthenticationActivity) {
    if let Err(error) = state.storage.insert_authentication_activity(&activity).await {
        tracing::error!(%error, source = activity.source.as_deref(), success = activity.success, "failed to persist authentication activity");
    }
}

pub(crate) async fn record_oauth_activity(
    state: &AppState,
    metadata: &AuthenticationRequestMetadata,
    user: Option<&User>,
    email: Option<String>,
    provider_name: &str,
    success: bool,
    error: Option<&str>,
) {
    record_authentication_activity(
        state,
        AuthenticationActivity {
            user_id: user.map(|user| user.id.clone()),
            email: user.map(|user| user.email.clone()).or(email),
            api_key_id: None,
            api_key_comment: None,
            ip: metadata.ip.clone(),
            user_agent: metadata.user_agent.clone(),
            success,
            error: error.map(|error| truncate(error, 512)),
            date_time: chrono::Utc::now().naive_utc(),
            source: Some(format!("OAuth2:{}", truncate(provider_name, 128))),
        },
    )
    .await;
}

fn has_basic_authorization(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .get(..6)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Basic "))
        })
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| truncate(value, 512))
}

pub(crate) fn authentication_request_metadata(request: &Request) -> AuthenticationRequestMetadata {
    AuthenticationRequestMetadata {
        ip: forwarded_ip(request.headers())
            .or_else(|| x_forwarded_for(request.headers()))
            .or_else(|| {
                request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .map(|connect| connect.0.ip().to_string())
            }),
        user_agent: user_agent(request.headers()),
    }
}

fn forwarded_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("forwarded")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .flat_map(|element| element.split(';'))
        .find_map(|parameter| {
            let (name, value) = parameter.trim().split_once('=')?;
            name.eq_ignore_ascii_case("for")
                .then(|| normalize_forwarded_address(value.trim().trim_matches('"')))
                .flatten()
        })
}

fn x_forwarded_for(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .find_map(|value| normalize_forwarded_address(value.trim()))
}

fn normalize_forwarded_address(value: &str) -> Option<String> {
    if value.is_empty() || value.eq_ignore_ascii_case("unknown") || value.starts_with('_') {
        return None;
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(ip.to_string());
    }
    if let Ok(address) = value.parse::<SocketAddr>() {
        return Some(address.ip().to_string());
    }
    value
        .strip_prefix('[')
        .and_then(|value| value.split_once(']'))
        .and_then(|(ip, _)| ip.parse::<IpAddr>().ok())
        .map(|ip| ip.to_string())
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(crate) async fn optional_user(state: &AppState, headers: &HeaderMap) -> Result<Option<User>, ApiError> {
    let has_credentials = headers.contains_key("X-API-Key")
        || headers.contains_key(SESSION_HEADER)
        || headers.contains_key(header::AUTHORIZATION)
        || cookie_value(headers, SESSION_COOKIE).is_some()
        || cookie_value(headers, REMEMBER_ME_COOKIE).is_some();
    if !has_credentials {
        return Ok(None);
    }
    authenticate(state, headers, None).await.map(|(user, _)| Some(user))
}

fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value.strip_prefix("Basic ").or_else(|| value.strip_prefix("basic "))?;
    let decoded = STANDARD.decode(encoded).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (email, password) = decoded.split_once(':')?;
    Some((email.to_owned(), password.to_owned()))
}

fn query_has_true(query: Option<&str>, key: &str) -> bool {
    query.is_some_and(|query| {
        query.split('&').any(|part| {
            part.split_once('=')
                .is_some_and(|(name, value)| name == key && value.eq_ignore_ascii_case("true"))
        })
    })
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
}

/// Appends a validated `Set-Cookie` header without replacing existing cookies.
pub(crate) fn append_cookie(headers: &mut HeaderMap, cookie: &str) {
    if let Ok(value) = HeaderValue::from_str(cookie) {
        headers.append(header::SET_COOKIE, value);
    }
}

/// Builds the ordinary HTTP-only session cookie.
pub(crate) fn session_cookie(session_id: &str, settings: &CookieSettings) -> String {
    format!("{SESSION_COOKIE}={session_id}; {}", settings.attributes())
}

fn remember_me_cookie(session_id: &str, max_age: u64, settings: &CookieSettings) -> String {
    format!(
        "{REMEMBER_ME_COOKIE}={session_id}; Max-Age={max_age}; {}",
        settings.attributes()
    )
}

/// Returns deletion cookies for both supported browser-session mechanisms.
pub(crate) fn expired_session_cookies(settings: &CookieSettings) -> [String; 2] {
    [
        format!("{SESSION_COOKIE}=; Max-Age=0; {}", settings.attributes()),
        format!("{REMEMBER_ME_COOKIE}=; Max-Age=0; {}", settings.attributes()),
    ]
}

/// Produces the lowercase SHA-512 representation used by Kotlin API keys.
pub(crate) fn sha512_hex(value: &str) -> String {
    hex::encode(Sha512::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_expiry_refreshes_only_ordinary_sessions() {
        let expiry = SessionExpiry;
        let ordinary = Session {
            user_id: "ordinary".into(),
            remember_me: false,
            lifetime: Duration::from_secs(60),
        };
        let remembered = Session {
            user_id: "remembered".into(),
            remember_me: true,
            lifetime: Duration::from_secs(600),
        };
        let now = Instant::now();

        assert_eq!(
            expiry.expire_after_create(&"ordinary".into(), &ordinary, now),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            expiry.expire_after_read(&"ordinary".into(), &ordinary, now, Some(Duration::from_secs(10)), now,),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            expiry.expire_after_read(
                &"remembered".into(),
                &remembered,
                now,
                Some(Duration::from_secs(10)),
                now,
            ),
            Some(Duration::from_secs(10))
        );
    }

    #[test]
    fn session_store_enforces_its_capacity() {
        let store = SessionStore::with_policy(30, 2, Duration::from_secs(60));
        store.create("one", false);
        store.create("two", false);
        let newest = store.create("three", false);
        store.sessions.run_pending_tasks();

        assert!(store.sessions.entry_count() <= 2);
        assert_eq!(store.user_id(&newest).as_deref(), Some("three"));
    }

    #[test]
    fn cookie_settings_follow_forwarded_https_and_context_path() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "forwarded",
            "for=192.0.2.10;proto=https;host=reader.example".parse().unwrap(),
        );
        headers.insert("x-forwarded-prefix", "/komga/".parse().unwrap());
        let settings = CookieSettings::from_request("", &headers, &"/api/v2/users/me".parse().unwrap());

        assert_eq!(settings.path, "/komga");
        assert!(settings.secure);
        assert_eq!(
            session_cookie("session", &settings),
            "KOMGA-SESSION=session; Path=/komga; HttpOnly; SameSite=Lax; Secure"
        );
        assert!(
            expired_session_cookies(&settings)
                .iter()
                .all(|cookie| cookie.contains("Path=/komga") && cookie.ends_with("; Secure"))
        );
    }

    #[test]
    fn cookie_settings_reject_unsafe_forwarded_paths_and_support_public_urls() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-prefix", "/bad; Secure".parse().unwrap());
        headers.insert("x-forwarded-ssl", "on".parse().unwrap());
        let settings = CookieSettings::from_request("/configured", &headers, &"/api/v2/users/me".parse().unwrap());
        assert_eq!(settings.path, "/configured");
        assert!(settings.secure);

        let public = CookieSettings::from_public_url(&Url::parse("https://reader.example/komga/").unwrap());
        assert_eq!(public.path, "/komga");
        assert!(public.secure);

        let local = CookieSettings::from_request("", &HeaderMap::new(), &"/api/v2/users/me".parse().unwrap());
        assert_eq!(local.path, "/");
        assert!(!local.secure);
        assert!(!session_cookie("session", &local).contains("Secure"));
        assert!(!CookieSettings::from_public_url(&Url::parse("http://localhost:25600/").unwrap()).secure);
    }
}
