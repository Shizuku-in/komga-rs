//! Collection and read-list HTTP contracts.

use crate::{
    AppState,
    auth::AuthContext,
    content_access::{
        load_visible_book, load_visible_series, redact_book_source_for_user, redact_series_source_for_user,
    },
    dto::{CollectionPatchDto, ReadListPatchDto, preserving},
    error::ApiError,
    pagination::{SpringPage, parse_page_request},
    routes::{ApiJson, require_admin},
    search_query::{non_empty, search_hits},
};
use axum::{
    Json,
    extract::{Extension, Multipart, Path, Query, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use komga_core::{
    CollectionCreation, CollectionDto, ReadListCreation, ReadListDto, ReadListProgressUpdate, ReadListReadProgressDto,
    new_id,
};
use komga_search::EntityKind;
use komga_storage::ContainerThumbnail;

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
    let search = query_value(raw.as_deref(), "search");
    let result = if let Some(query) = non_empty(search.as_deref()) {
        let hits = search_hits(&state, EntityKind::Collection, query).await;
        state
            .storage
            .list_collections(None, library.as_deref(), Some(&hits), &page, &auth.user)
            .await?
    } else {
        state
            .storage
            .list_collections(None, library.as_deref(), None, &page, &auth.user)
            .await?
    };
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
    if !state.sync_collections(std::slice::from_ref(&id)).await {
        state.recover_search_after_http_failure().await;
    }
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
    if !state.sync_collections(std::slice::from_ref(&id)).await {
        state.recover_search_after_http_failure().await;
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
    if !state.delete_search_entities(vec![(EntityKind::Collection, id)]).await {
        state.recover_search_after_http_failure().await;
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
    result
        .content
        .iter_mut()
        .for_each(|series| redact_series_source_for_user(series, &auth.user));
    Ok(Json(SpringPage::from_content(result)))
}
pub(crate) async fn series_collections(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<CollectionDto>>, ApiError> {
    load_visible_series(&state, &auth.user, &id).await?;
    Ok(Json(state.storage.collections_for_series(&id, &auth.user).await?))
}

pub(crate) async fn list_read_lists(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    RawQuery(raw): RawQuery,
) -> Result<Json<SpringPage<ReadListDto>>, ApiError> {
    let page = parse_page_request(raw.as_deref())?;
    let search = query_value(raw.as_deref(), "search");
    let result = if let Some(query) = non_empty(search.as_deref()) {
        let hits = search_hits(&state, EntityKind::ReadList, query).await;
        state
            .storage
            .list_read_lists(None, Some(&hits), &page, &auth.user)
            .await?
    } else {
        state.storage.list_read_lists(None, None, &page, &auth.user).await?
    };
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
    if !state.sync_read_lists(std::slice::from_ref(&id)).await {
        state.recover_search_after_http_failure().await;
    }
    Ok(Json(state.storage.find_read_list(&id, &auth.user).await?.ok_or_else(
        || ApiError::internal("Created read list could not be loaded"),
    )?))
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
    if !state.sync_read_lists(std::slice::from_ref(&id)).await {
        state.recover_search_after_http_failure().await;
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
    if !state.delete_search_entities(vec![(EntityKind::ReadList, id)]).await {
        state.recover_search_after_http_failure().await;
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
    result
        .content
        .iter_mut()
        .for_each(|book| redact_book_source_for_user(book, &auth.user));
    Ok(Json(SpringPage::from_content(result)))
}
pub(crate) async fn book_read_lists(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ReadListDto>>, ApiError> {
    load_visible_book(&state, &auth.user, &id).await?;
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
    redact_book_source_for_user(&mut result, &auth.user);
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
