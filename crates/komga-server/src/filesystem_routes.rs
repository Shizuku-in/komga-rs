use std::{
    fs,
    path::{Path, PathBuf},
};

use axum::{
    Json,
    extract::{Extension, State},
};
use serde::Serialize;

use crate::{AppState, auth::AuthContext, dto::DirectoryRequestDto, error::ApiError, routes::require_admin};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DirectoryListingDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    directories: Vec<PathDto>,
    files: Vec<PathDto>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PathDto {
    #[serde(rename = "type")]
    path_type: &'static str,
    name: String,
    path: String,
}

pub(crate) async fn directory_listing(
    State(_state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    payload: Option<Json<DirectoryRequestDto>>,
) -> Result<Json<DirectoryListingDto>, ApiError> {
    require_admin(&auth)?;
    let request = payload.map(|Json(value)| value).unwrap_or_default();
    if request.path.is_empty() {
        #[cfg(windows)]
        let roots = (b'A'..=b'Z')
            .map(|drive| PathBuf::from(format!("{}:\\", drive as char)))
            .filter(|path| path.exists())
            .map(|path| to_dto(&path))
            .collect();
        #[cfg(not(windows))]
        let roots = vec![to_dto(Path::new("/"))];
        return Ok(Json(DirectoryListingDto {
            parent: None,
            directories: roots,
            files: Vec::new(),
        }));
    }

    let path = PathBuf::from(&request.path);
    if !path.is_absolute() {
        return Err(ApiError::bad_request("Path must be absolute"));
    }
    let directory = if path.is_dir() {
        path.clone()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ApiError::bad_request("Path does not exist"))?
    };
    let entries = fs::read_dir(&directory).map_err(|_| ApiError::bad_request("Path does not exist"))?;
    let mut paths = entries
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| !is_hidden(entry))
        .filter(|entry| request.show_files || entry.is_dir())
        .collect::<Vec<_>>();
    paths.sort_by_cached_key(|entry| entry.to_string_lossy().to_lowercase());
    let (directories, files) = paths.into_iter().partition::<Vec<_>, _>(|entry| entry.is_dir());
    Ok(Json(DirectoryListingDto {
        parent: Some(
            path.parent()
                .unwrap_or_else(|| Path::new(""))
                .to_string_lossy()
                .into_owned(),
        ),
        directories: directories.iter().map(|entry| to_dto(entry)).collect(),
        files: files.iter().map(|entry| to_dto(entry)).collect(),
    }))
}

fn to_dto(path: &Path) -> PathDto {
    PathDto {
        path_type: if path.is_dir() { "directory" } else { "file" },
        name: path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned(),
        path: path.to_string_lossy().into_owned(),
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}
