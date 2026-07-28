//! Compatibility authentication, signed remember-me tokens, and sessions.
//!
//! Authentication is attempted in this order: `X-API-Key`, an existing header
//! or cookie session, then HTTP Basic credentials. Supplying an API key makes it
//! authoritative; an invalid key does not fall back to another credential.
//! Successful Basic authentication creates a session and returns it through
//! `X-Auth-Token` when that header was present on the request, otherwise through
//! `KOMGA-SESSION`.

use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Uri, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use komga_core::{ApiKey, AuthenticationActivity, User, UserRole};
use moka::{Expiry, policy::EvictionPolicy, sync::Cache};
use rand::Rng;
use sha2::{Digest, Sha256, Sha512};
use tokio::sync::Semaphore;
use url::Url;

use crate::{AppState, error::ApiError, proxy::ClientAddress};

/// Cookie name used for ordinary Komga sessions.
pub const SESSION_COOKIE: &str = "KOMGA-SESSION";
/// Header used to transport a Komga session without a cookie.
pub const SESSION_HEADER: &str = "X-Auth-Token";
/// Cookie name used for extended login sessions.
pub const REMEMBER_ME_COOKIE: &str = "komga-remember-me";

const MAX_SESSION_COUNT: u64 = 16_384;
const ORDINARY_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
const ACCOUNT_FAILURE_LIMIT: u32 = 5;
const IP_FAILURE_LIMIT: u32 = 25;
const BCRYPT_QUEUE_TIMEOUT: Duration = Duration::from_secs(2);
const BCRYPT_COST: u32 = 10;
static BCRYPT_SEMAPHORE: Semaphore = Semaphore::const_new(4);

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
        _duration_until_expiry: Option<Duration>,
        _last_modified_at: Instant,
    ) -> Option<Duration> {
        Some(value.lifetime)
    }
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    sessions: Cache<String, Session>,
    remember_me_duration_seconds: Arc<AtomicU64>,
    remember_me_key: Arc<RwLock<String>>,
    ordinary_session_idle_timeout: Duration,
}

impl SessionStore {
    pub(crate) fn new(remember_me_duration_days: u64, remember_me_key: String) -> Self {
        Self::with_policy(
            remember_me_duration_days,
            remember_me_key,
            MAX_SESSION_COUNT,
            ORDINARY_SESSION_IDLE_TIMEOUT,
        )
    }

    fn with_policy(
        remember_me_duration_days: u64,
        remember_me_key: String,
        max_sessions: u64,
        ordinary_idle_timeout: Duration,
    ) -> Self {
        Self {
            sessions: Cache::builder()
                .max_capacity(max_sessions)
                .eviction_policy(EvictionPolicy::lru())
                .expire_after(SessionExpiry)
                .build(),
            remember_me_duration_seconds: Arc::new(AtomicU64::new(duration_seconds(remember_me_duration_days))),
            remember_me_key: Arc::new(RwLock::new(remember_me_key)),
            ordinary_session_idle_timeout: ordinary_idle_timeout,
        }
    }

