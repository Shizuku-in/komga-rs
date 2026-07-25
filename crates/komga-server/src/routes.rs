//! HTTP handlers for the first Komga compatibility slice.
//!
//! Handlers keep transport validation and status-code mapping here while domain
//! policy remains in `komga-core` and all SQL remains in `komga-storage`.

use std::collections::BTreeSet;

use axum::{
    Json,
    body::Bytes,
    extract::{Extension, FromRequest, Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use komga_core::{
    ALL_ROLES, AgeRestriction, AllowExclude, ApiKey, ContentRestrictions, DomainEvent, User, UserEvent, UserRole,
    new_id,
};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{
    AppState,
    auth::{AuthContext, append_cookie, expired_session_cookies, session_cookie},
    dto::{
        AgeRestrictionUpdateDto, ApiKeyRequestDto, PasswordUpdateDto, Patch, RestrictionUpdate, UserCreationDto,
        UserUpdateDto,
    },
    error::ApiError,
};

pub(crate) async fn health(State(state): State<AppState>) -> Response {
    match state.storage.health_check().await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "UP"}))).into_response(),
        Err(error) => {
            tracing::error!(%error, "health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"status": "DOWN"})),
            )
                .into_response()
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaimStatus {
    is_claimed: bool,
}

pub(crate) async fn claim_status(State(state): State<AppState>) -> Result<Json<ClaimStatus>, ApiError> {
    Ok(Json(ClaimStatus {
        is_claimed: state.storage.count_users().await? > 0,
    }))
}

pub(crate) async fn claim_server(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<komga_core::UserDto>, ApiError> {
    let email = required_header(&headers, "X-Komga-Email")?;
    let password = required_header(&headers, "X-Komga-Password")?;
    validate_email(&email)?;
    validate_password(&password)?;

    let _guard = state.claim_lock.lock().await;
    if state.storage.count_users().await? > 0 {
        return Err(ApiError::bad_request("This server has already been claimed"));
    }

    let password_hash = hash_password(password).await?;
    let mut user = User::new(email, password_hash);
    user.roles = ALL_ROLES.into_iter().collect();
    state.storage.create_user(&user).await?;
    Ok(Json(user.to_dto()))
}

pub(crate) async fn current_user(Extension(auth): Extension<AuthContext>) -> Json<komga_core::UserDto> {
    Json(auth.user.to_dto())
}

pub(crate) async fn list_users(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Vec<komga_core::UserDto>>, ApiError> {
    require_admin(&auth)?;
    Ok(Json(
        state.storage.list_users().await?.iter().map(User::to_dto).collect(),
    ))
}

pub(crate) async fn create_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(payload): ApiJson<UserCreationDto>,
) -> Result<(StatusCode, Json<komga_core::UserDto>), ApiError> {
    require_admin(&auth)?;
    validate_email(&payload.email)?;
    validate_password(&payload.password)?;

    let password_hash = hash_password(payload.password).await?;
    let mut user = User::new(payload.email, password_hash);
    user.roles = parse_roles(payload.roles);
    user.restrictions = ContentRestrictions::new(
        parse_age_restriction(payload.age_restriction)?,
        payload.labels_allow.unwrap_or_default(),
        payload.labels_exclude.unwrap_or_default(),
    );
    if let Some(shared) = payload.shared_libraries {
        user.shared_all_libraries = shared.all;
        if !shared.all {
            user.shared_library_ids = shared.library_ids;
        }
    }
    state.storage.create_user(&user).await?;
    let created = state
        .storage
        .find_user_by_id(&user.id)
        .await?
        .ok_or_else(|| ApiError::internal("Created user could not be loaded"))?;
    Ok((StatusCode::CREATED, Json(created.to_dto())))
}

pub(crate) async fn patch_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
    ApiJson(patch): ApiJson<UserUpdateDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if auth.user.id == user_id {
        return Err(ApiError::forbidden());
    }
    let mut user = state
        .storage
        .find_user_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;

    match patch.roles {
        Patch::Missing => {}
        Patch::Null => return Err(ApiError::bad_request("roles cannot be null")),
        Patch::Value(roles) => user.roles = parse_roles(roles),
    }
    match patch.shared_libraries {
        Patch::Missing => {}
        Patch::Null => return Err(ApiError::bad_request("sharedLibraries cannot be null")),
        Patch::Value(shared) => {
            user.shared_all_libraries = shared.all;
            user.shared_library_ids = if shared.all {
                BTreeSet::new()
            } else {
                shared.library_ids
            };
        }
    }

    let age_restriction = match patch.age_restriction {
        Patch::Missing => user.restrictions.age_restriction.clone(),
        Patch::Null => None,
        Patch::Value(value) => parse_age_restriction(Some(value))?,
    };
    let labels_allow = match patch.labels_allow {
        Patch::Missing => user.restrictions.labels_allow.clone(),
        Patch::Null => BTreeSet::new(),
        Patch::Value(value) => value,
    };
    let labels_exclude = match patch.labels_exclude {
        Patch::Missing => user.restrictions.labels_exclude.clone(),
        Patch::Null => BTreeSet::new(),
        Patch::Value(value) => value,
    };
    user.restrictions = ContentRestrictions::new(age_restriction, labels_allow, labels_exclude);

    state.storage.update_user(&user).await?;
    state.sessions.remove_user(&user_id);
    state.publish_domain_event(DomainEvent::UserUpdated(UserEvent {
        user_id,
        expire_sessions: true,
    }));
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if auth.user.id == user_id {
        return Err(ApiError::forbidden());
    }
    if !state.storage.delete_user(&user_id).await? {
        return Err(ApiError::not_found("User not found"));
    }
    state.sessions.remove_user(&user_id);
    state.publish_domain_event(DomainEvent::UserDeleted(UserEvent {
        user_id,
        expire_sessions: true,
    }));
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn update_my_password(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(payload): ApiJson<PasswordUpdateDto>,
) -> Result<StatusCode, ApiError> {
    update_password(&state, &auth.user.id, payload.password, false).await
}

pub(crate) async fn update_user_password(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
    ApiJson(payload): ApiJson<PasswordUpdateDto>,
) -> Result<StatusCode, ApiError> {
    if !auth.user.is_admin() && auth.user.id != user_id {
        return Err(ApiError::forbidden());
    }
    update_password(&state, &user_id, payload.password, auth.user.id != user_id).await
}

async fn update_password(
    state: &AppState,
    user_id: &str,
    password: String,
    expire_sessions: bool,
) -> Result<StatusCode, ApiError> {
    validate_password(&password)?;
    let password_hash = hash_password(password).await?;
    if !state.storage.update_password(user_id, &password_hash).await? {
        return Err(ApiError::not_found("User not found"));
    }
    if expire_sessions {
        state.sessions.remove_user(user_id);
    }
    state.publish_domain_event(DomainEvent::UserUpdated(UserEvent {
        user_id: user_id.to_owned(),
        expire_sessions,
    }));
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn list_api_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Vec<ApiKey>>, ApiError> {
    let keys = state
        .storage
        .list_api_keys(&auth.user.id)
        .await?
        .into_iter()
        .map(|mut key| {
            key.key = "******".into();
            key
        })
        .collect();
    Ok(Json(keys))
}

pub(crate) async fn create_api_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(payload): ApiJson<ApiKeyRequestDto>,
) -> Result<Json<ApiKey>, ApiError> {
    let comment = payload.comment.trim();
    if comment.is_empty() {
        return Err(ApiError::bad_request("comment must not be blank"));
    }
    let plain_key = Uuid::new_v4().simple().to_string();
    let mut api_key = state
        .storage
        .create_api_key(&new_id(), &auth.user.id, &crate::auth::sha512_hex(&plain_key), comment)
        .await?;
    api_key.key = plain_key;
    Ok(Json(api_key))
}

pub(crate) async fn delete_api_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(key_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !state.storage.delete_api_key(&auth.user.id, &key_id).await? {
        return Err(ApiError::not_found("API key not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn set_cookie(Extension(auth): Extension<AuthContext>) -> Result<Response, ApiError> {
    let session_id = auth
        .session_id
        .ok_or_else(|| ApiError::bad_request("An existing session is required"))?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    append_cookie(response.headers_mut(), &session_cookie(&session_id));
    Ok(response)
}

pub(crate) async fn logout(State(state): State<AppState>, Extension(auth): Extension<AuthContext>) -> Response {
    if let Some(session_id) = auth.session_id {
        state.sessions.remove(&session_id);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    for cookie in expired_session_cookies() {
        append_cookie(response.headers_mut(), cookie);
    }
    response
}

pub(crate) fn require_admin(auth: &AuthContext) -> Result<(), ApiError> {
    auth.user.is_admin().then_some(()).ok_or_else(ApiError::forbidden)
}

fn parse_roles(roles: BTreeSet<String>) -> BTreeSet<UserRole> {
    roles
        .iter()
        .filter_map(|role| UserRole::try_from(role.as_str()).ok())
        .collect()
}

fn parse_age_restriction(value: Option<AgeRestrictionUpdateDto>) -> Result<Option<AgeRestriction>, ApiError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.age < 0 {
        return Err(ApiError::bad_request("age must be greater than or equal to 0"));
    }
    let restriction = match value.restriction {
        RestrictionUpdate::AllowOnly => AllowExclude::AllowOnly,
        RestrictionUpdate::Exclude => AllowExclude::Exclude,
        RestrictionUpdate::None => return Ok(None),
    };
    Ok(Some(AgeRestriction {
        age: value.age as u32,
        restriction,
    }))
}

fn required_header(headers: &HeaderMap, name: &str) -> Result<String, ApiError> {
    let value = headers
        .get(name)
        .ok_or_else(|| ApiError::bad_request(format!("Missing required header: {name}")))?
        .to_str()
        .map_err(|_| ApiError::bad_request(format!("Invalid header: {name}")))?;
    if value.trim().is_empty() {
        return Err(ApiError::bad_request(format!("Header must not be blank: {name}")));
    }
    Ok(value.to_owned())
}

fn validate_email(email: &str) -> Result<(), ApiError> {
    let Some((local, domain)) = email.split_once('@') else {
        return Err(ApiError::bad_request("email must be a well-formed email address"));
    };
    if local.is_empty()
        || domain.starts_with('.')
        || domain.ends_with('.')
        || !domain.contains('.')
        || email.chars().any(char::is_whitespace)
    {
        return Err(ApiError::bad_request("email must be a well-formed email address"));
    }
    Ok(())
}

fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.trim().is_empty() {
        Err(ApiError::bad_request("password must not be blank"))
    } else {
        Ok(())
    }
}

async fn hash_password(password: String) -> Result<String, ApiError> {
    // Spring's BCryptPasswordEncoder defaults to strength 10.
    tokio::task::spawn_blocking(move || bcrypt::hash(password, 10))
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

pub(crate) struct ApiJson<T>(pub(crate) T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|error| ApiError::bad_request(format!("Invalid request body: {error}")))?;
        serde_json::from_slice(&bytes)
            .map(Self)
            .map_err(|error| ApiError::bad_request(format!("Invalid JSON body: {error}")))
    }
}
