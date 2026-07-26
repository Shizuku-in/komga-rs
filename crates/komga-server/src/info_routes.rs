//! Administrative server build information.

use axum::{Json, extract::Extension};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{auth::AuthContext, error::ApiError, routes::require_admin};

#[derive(Debug, Serialize)]
pub(crate) struct InfoDto {
    git: GitInfo,
    build: BuildInfo,
}

#[derive(Debug, Serialize)]
pub(crate) struct GitInfo {
    commit: GitCommitInfo,
    branch: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct GitCommitInfo {
    id: &'static str,
    time: &'static str,
}

#[derive(Debug, Serialize)]
pub(crate) struct BuildInfo {
    version: &'static str,
    artifact: &'static str,
    name: &'static str,
    group: &'static str,
    time: DateTime<Utc>,
}

pub(crate) async fn info(Extension(auth): Extension<AuthContext>) -> Result<Json<InfoDto>, ApiError> {
    require_admin(&auth)?;
    let build_time = env!("KOMGA_BUILD_TIME").parse::<i64>().unwrap_or_default();
    Ok(Json(InfoDto {
        git: GitInfo {
            commit: GitCommitInfo {
                id: env!("KOMGA_GIT_COMMIT_ID"),
                time: env!("KOMGA_GIT_COMMIT_TIME"),
            },
            branch: env!("KOMGA_GIT_BRANCH"),
        },
        build: BuildInfo {
            version: env!("CARGO_PKG_VERSION"),
            artifact: "komga",
            name: "komga",
            group: "org.gotson",
            time: DateTime::from_timestamp(build_time, 0).unwrap_or(DateTime::UNIX_EPOCH),
        },
    }))
}
