use std::{future::poll_fn, pin::Pin};

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use futures_core::Stream;
use komga_core::{
    DomainEvent, EPUB_MEDIA_TYPE, MediaExtension, MediaStatus, R2Device, R2Location, R2Locator, R2Progression,
    ReadProgressEvent, SeriesDto, User, UserRole, progression,
};
use rand::distr::{Alphanumeric, SampleString};
use serde::Deserialize;
use tokio_util::io::ReaderStream;

use crate::{
    AppState, book_file_routes, content_access::load_visible_book, error::ApiError, thumbnail_routes,
    webpub_routes::ManifestUrls,
};

use super::{
    auth::KoboAuthContext,
    dto::{
        AuthDto, ReadingStateUpdateDto, ReadingStateUpdateResultDto, RequestResultDto, ResourcesDto, ResultDto,
        StatusDto, WrappedResultDto, metadata as metadata_dto, reading_state, should_convert_kepub,
    },
    proxy::{ProxyError, error_response},
};

const FALLBACK_RESOURCES: &str = include_str!("resources.json");

pub(super) async fn ping() -> Json<&'static str> {
    Json("pong")
}

pub(super) async fn initialization(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    let mut resources = if state.kobo_proxy.is_enabled() {
        match state.kobo_proxy.forward(&Method::GET, &uri, &headers, None, None).await {
            Ok(response) => response
                .json_value()
                .ok()
                .and_then(|value| value.get("Resources").cloned())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(fallback_resources),
            Err(ProxyError::Upstream(StatusCode::UNAUTHORIZED)) => {
                return Ok(error_response(&ProxyError::Upstream(StatusCode::UNAUTHORIZED)));
            }
            Err(error) => {
                tracing::error!(%error, "Kobo initialization proxy failed; using local resources");
                fallback_resources()
            }
        }
    } else {
        fallback_resources()
    };
    let base = request_base(&headers, &uri)?;
    let base = base.trim_end_matches('/');
    let image_root = format!("{base}/kobo/{}/v1/books", auth.auth_token);
    resources["image_host"] = serde_json::Value::String(base.to_owned());
    resources["image_url_template"] = serde_json::Value::String(format!(
        "{image_root}/{{ImageId}}/thumbnail/{{Width}}/{{Height}}/false/image.jpg"
    ));
    resources["image_url_quality_template"] = serde_json::Value::String(format!(
        "{image_root}/{{ImageId}}/thumbnail/{{Width}}/{{Height}}/{{Quality}}/{{IsGreyscale}}/image.jpg"
    ));
    let mut response = Json(ResourcesDto { resources }).into_response();
    response
        .headers_mut()
        .insert("x-kobo-apitoken", HeaderValue::from_static("e30="));
    Ok(response)
}

pub(super) async fn auth_device(State(state): State<AppState>, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
    if state.kobo_proxy.is_enabled() {
        match state
            .kobo_proxy
            .forward(&Method::POST, &uri, &headers, Some(body.clone()), None)
            .await
        {
            Ok(response) if response.json_value().is_ok() => return response.into_response(),
            Ok(_) => tracing::error!("Kobo device authentication proxy returned invalid JSON; using local response"),
            Err(error) => tracing::error!(%error, "Kobo device authentication proxy failed; using local response"),
        }
    }
    let user_key = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("UserKey")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let mut rng = rand::rng();
    Json(AuthDto {
        access_token: Alphanumeric.sample_string(&mut rng, 24),
        refresh_token: Alphanumeric.sample_string(&mut rng, 24),
        token_type: "Bearer",
        tracking_id: uuid::Uuid::new_v4().to_string(),
        user_key,
    })
    .into_response()
}