    /// Issues a cryptographically random ordinary session.
    pub(crate) fn create(&self, user_id: &str) -> String {
        let mut bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        self.sessions.insert(
            id.clone(),
            Session {
                user_id: user_id.to_owned(),
                lifetime: self.ordinary_session_idle_timeout,
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

    /// Updates the duration and signing key used for remember-me tokens.
    pub(crate) fn set_remember_me_policy(&self, days: u64, key: String) {
        self.remember_me_duration_seconds
            .store(duration_seconds(days), Ordering::Relaxed);
        *self
            .remember_me_key
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = key;
    }

    pub(crate) fn remember_me_max_age(&self) -> u64 {
        self.remember_me_duration_seconds.load(Ordering::Relaxed)
    }

    fn remember_me_token(&self, user: &User) -> String {
        let expires_at = unix_time_millis().saturating_add(self.remember_me_max_age().saturating_mul(1_000));
        let signature = self.remember_me_signature(&user.email, expires_at, &user.password_hash, "SHA256");
        encode_remember_me_tokens(&[&user.email, &expires_at.to_string(), "SHA256", &signature])
    }

    fn verify_remember_me_token(&self, token: &RememberMeToken, user: &User) -> bool {
        if token.expires_at < unix_time_millis() || !token.email.eq_ignore_ascii_case(&user.email) {
            return false;
        }
        let expected = self.remember_me_signature(&user.email, token.expires_at, &user.password_hash, token.algorithm);
        constant_time_equal(expected.as_bytes(), token.signature.as_bytes())
    }

    fn remember_me_signature(&self, email: &str, expires_at: u64, password_hash: &str, algorithm: &str) -> String {
        if algorithm != "SHA256" {
            return String::new();
        }
        let key = self
            .remember_me_key
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        hex::encode(Sha256::digest(
            format!("{email}:{expires_at}:{password_hash}:{key}").as_bytes(),
        ))
    }
}

fn duration_seconds(days: u64) -> u64 {
    days.saturating_mul(24 * 60 * 60)
}

fn unix_time_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

struct RememberMeToken {
    email: String,
    expires_at: u64,
    algorithm: &'static str,
    signature: String,
}

impl RememberMeToken {
    fn decode(value: &str) -> Option<Self> {
        if value.len() > 4_096 {
            return None;
        }
        let mut encoded = value.to_owned();
        while !encoded.len().is_multiple_of(4) {
            encoded.push('=');
        }
        let decoded = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
        let tokens = decoded
            .split(':')
            .map(decode_form_component)
            .collect::<Option<Vec<_>>>()?;
        let (email, expires_at, algorithm, signature) = match tokens.as_slice() {
            [email, expires_at, signature] => (email.clone(), expires_at, "SHA256", signature.clone()),
            [email, expires_at, algorithm, signature] if algorithm == "SHA256" => {
                (email.clone(), expires_at, "SHA256", signature.clone())
            }
            _ => return None,
        };
        if email.is_empty() || signature.len() != 64 || !signature.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self {
            email,
            expires_at: expires_at.parse().ok()?,
            algorithm,
            signature,
        })
    }
}

fn encode_remember_me_tokens(tokens: &[&str]) -> String {
    let encoded = tokens
        .iter()
        .map(|token| url::form_urlencoded::byte_serialize(token.as_bytes()).collect::<String>())
        .collect::<Vec<_>>()
        .join(":");
    STANDARD.encode(encoded).trim_end_matches('=').to_owned()
}

fn decode_form_component(value: &str) -> Option<String> {
    url::form_urlencoded::parse(format!("value={value}").as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[derive(Clone)]
pub(crate) struct AuthenticationLimiter {
    failures: Cache<String, Arc<AtomicU32>>,
    account_limit: u32,
    ip_limit: u32,
    retry_after_seconds: u64,
}

impl Default for AuthenticationLimiter {
    fn default() -> Self {
        Self::with_policy(ACCOUNT_FAILURE_LIMIT, IP_FAILURE_LIMIT, AUTH_FAILURE_WINDOW)
    }
}

impl AuthenticationLimiter {
    fn with_policy(account_limit: u32, ip_limit: u32, window: Duration) -> Self {
        Self {
            failures: Cache::builder().max_capacity(65_536).time_to_live(window).build(),
            account_limit,
            ip_limit,
            retry_after_seconds: window.as_secs(),
        }
    }

    fn check(&self, email: &str, ip: Option<ClientAddress>) -> Result<(), ApiError> {
        let account = account_failure_key(email);
        let account_limited = self.count(&account) >= self.account_limit;
        let ip_limited = ip.is_some_and(|ip| self.count(&ip_failure_key(ip)) >= self.ip_limit);
        if account_limited || ip_limited {
            Err(ApiError::too_many_requests(self.retry_after_seconds))
        } else {
            Ok(())
        }
    }

    fn record_failure(&self, email: &str, ip: Option<ClientAddress>) {
        self.increment(account_failure_key(email));
        if let Some(ip) = ip {
            self.increment(ip_failure_key(ip));
        }
    }

    fn record_success(&self, email: &str) {
        self.failures.invalidate(&account_failure_key(email));
    }

    fn count(&self, key: &str) -> u32 {
        self.failures.get(key).map_or(0, |count| count.load(Ordering::Relaxed))
    }

    fn increment(&self, key: String) {
        let count = self.failures.get_with(key, || Arc::new(AtomicU32::new(0)));
        let _ = count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            Some(value.saturating_add(1))
        });
    }
}

fn account_failure_key(email: &str) -> String {
    format!(
        "account:{}",
        hex::encode(Sha256::digest(email.trim().to_ascii_lowercase().as_bytes()))
    )
}

fn ip_failure_key(ip: ClientAddress) -> String {
    format!("ip:{}", ip.0)
}

enum AuthSource {
    ApiKey(ApiKey),
    Session(String),
    RememberMe,
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
    let client_address = request.extensions().get::<ClientAddress>().copied();
    match authenticate(&state, request.headers(), request.uri().query(), client_address).await {
        Ok((user, source)) => {
            record_authentication_success(&state, &request_metadata, &user, &source).await;
            let (session_id, issue_session, remember_me_token) = match source {
                AuthSource::ApiKey(_) => (None, false, None),
                AuthSource::Session(id) => (Some(id), false, None),
                AuthSource::RememberMe => (Some(state.sessions.create(&user.id)), true, None),
                AuthSource::Basic { remember_me } => {
                    let token = remember_me.then(|| state.sessions.remember_me_token(&user));
                    (Some(state.sessions.create(&user.id)), true, token)
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
                if let Some(token) = remember_me_token {
                    append_cookie(
                        response.headers_mut(),
                        &remember_me_cookie(&token, state.sessions.remember_me_max_age(), &cookie_settings),
                    );
                }
            }
            response
        }
        Err(error) => {
            record_authentication_failure(&state, &request_headers, &request_metadata).await;
            let mut response = if opds_v2 && error.is_unauthorized() {
                crate::opds_v2::unauthorized(&request_headers, &request_uri)
            } else {
                error.into_response()
            };
            if cookie_value(&request_headers, REMEMBER_ME_COOKIE).is_some() {
                append_cookie(
                    response.headers_mut(),
                    &format!("{REMEMBER_ME_COOKIE}=; Max-Age=0; {}", cookie_settings.attributes()),
                );
            }
            response
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
    client_address: Option<ClientAddress>,
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
    if let Some(value) = cookie_value(headers, REMEMBER_ME_COOKIE)
        && let Some(token) = RememberMeToken::decode(&value)
        && let Some(user) = state.storage.find_user_by_email(&token.email).await?
        && state.sessions.verify_remember_me_token(&token, &user)
    {
        return Ok((user, AuthSource::RememberMe));
    }

    let Some((email, password)) = basic_credentials(headers) else {
        return Err(ApiError::unauthorized());
    };
    state.authentication_limiter.check(&email, client_address)?;
    let Some(user) = state.storage.find_user_by_email(&email).await? else {
        state.authentication_limiter.record_failure(&email, client_address);
        return Err(ApiError::unauthorized());
    };
    let password_hash = user.password_hash.clone();
    let valid = verify_password(password, password_hash).await?;
    if !valid {
        state.authentication_limiter.record_failure(&email, client_address);
        return Err(ApiError::unauthorized());
    }
    state.authentication_limiter.record_success(&email);

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
        AuthSource::RememberMe => (Some("RememberMe".to_owned()), None, None),
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
        ip: request
            .extensions()
            .get::<ClientAddress>()
            .map(|address| address.0.to_string()),
        user_agent: user_agent(request.headers()),
    }
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
    authenticate(state, headers, None, None)
        .await
        .map(|(user, _)| Some(user))
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

fn remember_me_cookie(token: &str, max_age: u64, settings: &CookieSettings) -> String {
    format!(
        "{REMEMBER_ME_COOKIE}={token}; Max-Age={max_age}; {}",
        settings.attributes()
    )
}

pub(crate) async fn hash_password(password: String) -> Result<String, ApiError> {
    let _permit = bcrypt_permit().await?;
    tokio::task::spawn_blocking(move || bcrypt::hash(password, BCRYPT_COST))
        .await
        .map_err(|error| {
            tracing::error!(%error, "password hashing task failed");
            ApiError::internal("Password hashing failed")
        })?
        .map_err(|error| {
            tracing::error!(%error, "password hashing failed");
            ApiError::internal("Password hashing failed")
        })
}

async fn verify_password(password: String, password_hash: String) -> Result<bool, ApiError> {
    let _permit = bcrypt_permit().await?;
    tokio::task::spawn_blocking(move || bcrypt::verify(password, &password_hash))
        .await
        .map_err(|error| {
            tracing::error!(%error, "password verification task failed");
            ApiError::internal("Authentication failed")
        })
        .map(|result| result.unwrap_or(false))
}

async fn bcrypt_permit() -> Result<tokio::sync::SemaphorePermit<'static>, ApiError> {
    tokio::time::timeout(BCRYPT_QUEUE_TIMEOUT, BCRYPT_SEMAPHORE.acquire())
        .await
        .map_err(|_| ApiError::too_many_requests(1))?
        .map_err(|_| ApiError::service_unavailable("Authentication is unavailable"))
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
    fn ordinary_session_expiry_refreshes_after_reads() {
        let expiry = SessionExpiry;
        let ordinary = Session {
            user_id: "ordinary".into(),
            lifetime: Duration::from_secs(60),
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
    }

    #[test]
    fn session_store_enforces_its_capacity() {
        let store = SessionStore::with_policy(30, "key".into(), 2, Duration::from_secs(60));
        store.create("one");
        store.create("two");
        let newest = store.create("three");
        store.sessions.run_pending_tasks();

        assert!(store.sessions.entry_count() <= 2);
        assert_eq!(store.user_id(&newest).as_deref(), Some("three"));
    }

    #[test]
    fn remember_me_tokens_survive_store_recreation_and_follow_password_and_key_rotation() {
        let user = User::new("reader+test@example.org".into(), "$2a$10$hash".into());
        let first = SessionStore::with_policy(30, "persistent-key".into(), 2, Duration::from_secs(60));
        let token = first.remember_me_token(&user);
        let decoded = RememberMeToken::decode(&token).unwrap();
        let restarted = SessionStore::with_policy(30, "persistent-key".into(), 2, Duration::from_secs(60));

        assert_eq!(decoded.email, user.email);
        assert!(restarted.verify_remember_me_token(&decoded, &user));

        let changed_password = User::new(user.email.clone(), "$2a$10$changed".into());
        assert!(!restarted.verify_remember_me_token(&decoded, &changed_password));
        restarted.set_remember_me_policy(30, "rotated-key".into());
        assert!(!restarted.verify_remember_me_token(&decoded, &user));
    }

    #[test]
    fn spring_sha256_remember_me_cookie_fixture_is_accepted() {
        let token = "cmVhZGVyJTJCdGVzdCU0MGV4YW1wbGUub3JnOjQxMDI0NDQ4MDAwMDA6U0hBMjU2OjgwMGE1Njk1MjU3NmQ3NzA1YmM3MTNjNTlkZTcyNWE5M2FjZTYzYmE3MjIxMTZlOTY1ZDkyZWYyNzBhZDJkMDI";
        let user = User::new("reader+test@example.org".into(), "$2a$10$hash".into());
        let store = SessionStore::with_policy(30, "persistent-key".into(), 2, Duration::from_secs(60));
        let decoded = RememberMeToken::decode(token).unwrap();

        assert_eq!(decoded.email, user.email);
        assert!(store.verify_remember_me_token(&decoded, &user));
    }

    #[test]
    fn authentication_limiter_blocks_accounts_and_ips_after_failed_attempts() {
        let limiter = AuthenticationLimiter::with_policy(2, 3, Duration::from_secs(60));
        let first_ip = Some(ClientAddress("192.0.2.1".parse().unwrap()));
        let second_ip = Some(ClientAddress("192.0.2.2".parse().unwrap()));

        limiter.record_failure("reader@example.org", first_ip);
        assert!(limiter.check("reader@example.org", second_ip).is_ok());
        limiter.record_failure("READER@example.org", first_ip);
        assert!(limiter.check("reader@example.org", second_ip).is_err());
        limiter.record_success("reader@example.org");
        assert!(limiter.check("reader@example.org", second_ip).is_ok());

        for email in ["one@example.org", "two@example.org", "three@example.org"] {
            limiter.record_failure(email, first_ip);
        }
        assert!(limiter.check("other@example.org", first_ip).is_err());
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
