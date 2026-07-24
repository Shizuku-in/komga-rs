//! Collection and read-list HTTP contracts.

use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

use crate::{
    AppState,
    auth::AuthContext,
    content_routes::{SpringPage, parse_page_request, restrict_book_url},
    dto::{CollectionPatchDto, ReadListPatchDto, preserving},
    error::ApiError,
    routes::{ApiJson, require_admin},
};
use axum::{
    Json,
    body::Body,
    extract::{Extension, Multipart, Path, Query, RawQuery, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use komga_core::{
    CollectionCreation, CollectionDto, ReadListCreation, ReadListDto, ReadListProgressUpdate, ReadListReadProgressDto,
    UserRole, new_id,
};
use komga_storage::ContainerThumbnail;
use tokio_util::io::{ReaderStream, SyncIoBridge};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::book_file_routes::content_disposition;

const ZIP_BUFFER_CAPACITY: usize = 8 * 1024;

fn query_value(raw: Option<&str>, key: &str) -> Option<String> {
    url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .find_map(|(k, v)| (k == key).then(|| v.into_owned()))
}

pub(crate) async fn list_collections(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<CollectionDto>>, ApiError> {
    let page = parse_page_request(raw.as_deref())?;
    let library = query_value(raw.as_deref(), "library_id").or_else(|| query_value(raw.as_deref(), "libraryId"));
    let result = state
        .storage
        .list_collections(
            query_value(raw.as_deref(), "search").as_deref(),
            library.as_deref(),
            &page,
            &auth.user,
        )
        .await?;
    Ok(Json(SpringPage::from_content(result)))
}
pub(crate) async fn create_collection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(value): ApiJson<CollectionCreation>,
) -> Result<Json<CollectionDto>, ApiError> {
    require_admin(&auth)?;
    let id = new_id();
    state.storage.create_collection(&id, &value).await?;
    Ok(Json(state.storage.find_collection(&id, &auth.user).await?.ok_or_else(
        || ApiError::internal("Created collection could not be loaded"),
    )?))
}
pub(crate) async fn get_collection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<CollectionDto>, ApiError> {
    Ok(Json(
        state
            .storage
            .find_collection(&id, &auth.user)
            .await?
            .ok_or_else(|| ApiError::not_found("Collection not found"))?,
    ))
}
pub(crate) async fn patch_collection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    ApiJson(p): ApiJson<CollectionPatchDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if !state
        .storage
        .update_collection(&id, preserving(p.name), preserving(p.ordered), preserving(p.series_ids))
        .await?
    {
        return Err(ApiError::not_found("Collection not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn delete_collection(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if !state.storage.delete_collection(&id).await? {
        return Err(ApiError::not_found("Collection not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn collection_series(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<komga_core::SeriesDto>>, ApiError> {
    state
        .storage
        .find_collection(&id, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Collection not found"))?;
    let mut result = state
        .storage
        .collection_series(&id, &auth.user, parse_page_request(raw.as_deref())?)
        .await?;
    if !auth.user.is_admin() {
        result.content.iter_mut().for_each(|s| s.url.clear())
    }
    Ok(Json(SpringPage::from_content(result)))
}
pub(crate) async fn series_collections(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<CollectionDto>>, ApiError> {
    crate::content_routes::load_visible_series(&state, &auth.user, &id).await?;
    Ok(Json(state.storage.collections_for_series(&id, &auth.user).await?))
}

pub(crate) async fn list_read_lists(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<ReadListDto>>, ApiError> {
    let page = parse_page_request(raw.as_deref())?;
    let result = state
        .storage
        .list_read_lists(query_value(raw.as_deref(), "search").as_deref(), &page, &auth.user)
        .await?;
    Ok(Json(SpringPage::from_content(result)))
}
pub(crate) async fn create_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(value): ApiJson<ReadListCreation>,
) -> Result<Json<ReadListDto>, ApiError> {
    require_admin(&auth)?;
    let id = new_id();
    state.storage.create_read_list(&id, &value).await?;
    Ok(Json(state.storage.find_read_list(&id, &auth.user).await?.ok_or_else(
        || ApiError::internal("Created read list could not be loaded"),
    )?))
}

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

pub(crate) async fn download_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if !auth.user.roles.contains(&UserRole::FileDownload) {
        return Err(ApiError::forbidden());
    }
    let read_list = state
        .storage
        .find_read_list(&id, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Read list not found"))?;
    let books = state
        .storage
        .read_list_books(
            &id,
            &auth.user,
            komga_core::PageRequest {
                unpaged: true,
                sort: vec![komga_core::SortOrder {
                    property: "readList.number".into(),
                    direction: komga_core::SortDirection::Ascending,
                }],
                ..Default::default()
            },
        )
        .await?
        .content;
    if books.is_empty() {
        return Err(ApiError::not_found("Read list not found"));
    }
    let mut entries = Vec::with_capacity(books.len());
    for (index, visible) in books.into_iter().enumerate() {
        let Some(book) = state.storage.find_book_by_id(&visible.id).await? else {
            tracing::warn!(book_id = %visible.id, "visible read-list relation no longer resolves; skipping ZIP entry");
            continue;
        };
        match book.url.to_file_path() {
            Ok(path) => entries.push((index + 1, book.id, path)),
            Err(()) => {
                tracing::warn!(book_id = %book.id, url = %book.url, "book URL is not a local file; skipping ZIP entry");
            }
        }
    }
    let (writer, reader) = tokio::io::duplex(64 * 1024);
    tokio::task::spawn_blocking(move || write_read_list_zip(writer, entries));
    let mut response = Body::from_stream(ReaderStream::with_capacity(reader, ZIP_BUFFER_CAPACITY)).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&format!("{}.zip", read_list.name))
            .map_err(|_| ApiError::internal("Could not encode read-list attachment filename"))?,
    );
    Ok(response)
}

fn write_read_list_zip(writer: tokio::io::DuplexStream, entries: Vec<(usize, String, PathBuf)>) {
    let bridge = SyncIoBridge::new(writer);
    let mut archive = ZipWriter::new_stream(bridge);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .large_file(true);
    let mut buffer = [0_u8; ZIP_BUFFER_CAPACITY];
    for (number, book_id, path) in entries {
        let Ok(metadata) = path.metadata() else {
            tracing::warn!(%book_id, path = %path.display(), "book file is missing; skipping ZIP entry");
            continue;
        };
        if !metadata.is_file() {
            tracing::warn!(%book_id, path = %path.display(), "book path is not a regular file; skipping ZIP entry");
            continue;
        }
        let Some(file_name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else {
            tracing::warn!(%book_id, path = %path.display(), "book path has no filename; skipping ZIP entry");
            continue;
        };
        let Ok(mut file) = File::open(&path) else {
            tracing::warn!(%book_id, path = %path.display(), "book file could not be opened; skipping ZIP entry");
            continue;
        };
        if archive.start_file(format!("{number} - {file_name}"), options).is_err() {
            return;
        }
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) if archive.write_all(&buffer[..count]).is_ok() => {}
                Ok(_) | Err(_) => return,
            }
        }
    }
    let _ = archive.finish();
}

pub(crate) async fn get_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<ReadListDto>, ApiError> {
    Ok(Json(
        state
            .storage
            .find_read_list(&id, &auth.user)
            .await?
            .ok_or_else(|| ApiError::not_found("Read list not found"))?,
    ))
}
pub(crate) async fn patch_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    ApiJson(p): ApiJson<ReadListPatchDto>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if !state
        .storage
        .update_read_list(
            &id,
            preserving(p.name),
            preserving(p.summary),
            preserving(p.ordered),
            preserving(p.book_ids),
        )
        .await?
    {
        return Err(ApiError::not_found("Read list not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn delete_read_list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    if !state.storage.delete_read_list(&id).await? {
        return Err(ApiError::not_found("Read list not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
pub(crate) async fn read_list_books(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<komga_core::BookDto>>, ApiError> {
    state
        .storage
        .find_read_list(&id, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Read list not found"))?;
    let mut result = state
        .storage
        .read_list_books(&id, &auth.user, parse_page_request(raw.as_deref())?)
        .await?;
    if !auth.user.is_admin() {
        result.content.iter_mut().for_each(restrict_book_url)
    }
    Ok(Json(SpringPage::from_content(result)))
}
pub(crate) async fn book_read_lists(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ReadListDto>>, ApiError> {
    crate::content_routes::load_visible_book(&state, &auth.user, &id).await?;
    Ok(Json(state.storage.read_lists_for_book(&id, &auth.user).await?))
}
async fn sibling(
    state: &AppState,
    auth: &AuthContext,
    list: &str,
    book: &str,
    next: bool,
) -> Result<Json<komga_core::BookDto>, ApiError> {
    state
        .storage
        .find_read_list(list, &auth.user)
        .await?
        .ok_or_else(|| ApiError::not_found("Read list not found"))?;
    let mut result = state
        .storage
        .read_list_sibling(list, book, &auth.user, next)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    if !auth.user.is_admin() {
        restrict_book_url(&mut result)
    }
    Ok(Json(result))
}
pub(crate) async fn previous(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((list, book)): Path<(String, String)>,
) -> Result<Json<komga_core::BookDto>, ApiError> {
    sibling(&state, &auth, &list, &book, false).await
}
pub(crate) async fn next(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((list, book)): Path<(String, String)>,
) -> Result<Json<komga_core::BookDto>, ApiError> {
    sibling(&state, &auth, &list, &book, true).await
}
pub(crate) async fn get_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<ReadListReadProgressDto>, ApiError> {
    Ok(Json(
        state
            .storage
            .read_list_progress(&id, &auth.user)
            .await?
            .ok_or_else(|| ApiError::not_found("Read list not found"))?,
    ))
}
pub(crate) async fn put_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    ApiJson(value): ApiJson<ReadListProgressUpdate>,
) -> Result<StatusCode, ApiError> {
    if !state
        .storage
        .complete_read_list(&id, &auth.user, value.last_book_read)
        .await?
    {
        return Err(ApiError::not_found("Read list not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn ensure_parent(state: &AppState, auth: &AuthContext, id: &str, read_list: bool) -> Result<(), ApiError> {
    let found = if read_list {
        state.storage.find_read_list(id, &auth.user).await?.is_some()
    } else {
        state.storage.find_collection(id, &auth.user).await?.is_some()
    };
    if found {
        Ok(())
    } else {
        Err(ApiError::not_found("Container not found"))
    }
}
fn thumbnail_json(t: ContainerThumbnail, read_list: bool) -> serde_json::Value {
    if read_list {
        serde_json::json!({"id":t.id,"readListId":t.parent_id,"type":t.thumbnail_type,"selected":t.selected,"mediaType":t.media_type,"fileSize":t.file_size,"width":t.width,"height":t.height})
    } else {
        serde_json::json!({"id":t.id,"collectionId":t.parent_id,"type":t.thumbnail_type,"selected":t.selected,"mediaType":t.media_type,"fileSize":t.file_size,"width":t.width,"height":t.height})
    }
}
async fn list_thumbnails(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
    read_list: bool,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    ensure_parent(state, auth, id, read_list).await?;
    Ok(Json(
        state
            .storage
            .list_container_thumbnails(read_list, id)
            .await?
            .into_iter()
            .map(|t| thumbnail_json(t, read_list))
            .collect(),
    ))
}
pub(crate) async fn list_collection_thumbnails(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    list_thumbnails(&s, &a, &id, false).await
}
pub(crate) async fn list_read_list_thumbnails(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<serde_json::Value>>, ApiError> {
    list_thumbnails(&s, &a, &id, true).await
}
async fn upload(
    state: AppState,
    auth: AuthContext,
    id: String,
    query: crate::thumbnail_routes::UploadQuery,
    multipart: Multipart,
    read_list: bool,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&auth)?;
    ensure_parent(&state, &auth, &id, read_list).await?;
    let upload = crate::thumbnail_routes::parse_upload(multipart, query.selected).await?;
    let value = ContainerThumbnail {
        id: komga_core::new_id(),
        thumbnail: Some(upload.bytes),
        url: None,
        selected: false,
        thumbnail_type: "USER_UPLOADED".into(),
        media_type: upload.media_type,
        file_size: upload.file_size,
        width: upload.dimension.width,
        height: upload.dimension.height,
        parent_id: id,
    };
    let saved = state
        .storage
        .insert_container_thumbnail(read_list, &value, upload.selected)
        .await?;
    Ok(Json(thumbnail_json(saved, read_list)))
}
pub(crate) async fn upload_collection_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(q): Query<crate::thumbnail_routes::UploadQuery>,
    m: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    upload(s, a, id, q, m, false).await
}
pub(crate) async fn upload_read_list_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(q): Query<crate::thumbnail_routes::UploadQuery>,
    m: Multipart,
) -> Result<Json<serde_json::Value>, ApiError> {
    upload(s, a, id, q, m, true).await
}
async fn image(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
    thumbnail_id: Option<&str>,
    read_list: bool,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    ensure_parent(state, auth, id, read_list).await?;
    let stored = match thumbnail_id {
        Some(t) => state.storage.find_container_thumbnail(read_list, id, t).await?,
        None => state.storage.selected_container_thumbnail(read_list, id).await?,
    };
    if let Some(t) = stored {
        return crate::thumbnail_routes::thumbnail_response(
            crate::thumbnail_routes::read_thumbnail(t.thumbnail, t.url).await?,
            headers,
        );
    }
    if thumbnail_id.is_some() {
        return Err(ApiError::not_found("Thumbnail not found"));
    }
    let mut sources = Vec::new();
    if read_list {
        let books = state
            .storage
            .read_list_books(
                id,
                &auth.user,
                komga_core::PageRequest {
                    unpaged: true,
                    ..Default::default()
                },
            )
            .await?
            .content;
        for book in books.into_iter().take(4) {
            if let Ok(bytes) = crate::thumbnail_routes::selected_book_thumbnail_bytes(state, &book.id).await {
                sources.push(bytes)
            }
        }
    } else {
        let series = state
            .storage
            .collection_series(
                id,
                &auth.user,
                komga_core::PageRequest {
                    unpaged: true,
                    ..Default::default()
                },
            )
            .await?
            .content;
        for item in series.into_iter().take(4) {
            if let Some(book) = state
                .storage
                .find_series_cover_book_id(&item.id, &auth.user.id, komga_core::SeriesCover::First)
                .await?
                && let Ok(bytes) = crate::thumbnail_routes::selected_book_thumbnail_bytes(state, &book).await
            {
                sources.push(bytes)
            }
        }
    }
    let bytes = komga_media::collage_thumbnail(&sources, state.storage.thumbnail_max_edge().await?)
        .map_err(|_| ApiError::internal("Could not generate thumbnail"))?;
    crate::thumbnail_routes::thumbnail_response(bytes, headers)
}
pub(crate) async fn get_collection_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    image(&s, &a, &id, None, false, &h).await
}
pub(crate) async fn get_read_list_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    image(&s, &a, &id, None, true, &h).await
}
pub(crate) async fn get_collection_thumbnail_id(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path((id, t)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    image(&s, &a, &id, Some(&t), false, &h).await
}
pub(crate) async fn get_read_list_thumbnail_id(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path((id, t)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Response, ApiError> {
    image(&s, &a, &id, Some(&t), true, &h).await
}
async fn select_or_delete(
    state: &AppState,
    auth: &AuthContext,
    id: &str,
    t: &str,
    read_list: bool,
    delete: bool,
) -> Result<StatusCode, ApiError> {
    require_admin(auth)?;
    ensure_parent(state, auth, id, read_list).await?;
    let changed = if delete {
        state.storage.delete_container_thumbnail(read_list, id, t).await?
    } else {
        state.storage.select_container_thumbnail(read_list, id, t).await?
    };
    if !changed {
        return Err(ApiError::not_found("Thumbnail not found"));
    }
    Ok(StatusCode::ACCEPTED)
}
pub(crate) async fn select_collection_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path((id, t)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    select_or_delete(&s, &a, &id, &t, false, false).await
}
pub(crate) async fn select_read_list_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path((id, t)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    select_or_delete(&s, &a, &id, &t, true, false).await
}
pub(crate) async fn delete_collection_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path((id, t)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    select_or_delete(&s, &a, &id, &t, false, true).await
}
pub(crate) async fn delete_read_list_thumbnail(
    State(s): State<AppState>,
    Extension(a): Extension<AuthContext>,
    Path((id, t)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    select_or_delete(&s, &a, &id, &t, true, true).await
}
