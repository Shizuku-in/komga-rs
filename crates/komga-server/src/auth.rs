//! Compatibility authentication and process-local session management.
//!
//! Authentication is attempted in this order: `X-API-Key`, an existing header
//! or cookie session, then HTTP Basic credentials. Supplying an API key makes it
//! authoritative; an invalid key does not fall back to another credential.
//! Successful Basic authentication creates a session and returns it through
//! `X-Auth-Token` when that header was present on the request, otherwise through
//! `KOMGA-SESSION`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use dashmap::DashMap;
use komga_core::User;
use rand::RngCore;
use sha2::{Digest, Sha512};

use crate::{AppState, error::ApiError};

/// Cookie name used for ordinary Komga sessions.
pub const SESSION_COOKIE: &str = "KOMGA-SESSION";
/// Header used to transport a Komga session without a cookie.
pub const SESSION_HEADER: &str = "X-Auth-Token";
/// Cookie name used for extended login sessions.
pub const REMEMBER_ME_COOKIE: &str = "komga-remember-me";

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
    expires_at: Instant,
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    sessions: Arc<DashMap<String, Session>>,
    remember_me_duration: Duration,
}

impl SessionStore {
    pub(crate) fn new(remember_me_duration_days: u64) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            remember_me_duration: Duration::from_secs(remember_me_duration_days.saturating_mul(24 * 60 * 60)),
        }
    }

    /// Issues a cryptographically random session with the selected lifetime.
    pub(crate) fn create(&self, user_id: &str, remember_me: bool) -> String {
        let mut bytes = [0_u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let duration = if remember_me {
            self.remember_me_duration
        } else {
            Duration::from_secs(7 * 24 * 60 * 60)
        };
        self.sessions.insert(
            id.clone(),
            Session {
                user_id: user_id.to_owned(),
                expires_at: Instant::now() + duration,
            },
        );
        id
    }

    /// Resolves a live session and eagerly removes it when expired.
    pub(crate) fn user_id(&self, session_id: &str) -> Option<String> {
        let session = self.sessions.get(session_id)?;
        if session.expires_at <= Instant::now() {
            drop(session);
            self.sessions.remove(session_id);
            return None;
        }
        Some(session.user_id.clone())
    }

    /// Invalidates one session.
    pub(crate) fn remove(&self, session_id: &str) {
        self.sessions.remove(session_id);
    }

    /// Invalidates every session owned by a user after security policy changes.
    pub(crate) fn remove_user(&self, user_id: &str) {
        self.sessions.retain(|_, session| session.user_id != user_id);
    }

    pub(crate) fn remember_me_max_age(&self) -> u64 {
        self.remember_me_duration.as_secs()
    }
}

enum AuthSource {
    ApiKey,
    Session(String),
    Basic { remember_me: bool },
}

/// Authenticates protected requests and attaches [`AuthContext`] for handlers.
pub(crate) async fn require_auth(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let opds_v2 = request.uri().path().starts_with("/opds/v2/");
    let request_headers = request.headers().clone();
    let request_uri = request.uri().clone();
    match authenticate(&state, request.headers(), request.uri().query()).await {
        Ok((user, source)) => {
            let (session_id, issue_session, remember_me) = match source {
                AuthSource::ApiKey => (None, false, false),
                AuthSource::Session(id) => (Some(id), false, false),
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
                    append_cookie(response.headers_mut(), &session_cookie(&session_id));
                }
                if remember_me {
                    append_cookie(
                        response.headers_mut(),
                        &remember_me_cookie(&session_id, state.sessions.remember_me_max_age()),
                    );
                }
            }
            response
        }
        Err(error) => {
            if opds_v2 {
                crate::opds_v2::unauthorized(&request_headers, &request_uri)
            } else {
                error.into_response()
            }
        }
    }
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> Result<(User, AuthSource), ApiError> {
    if let Some(key) = headers.get("X-API-Key") {
        let key = key.to_str().map_err(|_| ApiError::unauthorized())?;
        let digest = hex::encode(Sha512::digest(key.as_bytes()));
        return state
            .storage
            .find_user_by_api_key_hash(&digest)
            .await?
            .map(|user| (user, AuthSource::ApiKey))
            .ok_or_else(ApiError::unauthorized);
    }

    let header_session = headers.get(SESSION_HEADER).and_then(|value| value.to_str().ok());
    let cookie_session = cookie_value(headers, SESSION_COOKIE).or_else(|| cookie_value(headers, REMEMBER_ME_COOKIE));
    if let Some(session_id) = header_session
        .filter(|value| !value.is_empty())
        .or(cookie_session.as_deref())
        && let Some(user_id) = state.sessions.user_id(session_id)
        && let Some(user) = state.storage.find_user_by_id(&user_id).await?
    {
        return Ok((user, AuthSource::Session(session_id.to_owned())));
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
pub(crate) fn session_cookie(session_id: &str) -> String {
    format!("{SESSION_COOKIE}={session_id}; Path=/; HttpOnly; SameSite=Lax")
}

fn remember_me_cookie(session_id: &str, max_age: u64) -> String {
    format!(
        "{REMEMBER_ME_COOKIE}={session_id}; Path=/; Max-Age={}; HttpOnly; SameSite=Lax",
        max_age
    )
}

/// Returns deletion cookies for both supported browser-session mechanisms.
pub(crate) fn expired_session_cookies() -> [&'static str; 2] {
    [
        "KOMGA-SESSION=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
        "komga-remember-me=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
    ]
}

/// Produces the lowercase SHA-512 representation used by Kotlin API keys.
pub(crate) fn sha512_hex(value: &str) -> String {
    hex::encode(Sha512::digest(value.as_bytes()))
}
