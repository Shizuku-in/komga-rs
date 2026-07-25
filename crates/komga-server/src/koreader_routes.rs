//! KOReader progress-sync protocol compatibility.

use std::{collections::BTreeSet, str::FromStr, sync::OnceLock};

use axum::{
    Json, Router,
    body::Body,
    extract::{Extension, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use chrono::Utc;
use headers_accept::Accept;
use komga_core::{
    DomainEvent, Media, MediaExtension, MediaProfile, R2Device, R2Location, R2Locator, R2Progression,
    ReadProgressError, ReadProgressEvent, progression,
};
use mediatype::MediaTypeBuf;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{AppState, auth, auth::AuthContext, error::ApiError, routes::ApiJson};

const JSON_MEDIA_TYPE: &str = "application/json";
const KOREADER_MEDIA_TYPE: &str = "application/vnd.koreader.v1+json";

pub(crate) fn router(state: &AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/koreader/users/auth", get(authorize))
        .route("/koreader/syncs/progress/{book_hash}", get(get_progress))
        .route("/koreader/syncs/progress", put(update_progress))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_koreader_auth,
        ));
    Router::new()
        .route("/koreader/users/create", post(register_user))
        .merge(protected)
}

#[derive(Debug, Serialize)]
struct UserAuthenticationDto {
    authorized: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DocumentProgressDto {
    document: String,
    percentage: f32,
    progress: String,
    device: String,
    device_id: String,
}

#[derive(Debug, Serialize)]
struct NoProgressDto {
    status: u16,
    error: &'static str,
    message: &'static str,
}

async fn register_user() -> Result<StatusCode, ApiError> {
    Err(ApiError::forbidden())
}

async fn authorize(headers: HeaderMap) -> Result<Response, ApiError> {
    json_response(&headers, UserAuthenticationDto { authorized: "OK" })
}

async fn get_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(book_hash): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let (book, media) = resolve_book(&state, &auth, &book_hash).await?;
    let Some(progress) = state.storage.find_read_progress(&book.id, &auth.user.id).await? else {
        return json_response(
            &headers,
            NoProgressDto {
                status: 200,
                error: "OK",
                message: "No progress found for this book",
            },
        );
    };
    let percentage = progress
        .locator
        .as_ref()
        .and_then(|locator| locator.locations.as_ref())
        .and_then(|locations| locations.total_progression)
        .unwrap_or_else(|| {
            if media.page_count > 0 {
                progress.page as f32 / media.page_count as f32
            } else {
                0.0
            }
        });
    let protocol_progress = match media.profile() {
        Some(MediaProfile::Divina | MediaProfile::Pdf) => progress.page.to_string(),
        Some(MediaProfile::Epub) => {
            let extension = epub_extension(&media)?;
            let href = progress.locator.as_ref().map(|locator| locator.href.as_str());
            let resource = unique_epub_hrefs(extension)
                .iter()
                .position(|candidate| Some(candidate.as_str()) == href)
                .map_or(0, |index| index + 1);
            format!("/body/DocFragment[{resource}].0")
        }
        None => return Err(ApiError::not_found("Book has no media profile")),
    };
    json_response(
        &headers,
        DocumentProgressDto {
            document: book_hash,
            percentage,
            progress: protocol_progress,
            device: progress.device_name,
            device_id: progress.device_id,
        },
    )
}

async fn update_progress(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    ApiJson(payload): ApiJson<DocumentProgressDto>,
) -> Result<Response, ApiError> {
    require_json_content_type(&headers)?;
    response_media_type(&headers)?;
    if !payload.percentage.is_finite() || !(0.0..=1.0).contains(&payload.percentage) {
        return Err(ApiError::bad_request("percentage must be between 0 and 1"));
    }
    let (book, media) = resolve_book(&state, &auth, &payload.document).await?;
    let locator = match media.profile() {
        Some(MediaProfile::Divina | MediaProfile::Pdf) => {
            let page = payload
                .progress
                .parse::<i32>()
                .map_err(|_| ApiError::bad_request("progress must be a valid page number"))?;
            R2Locator {
                href: String::new(),
                media_type: String::new(),
                title: None,
                locations: Some(R2Location {
                    position: Some(page),
                    total_progression: Some(payload.percentage),
                    ..R2Location::default()
                }),
                text: None,
                kobo_span: None,
            }
        }
        Some(MediaProfile::Epub) => {
            let extension = epub_extension(&media)?;
            let index = parse_epub_resource_index(&payload.progress)?;
            let href = unique_epub_hrefs(extension)
                .get(index)
                .cloned()
                .ok_or_else(|| ApiError::bad_request("EPUB resource index is out of range"))?;
            R2Locator {
                href,
                media_type: "application/xhtml+xml".into(),
                title: None,
                locations: Some(R2Location {
                    progression: Some(0.0),
                    total_progression: Some(payload.percentage),
                    ..R2Location::default()
                }),
                text: None,
                kobo_span: None,
            }
        }
        None => return Err(ApiError::not_found("Book has no media profile")),
    };
    let saved = state.storage.find_read_progress(&book.id, &auth.user.id).await?;
    let normalized = progression(
        &book.id,
        &auth.user.id,
        &media,
        saved.as_ref(),
        R2Progression {
            modified: Utc::now(),
            device: R2Device {
                id: payload.device_id,
                name: payload.device,
            },
            locator,
        },
    )
    .map_err(map_progress_error)?;
    state.storage.save_read_progress(&normalized).await?;
    state.publish_domain_event(DomainEvent::ReadProgressChanged(ReadProgressEvent {
        book_id: book.id,
        user_id: auth.user.id,
    }));
    empty_json_response(&headers)
}

