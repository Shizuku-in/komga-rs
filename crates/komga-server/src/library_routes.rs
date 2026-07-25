use std::{collections::BTreeSet, path::PathBuf};

use axum::{
    Json,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
};
use komga_core::{Library, ScanInterval, SeriesCover};
use komga_search::EntityKind;
use komga_storage::{DeleteLibraryResult, TaskRecord};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    AppState,
    auth::AuthContext,
    dto::{LibraryCreationDto, LibraryUpdateDto, Patch},
    error::ApiError,
    routes::{ApiJson, require_admin},
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryDto {
    id: String,
    name: String,
    root: String,
    import_comic_info_book: bool,
    import_comic_info_series: bool,
    import_comic_info_collection: bool,
    import_comic_info_read_list: bool,
    import_comic_info_series_append_volume: bool,
    import_epub_book: bool,
    import_epub_series: bool,
    import_mylar_series: bool,
    import_local_artwork: bool,
    import_barcode_isbn: bool,
    scan_force_modified_time: bool,
    scan_interval: ScanInterval,
    scan_on_startup: bool,
    scan_cbx: bool,
    scan_pdf: bool,
    scan_epub: bool,
    scan_directory_exclusions: BTreeSet<String>,
    repair_extensions: bool,
    convert_to_cbz: bool,
    empty_trash_after_scan: bool,
    series_cover: SeriesCover,
    hash_files: bool,
    hash_pages: bool,
    hash_koreader: bool,
    analyze_dimensions: bool,
    oneshots_directory: Option<String>,
    unavailable: bool,
}

impl LibraryDto {
    fn from_library(library: Library, include_root: bool) -> Self {
        let root = if include_root {
            library
                .root_path()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            String::new()
        };
        Self {
            id: library.id,
            name: library.name,
            root,
            import_comic_info_book: library.import_comic_info_book,
            import_comic_info_series: library.import_comic_info_series,
            import_comic_info_collection: library.import_comic_info_collection,
            import_comic_info_read_list: library.import_comic_info_read_list,
            import_comic_info_series_append_volume: library.import_comic_info_series_append_volume,
            import_epub_book: library.import_epub_book,
            import_epub_series: library.import_epub_series,
            import_mylar_series: library.import_mylar_series,
            import_local_artwork: library.import_local_artwork,
            import_barcode_isbn: library.import_barcode_isbn,
            scan_force_modified_time: library.scan_force_modified_time,
            scan_interval: library.scan_interval,
            scan_on_startup: library.scan_on_startup,
            scan_cbx: library.scan_cbx,
            scan_pdf: library.scan_pdf,
            scan_epub: library.scan_epub,
            scan_directory_exclusions: library.scan_directory_exclusions,
            repair_extensions: library.repair_extensions,
            convert_to_cbz: library.convert_to_cbz,
            empty_trash_after_scan: library.empty_trash_after_scan,
            series_cover: library.series_cover,
            hash_files: library.hash_files,
            hash_pages: library.hash_pages,
            hash_koreader: library.hash_koreader,
            analyze_dimensions: library.analyze_dimensions,
            oneshots_directory: library.oneshots_directory,
            unavailable: library.unavailable_date.is_some(),
        }
    }
}

