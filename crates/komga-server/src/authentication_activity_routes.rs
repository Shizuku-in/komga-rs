use axum::{
    Json,
    extract::{Extension, Path, RawQuery, State},
};
use komga_core::AuthenticationActivity;
use serde::Serialize;

use crate::{
    AppState,
    auth::AuthContext,
    error::ApiError,
    pagination::{SpringPage, parse_page_request},
    routes::require_admin,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthenticationActivityDto {
    user_id: Option<String>,
    email: Option<String>,
    api_key_id: Option<String>,
    api_key_comment: Option<String>,
    ip: Option<String>,
    user_agent: Option<String>,
    success: bool,
    error: Option<String>,
    date_time: String,
    source: Option<String>,
}

pub(crate) async fn current_user_activity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(query): RawQuery,
) -> Result<Json<SpringPage<AuthenticationActivityDto>>, ApiError> {
    let page = parse_page_request(query.as_deref())?;
    let activity = state
        .storage
        .list_authentication_activity(Some(&auth.user), &page)
        .await?;
    Ok(Json(SpringPage::from_content(
        activity.map(AuthenticationActivityDto::from),
    )))
}

pub(crate) async fn all_activity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(query): RawQuery,
) -> Result<Json<SpringPage<AuthenticationActivityDto>>, ApiError> {
    require_admin(&auth)?;
    let page = parse_page_request(query.as_deref())?;
    let activity = state.storage.list_authentication_activity(None, &page).await?;
    Ok(Json(SpringPage::from_content(
        activity.map(AuthenticationActivityDto::from),
    )))
}

pub(crate) async fn latest_user_activity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(user_id): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<AuthenticationActivityDto>, ApiError> {
    if !auth.user.is_admin() && auth.user.id != user_id {
        return Err(ApiError::forbidden());
    }
    let user = state
        .storage
        .find_user_by_id(&user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("User not found"))?;
    let api_key_id = query.as_deref().and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find_map(|(name, value)| (name == "apikey_id").then(|| value.into_owned()))
    });
    let activity = state
        .storage
        .latest_authentication_activity(&user, api_key_id.as_deref())
        .await?
        .ok_or_else(|| ApiError::not_found("Authentication activity not found"))?;
    Ok(Json(activity.into()))
}

impl From<AuthenticationActivity> for AuthenticationActivityDto {
    fn from(activity: AuthenticationActivity) -> Self {
        Self {
            user_id: activity.user_id,
            email: activity.email,
            api_key_id: activity.api_key_id,
            api_key_comment: activity.api_key_comment,
            ip: activity.ip,
            user_agent: activity.user_agent,
            success: activity.success,
            error: activity.error,
            date_time: activity.date_time.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            source: activity.source,
        }
    }
}
