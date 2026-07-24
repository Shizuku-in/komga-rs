//! ComicRack read-list matching HTTP contract.

use axum::{
    Json,
    extract::{Extension, Multipart, State},
};

use crate::{AppState, auth::AuthContext, error::ApiError, routes::require_admin};

pub(crate) async fn match_comicrack_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    mut multipart: Multipart,
) -> Result<Json<komga_core::ReadListRequestMatchDto>, ApiError> {
    require_admin(&auth)?;
    let mut bytes = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?
    {
        if field.name() == Some("file") && bytes.is_none() {
            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(|error| ApiError::bad_request(error.to_string()))?
                    .to_vec(),
            );
        }
    }
    let bytes = bytes.ok_or_else(|| ApiError::bad_request("file is required"))?;
    let request = komga_media::parse_read_list(&bytes).map_err(|error| ApiError::bad_request(error.to_string()))?;
    Ok(Json(state.storage.match_read_list_request(request).await?))
}