pub(crate) async fn list_libraries(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Vec<LibraryDto>>, ApiError> {
    let mut libraries = state.storage.list_libraries().await?;
    libraries.retain(|library| auth.user.can_access_library(&library.id));
    libraries.sort_by_cached_key(|library| library.name.to_lowercase());
    Ok(Json(
        libraries
            .into_iter()
            .map(|library| LibraryDto::from_library(library, auth.user.is_admin()))
            .collect(),
    ))
}

pub(crate) async fn get_library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
) -> Result<Json<LibraryDto>, ApiError> {
    let library = state
        .storage
        .find_library_by_id(&library_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Library not found"))?;
    if !auth.user.can_access_library(&library.id) {
        return Err(ApiError::forbidden());
    }
    Ok(Json(LibraryDto::from_library(library, auth.user.is_admin())))
}

pub(crate) async fn create_library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(payload): ApiJson<LibraryCreationDto>,
) -> Result<Json<LibraryDto>, ApiError> {
    require_admin(&auth)?;
    validate_not_blank("name", &payload.name)?;
    let root = validated_root(&payload.root)?;
    let mut library = Library::new(payload.name, root);
    library.import_comic_info_book = payload.import_comic_info_book;
    library.import_comic_info_series = payload.import_comic_info_series;
    library.import_comic_info_collection = payload.import_comic_info_collection;
    library.import_comic_info_read_list = payload.import_comic_info_read_list;
    library.import_comic_info_series_append_volume = payload.import_comic_info_series_append_volume;
    library.import_epub_book = payload.import_epub_book;
    library.import_epub_series = payload.import_epub_series;
    library.import_mylar_series = payload.import_mylar_series;
    library.import_local_artwork = payload.import_local_artwork;
    library.import_barcode_isbn = payload.import_barcode_isbn;
    library.scan_force_modified_time = payload.scan_force_modified_time;
    library.scan_interval = payload.scan_interval;
    library.scan_on_startup = payload.scan_on_startup;
    library.scan_cbx = payload.scan_cbx;
    library.scan_pdf = payload.scan_pdf;
    library.scan_epub = payload.scan_epub;
    library.scan_directory_exclusions = payload.scan_directory_exclusions;
    library.repair_extensions = payload.repair_extensions;
    library.convert_to_cbz = payload.convert_to_cbz;
    library.empty_trash_after_scan = payload.empty_trash_after_scan;
    library.series_cover = payload.series_cover;
    library.hash_files = payload.hash_files;
    library.hash_pages = payload.hash_pages;
    library.hash_koreader = payload.hash_koreader;
    library.analyze_dimensions = payload.analyze_dimensions;
    library.oneshots_directory = payload.oneshots_directory.and_then(non_blank);

    validate_library(&library, &state.storage.list_libraries().await?)?;
    state.storage.create_library(&library).await?;
    state
        .enqueue_task(&TaskRecord::scan_library(&library.id, false, 4))
        .await?;
    state.library_schedule_changed();
    let created = state
        .storage
        .find_library_by_id(&library.id)
        .await?
        .ok_or_else(|| ApiError::internal("Created library could not be loaded"))?;
    Ok(Json(LibraryDto::from_library(created, true)))
}