async fn resolve_book(state: &AppState, auth: &AuthContext, hash: &str) -> Result<(komga_core::Book, Media), ApiError> {
    let matches = state
        .storage
        .find_visible_book_ids_by_koreader_hash(hash, &auth.user)
        .await?;
    let book_id = match matches.as_slice() {
        [] => return Err(ApiError::not_found("Book not found")),
        [book_id] => book_id,
        _ => return Err(ApiError::conflict("More than 1 book found with the same hash")),
    };
    let book = state
        .storage
        .find_book_by_id(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book not found"))?;
    let media = state
        .storage
        .find_media_by_book_id(book_id)
        .await?
        .ok_or_else(|| ApiError::not_found("Book has no media profile"))?;
    Ok((book, media))
}

fn epub_extension(media: &Media) -> Result<&komga_core::MediaExtensionEpub, ApiError> {
    match media.extension.as_ref() {
        Some(MediaExtension::Epub(extension)) => Ok(extension),
        None => Err(ApiError::bad_request("Epub extension not found")),
    }
}

fn unique_epub_hrefs(extension: &komga_core::MediaExtensionEpub) -> Vec<String> {
    let mut seen = BTreeSet::new();
    extension
        .positions
        .iter()
        .filter(|position| seen.insert(position.href.clone()))
        .map(|position| position.href.clone())
        .collect()
}

fn parse_epub_resource_index(progress: &str) -> Result<usize, ApiError> {
    static ONE_BASED: OnceLock<Regex> = OnceLock::new();
    static ZERO_BASED: OnceLock<Regex> = OnceLock::new();
    let one_based = ONE_BASED.get_or_init(|| Regex::new(r"(?i)DocFragment\[(\d+)]").expect("valid regex"));
    if let Some(value) = one_based
        .captures(progress)
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<usize>().ok())
    {
        return value
            .checked_sub(1)
            .ok_or_else(|| ApiError::bad_request("EPUB resource index must start at 1"));
    }
    let zero_based = ZERO_BASED.get_or_init(|| Regex::new(r"(?i)#_doc_fragment_(\d+)_").expect("valid regex"));
    zero_based
        .captures(progress)
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<usize>().ok())
        .ok_or_else(|| ApiError::bad_request(format!("Could not get Epub resource index from progress: {progress}")))
}

fn response_media_type(headers: &HeaderMap) -> Result<&'static str, ApiError> {
    let Some(value) = headers.get(header::ACCEPT) else {
        return Ok(JSON_MEDIA_TYPE);
    };
    let accept = Accept::try_from(value).map_err(|_| ApiError::not_acceptable())?;
    let json = MediaTypeBuf::from_str(JSON_MEDIA_TYPE).expect("valid media type");
    let koreader = MediaTypeBuf::from_str(KOREADER_MEDIA_TYPE).expect("valid media type");
    let json_ref = json.essence();
    let koreader_ref = koreader.essence();
    match accept.negotiate([&json_ref, &koreader_ref]) {
        Some(selected) if selected == &koreader_ref => Ok(KOREADER_MEDIA_TYPE),
        Some(_) => Ok(JSON_MEDIA_TYPE),
        None => Err(ApiError::not_acceptable()),
    }
}

fn require_json_content_type(headers: &HeaderMap) -> Result<(), ApiError> {
    let value = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(ApiError::unsupported_media_type)?;
    let media_type = MediaTypeBuf::from_str(value).map_err(|_| ApiError::unsupported_media_type())?;
    match media_type.essence().to_string().as_str() {
        JSON_MEDIA_TYPE | KOREADER_MEDIA_TYPE => Ok(()),
        _ => Err(ApiError::unsupported_media_type()),
    }
}

fn json_response(headers: &HeaderMap, value: impl Serialize) -> Result<Response, ApiError> {
    let mut response = Json(value).into_response();
    set_content_type(&mut response, response_media_type(headers)?);
    Ok(response)
}

fn empty_json_response(headers: &HeaderMap) -> Result<Response, ApiError> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    set_content_type(&mut response, response_media_type(headers)?);
    Ok(response)
}

fn set_content_type(response: &mut Response, media_type: &'static str) {
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(media_type));
}

fn map_progress_error(error: ReadProgressError) -> ApiError {
    match error {
        ReadProgressError::Invalid(message) => ApiError::bad_request(message),
        ReadProgressError::Conflict => ApiError::conflict("Progression is older than existing"),
    }
}
