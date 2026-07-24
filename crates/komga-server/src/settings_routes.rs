use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
};

use crate::{
    AppState,
    auth::{AuthContext, optional_user},
    dto::{ClientSettingGlobalUpdateDto, ClientSettingUserUpdateDto},
    error::ApiError,
    routes::{ApiJson, require_admin},
};

pub(crate) async fn list_global_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<BTreeMap<String, komga_storage::ClientSetting>>, ApiError> {
    let anonymous = optional_user(&state, &headers).await?.is_none();
    Ok(Json(state.storage.list_global_client_settings(anonymous).await?))
}

pub(crate) async fn list_user_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<BTreeMap<String, komga_storage::ClientSetting>>, ApiError> {
    Ok(Json(state.storage.list_user_client_settings(&auth.user.id).await?))
}

pub(crate) async fn save_global_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(settings): ApiJson<BTreeMap<String, ClientSettingGlobalUpdateDto>>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    validate_settings(
        settings
            .iter()
            .map(|(key, setting)| (key.as_str(), setting.value.as_str())),
    )?;
    let settings = settings
        .into_iter()
        .map(|(key, setting)| (key, setting.value, setting.allow_unauthorized))
        .collect::<Vec<_>>();
    state.storage.save_global_client_settings(&settings).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn save_user_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(settings): ApiJson<BTreeMap<String, ClientSettingUserUpdateDto>>,
) -> Result<StatusCode, ApiError> {
    validate_settings(
        settings
            .iter()
            .map(|(key, setting)| (key.as_str(), setting.value.as_str())),
    )?;
    let settings = settings
        .into_iter()
        .map(|(key, setting)| (key, setting.value))
        .collect::<Vec<_>>();
    state
        .storage
        .save_user_client_settings(&auth.user.id, &settings)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_global_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(keys): ApiJson<BTreeSet<String>>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    validate_keys(keys.iter().map(String::as_str))?;
    state
        .storage
        .delete_global_client_settings(&keys.into_iter().collect::<Vec<_>>())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_user_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(keys): ApiJson<BTreeSet<String>>,
) -> Result<StatusCode, ApiError> {
    validate_keys(keys.iter().map(String::as_str))?;
    state
        .storage
        .delete_user_client_settings(&auth.user.id, &keys.into_iter().collect::<Vec<_>>())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn oauth_providers() -> Json<Vec<serde_json::Value>> {
    Json(Vec::new())
}

pub(crate) async fn clear_tasks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<u64>, ApiError> {
    require_admin(&auth)?;
    Ok(Json(state.tasks.delete_all_unowned().await?))
}

fn validate_settings<'a>(settings: impl Iterator<Item = (&'a str, &'a str)>) -> Result<(), ApiError> {
    for (key, value) in settings {
        validate_key(key)?;
        if value.trim().is_empty() {
            return Err(ApiError::bad_request("setting value must not be blank"));
        }
    }
    Ok(())
}

fn validate_keys<'a>(keys: impl Iterator<Item = &'a str>) -> Result<(), ApiError> {
    for key in keys {
        validate_key(key)?;
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), ApiError> {
    let mut segments = key.split('.');
    let Some(first) = segments.next() else {
        return Err(ApiError::bad_request("invalid client setting key"));
    };
    if !valid_segment(first, true) || segments.any(|segment| !valid_segment(segment, false)) {
        return Err(ApiError::bad_request("invalid client setting key"));
    }
    Ok(())
}

fn valid_segment(segment: &str, first_segment: bool) -> bool {
    let bytes = segment.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    let first_ok = if first_segment {
        bytes[0].is_ascii_lowercase()
    } else {
        bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()
    };
    first_ok
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
}