pub(crate) async fn update_library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
    ApiJson(patch): ApiJson<LibraryUpdateDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    let current = state
        .storage
        .find_library_by_id(&library_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Library not found"))?;
    let mut updated = current.clone();

    apply_value(&mut updated.name, patch.name);
    if let Patch::Value(root) = patch.root {
        updated.root = validated_root(&root)?;
    }
    apply_value(&mut updated.import_comic_info_book, patch.import_comic_info_book);
    apply_value(&mut updated.import_comic_info_series, patch.import_comic_info_series);
    apply_value(
        &mut updated.import_comic_info_collection,
        patch.import_comic_info_collection,
    );
    apply_value(
        &mut updated.import_comic_info_read_list,
        patch.import_comic_info_read_list,
    );
    apply_value(
        &mut updated.import_comic_info_series_append_volume,
        patch.import_comic_info_series_append_volume,
    );
    apply_value(&mut updated.import_epub_book, patch.import_epub_book);
    apply_value(&mut updated.import_epub_series, patch.import_epub_series);
    apply_value(&mut updated.import_mylar_series, patch.import_mylar_series);
    apply_value(&mut updated.import_local_artwork, patch.import_local_artwork);
    apply_value(&mut updated.import_barcode_isbn, patch.import_barcode_isbn);
    apply_value(&mut updated.scan_force_modified_time, patch.scan_force_modified_time);
    apply_value(&mut updated.scan_interval, patch.scan_interval);
    apply_value(&mut updated.scan_on_startup, patch.scan_on_startup);
    apply_value(&mut updated.scan_cbx, patch.scan_cbx);
    apply_value(&mut updated.scan_pdf, patch.scan_pdf);
    apply_value(&mut updated.scan_epub, patch.scan_epub);
    match patch.scan_directory_exclusions {
        Patch::Missing => {}
        Patch::Null => updated.scan_directory_exclusions.clear(),
        Patch::Value(value) => updated.scan_directory_exclusions = value,
    }
    apply_value(&mut updated.repair_extensions, patch.repair_extensions);
    apply_value(&mut updated.convert_to_cbz, patch.convert_to_cbz);
    apply_value(&mut updated.empty_trash_after_scan, patch.empty_trash_after_scan);
    apply_value(&mut updated.series_cover, patch.series_cover);
    apply_value(&mut updated.hash_files, patch.hash_files);
    apply_value(&mut updated.hash_pages, patch.hash_pages);
    apply_value(&mut updated.hash_koreader, patch.hash_koreader);
    apply_value(&mut updated.analyze_dimensions, patch.analyze_dimensions);
    match patch.oneshots_directory {
        Patch::Missing => {}
        Patch::Null => updated.oneshots_directory = None,
        Patch::Value(value) => updated.oneshots_directory = non_blank(value),
    }

    validate_not_blank("name", &updated.name)?;
    let others = state
        .storage
        .list_libraries()
        .await?
        .into_iter()
        .filter(|library| library.id != library_id)
        .collect::<Vec<_>>();
    validate_library(&updated, &others)?;
    state.storage.update_library(&updated).await?;
    if current.should_rescan(&updated) {
        state
            .enqueue_task(&TaskRecord::scan_library(&library_id, false, 4))
            .await?;
    }
    if current.scan_interval != updated.scan_interval {
        state.library_schedule_changed();
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    match state.storage.delete_library(&library_id).await? {
        DeleteLibraryResult::Deleted { book_ids, series_ids } => {
            let entities = book_ids
                .into_iter()
                .map(|id| (EntityKind::Book, id))
                .chain(series_ids.into_iter().map(|id| (EntityKind::Series, id)))
                .collect();
            if !state.delete_search_entities(entities).await {
                state.recover_search_after_http_failure().await;
            }
            state.library_schedule_changed();
            Ok(StatusCode::NO_CONTENT)
        }
        DeleteLibraryResult::NotFound => Err(ApiError::not_found("Library not found")),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ScanQuery {
    #[serde(default)]
    deep: bool,
}

pub(crate) async fn scan_library(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
    Query(query): Query<ScanQuery>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    ensure_library_exists(&state, &library_id).await?;
    state
        .enqueue_task(&TaskRecord::scan_library(&library_id, query.deep, 8))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn empty_trash(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(library_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    ensure_library_exists(&state, &library_id).await?;
    state.enqueue_task(&TaskRecord::empty_trash(&library_id, 6)).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn ensure_library_exists(state: &AppState, library_id: &str) -> Result<(), ApiError> {
    state
        .storage
        .find_library_by_id(library_id)
        .await?
        .map(|_| ())
        .ok_or_else(|| ApiError::not_found("Library not found"))
}

fn apply_value<T>(target: &mut T, patch: Patch<T>) {
    if let Patch::Value(value) = patch {
        *target = value;
    }
}

fn non_blank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn validate_not_blank(field: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        Err(ApiError::bad_request(format!("{field} must not be blank")))
    } else {
        Ok(())
    }
}

fn validated_root(value: &str) -> Result<Url, ApiError> {
    validate_not_blank("root", value)?;
    let input = PathBuf::from(value);
    let path = if input.is_absolute() {
        input
    } else {
        std::env::current_dir()
            .map_err(|_| ApiError::bad_request("Library root folder does not exist"))?
            .join(input)
    };
    if !path.exists() {
        return Err(ApiError::bad_request(format!(
            "Library root folder does not exist: {}",
            path.display()
        )));
    }
    if !path.is_dir() {
        return Err(ApiError::bad_request(format!(
            "Library root folder is not a folder: {}",
            path.display()
        )));
    }
    Url::from_file_path(path).map_err(|_| ApiError::bad_request("Library root path is invalid"))
}

fn validate_library(library: &Library, existing: &[Library]) -> Result<(), ApiError> {
    let path = library
        .root_path()
        .ok_or_else(|| ApiError::bad_request("Library root path is invalid"))?;
    if existing.iter().any(|other| other.name == library.name) {
        return Err(ApiError::bad_request("Library name already exists"));
    }
    for other in existing {
        let Some(other_path) = other.root_path() else {
            continue;
        };
        if path.starts_with(&other_path) {
            return Err(ApiError::bad_request(format!(
                "Library path {} is a child of existing library {}: {}",
                path.display(),
                other.name,
                other_path.display()
            )));
        }
        if other_path.starts_with(&path) {
            return Err(ApiError::bad_request(format!(
                "Library path {} is a parent of existing library {}: {}",
                path.display(),
                other.name,
                other_path.display()
            )));
        }
    }
    Ok(())
}