pub(super) async fn metadata(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    if state.storage.find_book_by_id(&book_id).await?.is_none() && state.kobo_proxy.is_enabled() {
        return Ok(proxy_current(&state, &Method::GET, &uri, &headers, None).await);
    }
    load_visible_book(&state, &auth.user, &book_id).await?;
    let projection = state
        .storage
        .find_sync_book_projection(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let thumbnail_id = state
        .storage
        .find_selected_book_thumbnail(&book_id)
        .await?
        .map(|thumbnail| thumbnail.id);
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let conversion_available = state.kobo_kepub.is_available();
    let convert_kepub = should_convert_kepub(&projection, conversion_available);
    Ok(Json(vec![metadata_dto(
        &projection,
        thumbnail_id,
        book.file_size,
        download_url(&headers, &uri, &auth.auth_token, &book_id, convert_kepub)?,
        conversion_available,
    )])
    .into_response())
}

pub(super) async fn get_state(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response, ApiError> {
    if state.storage.find_book_by_id(&book_id).await?.is_none() && state.kobo_proxy.is_enabled() {
        return Ok(proxy_current(&state, &Method::GET, &uri, &headers, None).await);
    }
    load_visible_book(&state, &auth.user, &book_id).await?;
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let created = book.created_date.unwrap_or_else(|| Utc::now().naive_utc());
    let progress = state.storage.find_sync_read_progress(&book_id, &auth.user.id).await?;
    Ok(Json(vec![reading_state(progress.as_ref(), &book_id, created)]).into_response())
}

pub(super) async fn put_state(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response, ApiError> {
    if state.storage.find_book_by_id(&book_id).await?.is_none() && state.kobo_proxy.is_enabled() {
        return Ok(proxy_current(&state, &Method::PUT, &uri, &headers, Some(body)).await);
    }
    let mut update = serde_json::from_slice::<ReadingStateUpdateDto>(&body)
        .map_err(|_| ApiError::bad_request("Invalid reading state request"))?;
    load_visible_book(&state, &auth.user, &book_id).await?;
    let Some(update) = update.reading_states.drain(..).next() else {
        return Err(ApiError::bad_request("ReadingStates must not be empty"));
    };
    if update.current_bookmark.location.is_none() || update.current_bookmark.content_source_progress_percent.is_none() {
        return Err(ApiError::bad_request(
            "CurrentBookmark.Location and ContentSourceProgressPercent are required",
        ));
    }
    let result = save_progress(&state, &auth, &book_id, update).await;
    if let Err(error) = &result {
        tracing::error!(%error, %book_id, "could not update Kobo progression");
    }
    let (request_result, bookmark, statistics, status) = if result.is_ok() {
        (
            ResultDto::Success,
            ResultDto::Success,
            ResultDto::Ignored,
            ResultDto::Success,
        )
    } else {
        (
            ResultDto::Failure,
            ResultDto::Failure,
            ResultDto::Failure,
            ResultDto::Failure,
        )
    };
    Ok(Json(RequestResultDto {
        request_result,
        update_results: vec![ReadingStateUpdateResultDto {
            entitlement_id: book_id,
            current_bookmark_result: WrappedResultDto { result: bookmark },
            statistics_result: WrappedResultDto { result: statistics },
            status_info_result: WrappedResultDto { result: status },
        }],
    })
    .into_response())
}

async fn save_progress(
    state: &AppState,
    auth: &KoboAuthContext,
    book_id: &str,
    update: super::dto::ReadingStateDto,
) -> Result<(), String> {
    let media = state
        .storage
        .find_media_by_book_id(book_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Book analysis is missing".to_owned())?;
    let location = update
        .current_bookmark
        .location
        .as_ref()
        .ok_or_else(|| "Bookmark location is required".to_owned())?;
    let source_progress = update
        .current_bookmark
        .content_source_progress_percent
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .ok_or_else(|| "ContentSourceProgressPercent must be between 0 and 100".to_owned())?;
    let total_progress = update.current_bookmark.progress_percent;
    if total_progress.is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value)) {
        return Err("ProgressPercent must be between 0 and 100".into());
    }
    let locator = if update.status_info.status == StatusDto::Finished {
        match media.extension.as_ref() {
            Some(MediaExtension::Epub(extension)) => extension
                .positions
                .last()
                .cloned()
                .ok_or_else(|| "EPUB has no reading positions".to_owned())?,
            None => return Err("EPUB extension is missing".into()),
        }
    } else {
        R2Locator {
            href: location.source.clone(),
            media_type: "application/xhtml+xml".into(),
            title: None,
            locations: Some(R2Location {
                progression: Some(source_progress / 100.0),
                total_progression: total_progress.map(|value| value / 100.0),
                ..R2Location::default()
            }),
            text: None,
            kobo_span: location
                .r#type
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("kobospan"))
                .then(|| location.value.clone())
                .flatten(),
        }
    };
    let saved = state
        .storage
        .find_read_progress(book_id, &auth.user.id)
        .await
        .map_err(|error| error.to_string())?;
    let progress = progression(
        book_id,
        &auth.user.id,
        &media,
        saved.as_ref(),
        R2Progression {
            modified: update.last_modified,
            device: R2Device {
                id: auth.api_key_id.clone(),
                name: auth.api_key_comment.clone(),
            },
            locator,
        },
    )
    .map_err(|error| error.to_string())?;
    state
        .storage
        .save_read_progress(&progress)
        .await
        .map_err(|error| error.to_string())?;
    state.publish_domain_event(DomainEvent::ReadProgressChanged(ReadProgressEvent {
        book_id: book_id.to_owned(),
        user_id: auth.user.id.clone(),
    }));
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct BookFileQuery {
    #[serde(default)]
    convert_kepub: bool,
}

pub(super) async fn book_file(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, book_id)): Path<(String, String)>,
    Query(query): Query<BookFileQuery>,
) -> Result<Response, ApiError> {
    if !auth.user.roles.contains(&UserRole::FileDownload) {
        return Err(ApiError::forbidden());
    }
    load_visible_book(&state, &auth.user, &book_id).await?;
    if !query.convert_kepub {
        return book_file_routes::download_for_user(&state, &auth.user, &book_id).await;
    }
    let book = state
        .storage
        .find_book_by_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let media = state
        .storage
        .find_media_by_book_id(&book_id)
        .await?
        .ok_or_else(|| ApiError::service_unavailable("Kepub conversion failed"))?;
    let is_fixed_layout = matches!(
        media.extension.as_ref(),
        Some(MediaExtension::Epub(extension)) if extension.is_fixed_layout
    );
    if media.status != MediaStatus::Ready
        || media.media_type.as_deref() != Some(EPUB_MEDIA_TYPE)
        || media.epub_is_kepub
        || is_fixed_layout
    {
        return Err(ApiError::service_unavailable("Kepub conversion failed"));
    }
    let converted = state
        .kobo_kepub
        .get_or_convert(&book, &state.shutdown)
        .await
        .map_err(|error| {
            tracing::error!(%error, %book_id, "could not convert Kobo book to KEPUB");
            ApiError::service_unavailable("Kepub conversion failed")
        })?;
    let file = tokio::fs::File::open(&converted.path).await.map_err(|error| {
        tracing::error!(%error, %book_id, path = %converted.path.display(), "could not open cached KEPUB");
        ApiError::service_unavailable("Kepub conversion failed")
    })?;
    let file_name = book
        .url
        .to_file_path()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|stem| format!("{}.kepub.epub", stem.to_string_lossy()))
        })
        .unwrap_or_else(|| format!("{}.kepub.epub", book.name));
    let length = converted.length;
    let mut reader = ReaderStream::with_capacity(file, 8 * 1024);
    let stream = async_stream::stream! {
        let _keepalive = converted;
        while let Some(chunk) = poll_fn(|context| Pin::new(&mut reader).poll_next(context)).await {
            yield chunk;
        }
    };
    let mut response = Body::from_stream(stream).into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(EPUB_MEDIA_TYPE));
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string())
            .map_err(|_| ApiError::internal("Could not encode KEPUB file length"))?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        book_file_routes::content_disposition(&file_name)
            .map_err(|_| ApiError::internal("Could not encode KEPUB attachment filename"))?,
    );
    Ok(response)
}

