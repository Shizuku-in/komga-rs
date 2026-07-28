use std::collections::{BTreeMap, BTreeSet};

use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
};

use crate::{
    AppState,
    auth::{AuthContext, optional_user},
    dto::{
        ClientSettingGlobalUpdateDto, ClientSettingUserUpdateDto, Patch, ServerSettingsDto, ServerSettingsUpdateDto,
        SettingMultiSource,
    },
    error::ApiError,
    routes::{ApiJson, require_admin},
};
use komga_storage::{ServerSettingsPatch, SettingUpdate};

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

pub(crate) async fn server_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<ServerSettingsDto>, ApiError> {
    require_admin(&auth)?;
    let settings = state.storage.server_settings().await?;
    Ok(Json(ServerSettingsDto {
        delete_empty_collections: settings.delete_empty_collections,
        delete_empty_read_lists: settings.delete_empty_read_lists,
        remember_me_duration_days: settings.remember_me_duration_days,
        thumbnail_size: settings.thumbnail_size,
        task_pool_size: settings.task_pool_size,
        server_port: SettingMultiSource {
            configuration_source: Some(state.configuration_server_port),
            database_source: settings.server_port,
            effective_value: state.effective_server_port,
        },
        server_context_path: SettingMultiSource {
            configuration_source: state.configuration_server_context_path.clone(),
            database_source: settings.server_context_path,
            effective_value: Some(state.effective_server_context_path.clone()),
        },
        kobo_proxy: state.kobo_proxy.is_enabled(),
        kobo_port: settings.kobo_port,
        kepubify_path: SettingMultiSource {
            configuration_source: state
                .configuration_kepubify_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            database_source: settings.kepubify_path,
            effective_value: state
                .kobo_kepub
                .effective_path()
                .map(|path| path.to_string_lossy().into_owned()),
        },
    }))
}

pub(crate) async fn update_server_settings(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(update): ApiJson<ServerSettingsUpdateDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    validate_server_settings(&update)?;
    let remember_me_key_renewed = update.renew_remember_me_key == Some(true);
    let patch = ServerSettingsPatch {
        delete_empty_collections: update.delete_empty_collections,
        delete_empty_read_lists: update.delete_empty_read_lists,
        remember_me_duration_days: update.remember_me_duration_days.map(|value| value as u64),
        renew_remember_me_key: remember_me_key_renewed,
        thumbnail_size: update.thumbnail_size,
        task_pool_size: update.task_pool_size.map(|value| value as usize),
        server_port: setting_update(update.server_port),
        server_context_path: setting_update(update.server_context_path),
        kobo_proxy: update.kobo_proxy,
        kobo_port: setting_update(update.kobo_port),
        kepubify_path: setting_update(update.kepubify_path),
    };
    let settings = state.storage.update_server_settings(&patch).await?;
    state.apply_server_settings(&settings).await;
    Ok(StatusCode::NO_CONTENT)
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

fn validate_server_settings(settings: &ServerSettingsUpdateDto) -> Result<(), ApiError> {
    if settings.remember_me_duration_days.is_some_and(|value| value <= 0) {
        return Err(ApiError::bad_request("rememberMeDurationDays must be positive"));
    }
    if settings.task_pool_size.is_some_and(|value| value <= 0) {
        return Err(ApiError::bad_request("taskPoolSize must be positive"));
    }
    for (name, value) in [("serverPort", &settings.server_port), ("koboPort", &settings.kobo_port)] {
        if matches!(value, Patch::Value(0)) {
            return Err(ApiError::bad_request(format!("{name} must be positive")));
        }
    }
    if let Patch::Value(value) = &settings.server_context_path
        && !komga_core::valid_server_context_path(value)
    {
        return Err(ApiError::bad_request("invalid serverContextPath"));
    }
    Ok(())
}

fn setting_update<T>(patch: Patch<T>) -> SettingUpdate<T> {
    match patch {
        Patch::Missing => SettingUpdate::Keep,
        Patch::Null => SettingUpdate::Clear,
        Patch::Value(value) => SettingUpdate::Set(value),
    }
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
