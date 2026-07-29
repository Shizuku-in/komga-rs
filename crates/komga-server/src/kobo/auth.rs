use axum::{
    extract::{Request, State},
    middleware::Next,
    response::IntoResponse,
    response::Response,
};
use komga_core::{User, UserRole};

use crate::{
    AppState,
    auth::{
        authentication_request_metadata, record_api_key_authentication_failure, record_api_key_authentication_success,
        sha512_hex,
    },
    error::ApiError,
};

#[derive(Debug, Clone)]
pub(super) struct KoboAuthContext {
    pub user: User,
    pub api_key_id: String,
    pub api_key_comment: String,
    pub auth_token: String,
}

pub(super) async fn require_kobo_auth(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let request_metadata = authentication_request_metadata(&request);
    let Some(auth_token) = request
        .uri()
        .path()
        .strip_prefix("/kobo/")
        .and_then(|path| path.split('/').next())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
    else {
        return ApiError::unauthorized().into_response();
    };
    let api_key = match state.storage.find_api_key_by_hash(&sha512_hex(&auth_token)).await {
        Ok(Some(api_key)) => api_key,
        Ok(None) => {
            record_api_key_authentication_failure(&state, &request_metadata).await;
            return ApiError::unauthorized().into_response();
        }
        Err(error) => {
            record_api_key_authentication_failure(&state, &request_metadata).await;
            return ApiError::from(error).into_response();
        }
    };
    let user = match state.storage.find_user_by_id(&api_key.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            record_api_key_authentication_failure(&state, &request_metadata).await;
            return ApiError::unauthorized().into_response();
        }
        Err(error) => {
            record_api_key_authentication_failure(&state, &request_metadata).await;
            return ApiError::from(error).into_response();
        }
    };
    record_api_key_authentication_success(&state, &request_metadata, &user, &api_key).await;
    if !user.roles.contains(&UserRole::KoboSync) {
        return ApiError::forbidden().into_response();
    }
    request.extensions_mut().insert(KoboAuthContext {
        user,
        api_key_id: api_key.id,
        api_key_comment: api_key.comment,
        auth_token,
    });
    next.run(request).await
}