pub(super) async fn cover(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, thumbnail_id, width, height, _greyscale)): Path<(String, String, String, String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cover_impl(&state, &auth.user, &thumbnail_id, &width, &height, &headers).await
}

pub(super) async fn cover_with_quality(
    State(state): State<AppState>,
    Extension(auth): Extension<KoboAuthContext>,
    Path((_auth_token, thumbnail_id, width, height, _quality, _greyscale)): Path<(
        String,
        String,
        String,
        String,
        String,
        String,
    )>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cover_impl(&state, &auth.user, &thumbnail_id, &width, &height, &headers).await
}

async fn cover_impl(
    state: &AppState,
    user: &User,
    thumbnail_id: &str,
    width: &str,
    height: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let thumbnail = state.storage.find_book_thumbnail_by_id(thumbnail_id).await?;
    let Some(thumbnail) = thumbnail else {
        if state.kobo_proxy.is_enabled() {
            let mut url = url::Url::parse("https://cdn.kobo.com/book-images/")
                .map_err(|_| ApiError::internal("Could not build Kobo cover URL"))?;
            {
                let mut segments = url
                    .path_segments_mut()
                    .map_err(|()| ApiError::internal("Could not build Kobo cover URL"))?;
                segments.pop_if_empty();
                segments.extend([thumbnail_id, width, height, "false", "image.jpg"]);
            }
            return Ok(Redirect::temporary(url.as_str()).into_response());
        }
        return Err(ApiError::not_found("Book thumbnail not found"));
    };
    ensure_cover_visible(state, user, &thumbnail.book_id).await?;
    let mut bytes = thumbnail_routes::read_thumbnail(thumbnail.thumbnail, thumbnail.url).await?;
    if thumbnail.media_type != "image/jpeg" {
        bytes = komga_media::convert_image(&bytes, komga_media::ImageOutput::Jpeg, None)
            .map_err(|error| {
                tracing::error!(%error, %thumbnail_id, "could not convert Kobo cover to JPEG");
                ApiError::internal("Could not convert book thumbnail")
            })?
            .bytes;
    }
    thumbnail_routes::thumbnail_response(bytes, headers)
}

async fn ensure_cover_visible(state: &AppState, user: &User, book_id: &str) -> Result<(), ApiError> {
    let book = state
        .storage
        .find_book_dto_by_id(book_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let series = state
        .storage
        .find_series_dto_by_id(&book.series_id, &user.id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    if !series_visible(user, &series) {
        return Err(ApiError::not_found("Book not found"));
    }
    Ok(())
}

fn series_visible(user: &User, series: &SeriesDto) -> bool {
    let age = series.metadata.age_rating.and_then(|value| u32::try_from(value).ok());
    user.can_access_library(&series.library_id) && user.is_content_allowed(age, &series.metadata.sharing_labels)
}

pub(super) async fn catch_all(
    State(state): State<AppState>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    if !state.kobo_proxy.is_enabled() {
        return Json(serde_json::json!({})).into_response();
    }
    proxy_current(&state, &method, &uri, &headers, (!body.is_empty()).then_some(body)).await
}

fn fallback_resources() -> serde_json::Value {
    serde_json::from_str(FALLBACK_RESOURCES).expect("embedded Kobo Resources JSON is valid")
}

async fn proxy_current(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Option<Bytes>,
) -> Response {
    match state.kobo_proxy.forward(method, uri, headers, body, None).await {
        Ok(response) => response.into_response(),
        Err(error) => {
            tracing::error!(%error, "Kobo Store proxy request failed");
            error_response(&error)
        }
    }
}

pub(super) fn download_url(
    headers: &HeaderMap,
    uri: &Uri,
    auth_token: &str,
    book_id: &str,
    convert_kepub: bool,
) -> Result<String, ApiError> {
    Ok(format!(
        "{}?convert_kepub={convert_kepub}",
        ManifestUrls::from_request_with_prefix(headers, uri, &[])?
            .absolute_path(&["kobo", auth_token, "v1", "books", book_id, "file", "epub",])
    ))
}

fn request_base(headers: &HeaderMap, uri: &Uri) -> Result<String, ApiError> {
    Ok(ManifestUrls::from_request_with_prefix(headers, uri, &[])?.absolute_path(&[]))
}

#[cfg(test)]
mod tests {
    use axum::{
        Json, Router,
        extract::Request,
        routing::{any, get, post},
    };
    use http_body_util::BodyExt;
    use komga_core::{User, UserRole};
    use komga_storage::{Storage, TaskStorage};
    use serde_json::json;
    use sha2::{Digest, Sha512};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::*;
    use crate::{build_app, kobo::proxy::KoboProxy};

    async fn official(request: Request) -> Response {
        let path = request.uri().path().to_owned();
        if path == "/v1/initialization" {
            return Json(json!({"Resources": {"official_resource": "yes", "image_host": "upstream"}})).into_response();
        }
        if path == "/v1/auth/device" {
            return Json(json!({"AccessToken": "official-access", "TokenType": "Bearer"})).into_response();
        }
        if path.ends_with("/metadata") {
            return Json(json!([{"EntitlementId": "store-book"}])).into_response();
        }
        let method = request.method().to_string();
        let authorization = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let cookie = request.headers().contains_key(header::COOKIE);
        let sync_token = request.headers().contains_key("x-kobo-synctoken");
        let body = axum::body::to_bytes(request.into_body(), usize::MAX).await.unwrap();
        Json(json!({
            "Method": method,
            "Path": path,
            "Authorization": authorization,
            "CookieForwarded": cookie,
            "SyncTokenForwarded": sync_token,
            "Body": String::from_utf8_lossy(&body),
        }))
        .into_response()
    }

    async fn proxy_app() -> (Router, tokio::task::JoinHandle<()>, String) {
        proxy_app_with(Router::new().fallback(any(official))).await
    }

    async fn proxy_app_with(upstream_app: Router) -> (Router, tokio::task::JoinHandle<()>, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            axum::serve(listener, upstream_app).await.unwrap();
        });
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let mut user = User::new("proxy@example.org".into(), "unused".into());
        user.roles.insert(UserRole::KoboSync);
        storage.create_user(&user).await.unwrap();
        let key = "local-proxy-key";
        storage
            .create_api_key(
                "proxy-key-id",
                &user.id,
                &hex::encode(Sha512::digest(key.as_bytes())),
                "Kobo",
            )
            .await
            .unwrap();
        let mut state = AppState::new(storage, tasks).await.unwrap();
        state.kobo_proxy = KoboProxy::with_base_url(true, &base_url);
        (build_app(state), upstream, key.into())
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn proxy_routes_merge_resources_and_forward_only_missing_store_content() {
        let (app, upstream, key) = proxy_app().await;
        let initialization = app
            .clone()
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/initialization"))
                    .header(header::HOST, "reader.example:1234")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initialization.status(), StatusCode::OK);
        assert_eq!(initialization.headers()["x-kobo-apitoken"], "e30=");
        let initialization = response_json(initialization).await;
        assert_eq!(initialization["Resources"]["official_resource"], "yes");
        assert_eq!(initialization["Resources"]["image_host"], "http://reader.example:1234");

        let auth = app
            .clone()
            .oneshot(
                Request::post(format!("/kobo/{key}/v1/auth/device"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response_json(auth).await["AccessToken"], "official-access");

        let metadata = app
            .clone()
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/library/store-book/metadata"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response_json(metadata).await[0]["EntitlementId"], "store-book");

        let state = app
            .clone()
            .oneshot(
                Request::put(format!("/kobo/{key}/v1/library/store-book/state"))
                    .header(header::AUTHORIZATION, "Bearer official")
                    .header(header::COOKIE, "private=true")
                    .header("x-kobo-synctoken", "wrapped.local")
                    .body(Body::from("official body"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let state = response_json(state).await;
        assert_eq!(state["Method"], "PUT");
        assert_eq!(state["Authorization"], "Bearer official");
        assert_eq!(state["CookieForwarded"], false);
        assert_eq!(state["SyncTokenForwarded"], false);
        assert_eq!(state["Body"], "official body");

        let catch_all = app
            .clone()
            .oneshot(
                Request::patch(format!("/kobo/{key}/v1/wishlist?sort=new"))
                    .body(Body::from("patch"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let catch_all = response_json(catch_all).await;
        assert_eq!(catch_all["Method"], "PATCH");
        assert_eq!(catch_all["Path"], "/v1/wishlist");

        let cover = app
            .oneshot(
                Request::get(format!(
                    "/kobo/{key}/v1/books/store-cover/thumbnail/300/400/false/image.jpg"
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cover.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            cover.headers()[header::LOCATION],
            "https://cdn.kobo.com/book-images/store-cover/300/400/false/image.jpg"
        );
        upstream.abort();
    }

    #[tokio::test]
    async fn initialization_preserves_unauthorized_but_other_proxy_failures_fall_back() {
        let unauthorized = Router::new().route("/v1/initialization", get(|| async { StatusCode::UNAUTHORIZED }));
        let (app, upstream, key) = proxy_app_with(unauthorized).await;
        let response = app
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/initialization"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        upstream.abort();

        let failing = Router::new()
            .route(
                "/v1/initialization",
                get(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
            )
            .route("/v1/auth/device", post(|| async { StatusCode::INTERNAL_SERVER_ERROR }));
        let (app, upstream, key) = proxy_app_with(failing).await;
        let initialization = app
            .clone()
            .oneshot(
                Request::get(format!("/kobo/{key}/v1/initialization"))
                    .header(header::HOST, "reader.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initialization.status(), StatusCode::OK);
        assert_eq!(
            response_json(initialization).await["Resources"]["account_page"],
            "https://www.kobo.com/account/settings"
        );

        let auth = app
            .oneshot(
                Request::post(format!("/kobo/{key}/v1/auth/device"))
                    .body(Body::from(r#"{"UserKey":"local-user"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(auth.status(), StatusCode::OK);
        assert_eq!(response_json(auth).await["UserKey"], "local-user");
        upstream.abort();
    }
}
