//! Cached Komga announcements and GitHub release checks.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use axum::{
    Json,
    extract::{Extension, State},
    http::StatusCode,
};
use chrono::{DateTime, FixedOffset};
use futures_util::StreamExt;
use moka::future::Cache;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    AppState,
    auth::AuthContext,
    error::ApiError,
    routes::{ApiJson, require_admin},
};

const ANNOUNCEMENTS_URL: &str = "https://komga.org/blog/feed.json";
const RELEASES_URL: &str = "https://api.github.com/repos/gotson/komga/releases";
const CACHE_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct JsonFeedDto {
    version: String,
    title: String,
    #[serde(rename = "home_page_url")]
    home_page_url: Option<String>,
    description: Option<String>,
    #[serde(default)]
    items: Vec<JsonFeedItemDto>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JsonFeedItemDto {
    id: String,
    url: Option<String>,
    title: Option<String>,
    summary: Option<String>,
    #[serde(rename = "content_html")]
    content_html: Option<String>,
    #[serde(rename = "date_modified")]
    date_modified: Option<DateTime<FixedOffset>>,
    author: Option<JsonFeedAuthorDto>,
    #[serde(default)]
    tags: BTreeSet<String>,
    #[serde(rename = "_komga")]
    komga_extension: Option<JsonFeedKomgaExtensionDto>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JsonFeedAuthorDto {
    name: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct JsonFeedKomgaExtensionDto {
    read: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct GithubReleaseDto {
    html_url: String,
    tag_name: String,
    published_at: DateTime<FixedOffset>,
    body: String,
    prerelease: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseDto {
    version: String,
    release_date: DateTime<FixedOffset>,
    url: String,
    latest: bool,
    pre_release: bool,
    description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalError {
    Empty,
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct OperationsService {
    client: reqwest::Client,
    announcements_url: Arc<str>,
    releases_url: Arc<str>,
    announcements: Cache<&'static str, Arc<JsonFeedDto>>,
    releases: Cache<&'static str, Arc<Vec<GithubReleaseDto>>>,
}

impl Default for OperationsService {
    fn default() -> Self {
        Self::new(ANNOUNCEMENTS_URL, RELEASES_URL)
    }
}

impl OperationsService {
    fn new(announcements_url: impl Into<Arc<str>>, releases_url: impl Into<Arc<str>>) -> Self {
        Self {
            client: reqwest::Client::new(),
            announcements_url: announcements_url.into(),
            releases_url: releases_url.into(),
            announcements: Cache::builder()
                .max_capacity(1)
                .time_to_idle(CACHE_IDLE_TIMEOUT)
                .build(),
            releases: Cache::builder()
                .max_capacity(1)
                .time_to_idle(CACHE_IDLE_TIMEOUT)
                .build(),
        }
    }

    #[cfg(test)]
    fn with_urls(announcements_url: String, releases_url: String) -> Self {
        Self::new(announcements_url, releases_url)
    }

    async fn announcements(&self) -> Result<Arc<JsonFeedDto>, ExternalError> {
        self.announcements
            .try_get_with("announcements", async {
                let Some(feed) = self.fetch_json::<JsonFeedDto>(&self.announcements_url, false).await? else {
                    return Err(ExternalError::Empty);
                };
                Ok(Arc::new(feed))
            })
            .await
            .map_err(|error| *error)
    }

    async fn releases(&self) -> Result<Arc<Vec<GithubReleaseDto>>, ExternalError> {
        self.releases
            .try_get_with("releases", async {
                let releases = self
                    .fetch_json::<Vec<GithubReleaseDto>>(&self.releases_url, true)
                    .await?
                    .unwrap_or_default();
                Ok(Arc::new(releases))
            })
            .await
            .map_err(|error| *error)
    }

    async fn fetch_json<T: DeserializeOwned>(&self, url: &str, releases: bool) -> Result<Option<T>, ExternalError> {
        let request_url = if releases {
            format!("{url}?per_page=20")
        } else {
            url.to_owned()
        };
        let request = self
            .client
            .get(request_url)
            .timeout(REQUEST_TIMEOUT)
            .header(
                reqwest::header::USER_AGENT,
                concat!("komga-rs/", env!("CARGO_PKG_VERSION")),
            )
            .header(reqwest::header::ACCEPT, "application/json");
        let response = request.send().await.map_err(|error| {
            tracing::warn!(%error, upstream = url, "external operation request failed");
            ExternalError::Unavailable
        })?;
        let response = response.error_for_status().map_err(|error| {
            tracing::warn!(%error, upstream = url, "external operation returned an error status");
            ExternalError::Unavailable
        })?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            tracing::warn!(upstream = url, "external operation response exceeded the size limit");
            return Err(ExternalError::Unavailable);
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                tracing::warn!(%error, upstream = url, "could not read external operation response");
                ExternalError::Unavailable
            })?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                tracing::warn!(upstream = url, "external operation response exceeded the size limit");
                return Err(ExternalError::Unavailable);
            }
            body.extend_from_slice(&chunk);
        }
        if body.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice(&body).map(Some).map_err(|error| {
            tracing::warn!(%error, upstream = url, "could not decode external operation response");
            ExternalError::Unavailable
        })
    }
}

pub(crate) async fn announcements(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<JsonFeedDto>, ApiError> {
    require_admin(&auth)?;
    let mut feed = state
        .operations
        .announcements()
        .await
        .map_err(|error| match error {
            ExternalError::Empty => ApiError::not_found("Announcements not found"),
            ExternalError::Unavailable => ApiError::internal("Could not retrieve announcements"),
        })?
        .as_ref()
        .clone();
    let read = state.storage.announcement_ids_read(&auth.user.id).await?;
    for item in &mut feed.items {
        item.komga_extension = Some(JsonFeedKomgaExtensionDto {
            read: read.contains(&item.id),
        });
    }
    Ok(Json(feed))
}

pub(crate) async fn mark_announcements_read(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    ApiJson(announcement_ids): ApiJson<BTreeSet<String>>,
) -> Result<StatusCode, ApiError> {
    require_admin(&auth)?;
    state
        .storage
        .mark_announcements_read(&auth.user.id, &announcement_ids)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn releases(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Vec<ReleaseDto>>, ApiError> {
    require_admin(&auth)?;
    let releases = state
        .operations
        .releases()
        .await
        .map_err(|_| ApiError::internal("Could not retrieve releases"))?;
    Ok(Json(
        releases
            .iter()
            .enumerate()
            .map(|(index, release)| ReleaseDto {
                version: release.tag_name.clone(),
                release_date: release.published_at,
                url: release.html_url.clone(),
                latest: index == 0,
                pre_release: release.prerelease,
                description: release.body.clone(),
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Router, routing::get};
    use base64::{Engine, engine::general_purpose::STANDARD};
    use http_body_util::BodyExt;
    use komga_storage::{Storage, TaskStorage};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::*;

    #[derive(Clone)]
    struct UpstreamState {
        feed_requests: Arc<AtomicUsize>,
        release_requests: Arc<AtomicUsize>,
    }

    async fn upstream() -> (String, UpstreamState) {
        let state = UpstreamState {
            feed_requests: Arc::new(AtomicUsize::new(0)),
            release_requests: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route(
                "/feed",
                get(|State(state): State<UpstreamState>| async move {
                    state.feed_requests.fetch_add(1, Ordering::Relaxed);
                    Json(serde_json::json!({
                        "version":"https://jsonfeed.org/version/1",
                        "title":"Announcements",
                        "home_page_url":"https://komga.org/blog",
                        "description":"Latest Komga announcements",
                        "items":[{
                            "id":"announcement-1",
                            "url":"https://komga.org/blog/one",
                            "title":"One",
                            "summary":null,
                            "content_html":"<p>One</p>",
                            "date_modified":"2023-03-21T00:00:00Z",
                            "author":{"name":"gotson","url":"https://github.com/gotson"},
                            "tags":["komga"],
                            "_komga":null
                        }]
                    }))
                }),
            )
            .route(
                "/releases",
                get(
                    |State(state): State<UpstreamState>, request: axum::extract::Request| async move {
                        state.release_requests.fetch_add(1, Ordering::Relaxed);
                        assert_eq!(request.uri().query(), Some("per_page=20"));
                        Json(serde_json::json!([{
                            "html_url":"https://github.com/gotson/komga/releases/tag/v1",
                            "tag_name":"v1",
                            "published_at":"2024-01-02T03:04:05Z",
                            "body":"Release notes",
                            "prerelease":true
                        }]))
                    },
                ),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state)
    }

    #[tokio::test]
    async fn announcements_and_releases_cache_success_and_personalize_read_state() {
        let (upstream, counters) = upstream().await;
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let mut state = AppState::new(storage, tasks).await.unwrap();
        let operations = OperationsService::with_urls(format!("{upstream}/feed"), format!("{upstream}/releases"));
        let (first_load, second_load) = tokio::join!(operations.announcements(), operations.announcements());
        assert!(first_load.is_ok());
        assert!(second_load.is_ok());
        let (first_load, second_load) = tokio::join!(operations.releases(), operations.releases());
        assert!(first_load.is_ok());
        assert!(second_load.is_ok());
        assert_eq!(counters.feed_requests.load(Ordering::Relaxed), 1);
        assert_eq!(counters.release_requests.load(Ordering::Relaxed), 1);
        state.operations = operations;
        let app = crate::build_app(state);
        claim(&app).await;
        let admin = basic("admin@example.org", "password");

        let second_admin = app
            .clone()
            .oneshot(
                axum::http::Request::post("/api/v2/users")
                    .header(axum::http::header::AUTHORIZATION, &admin)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(
                        r#"{"email":"second@example.org","password":"password","roles":["ADMIN"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second_admin.status(), StatusCode::CREATED);

        let first = request_json(&app, "/api/v1/announcements", &admin).await;
        assert_eq!(first["items"][0]["_komga"]["read"], false);
        let marked = app
            .clone()
            .oneshot(
                axum::http::Request::put("/api/v1/announcements")
                    .header(axum::http::header::AUTHORIZATION, &admin)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(r#"["announcement-1"]"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(marked.status(), StatusCode::NO_CONTENT);
        let second = request_json(&app, "/api/v1/announcements", &admin).await;
        assert_eq!(second["items"][0]["_komga"]["read"], true);
        let other_user = request_json(&app, "/api/v1/announcements", &basic("second@example.org", "password")).await;
        assert_eq!(other_user["items"][0]["_komga"]["read"], false);
        assert_eq!(counters.feed_requests.load(Ordering::Relaxed), 1);

        let release = request_json(&app, "/api/v1/releases", &admin).await;
        assert_eq!(release[0]["version"], "v1");
        assert_eq!(release[0]["releaseDate"], "2024-01-02T03:04:05Z");
        assert_eq!(release[0]["latest"], true);
        assert_eq!(release[0]["preRelease"], true);
        let _ = request_json(&app, "/api/v1/releases", &admin).await;
        assert_eq!(counters.release_requests.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn empty_and_failed_responses_are_not_cached() {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/empty",
            get({
                let requests = requests.clone();
                move || {
                    let requests = requests.clone();
                    async move {
                        requests.fetch_add(1, Ordering::Relaxed);
                        StatusCode::OK
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let url = format!("http://{address}/empty");
        let service = OperationsService::with_urls(url.clone(), url);

        assert_eq!(service.announcements().await.unwrap_err(), ExternalError::Empty);
        assert_eq!(service.announcements().await.unwrap_err(), ExternalError::Empty);
        assert_eq!(requests.load(Ordering::Relaxed), 2);
        assert!(service.releases().await.unwrap().is_empty());
        assert!(service.releases().await.unwrap().is_empty());
        assert_eq!(requests.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn upstream_status_json_and_size_failures_are_rejected_and_not_cached() {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/status",
                get({
                    let requests = requests.clone();
                    move || {
                        let requests = requests.clone();
                        async move {
                            requests.fetch_add(1, Ordering::Relaxed);
                            StatusCode::BAD_GATEWAY
                        }
                    }
                }),
            )
            .route(
                "/invalid",
                get({
                    let requests = requests.clone();
                    move || {
                        let requests = requests.clone();
                        async move {
                            requests.fetch_add(1, Ordering::Relaxed);
                            "not-json"
                        }
                    }
                }),
            )
            .route(
                "/large",
                get({
                    let requests = requests.clone();
                    move || {
                        let requests = requests.clone();
                        async move {
                            requests.fetch_add(1, Ordering::Relaxed);
                            vec![b'x'; MAX_RESPONSE_BYTES + 1]
                        }
                    }
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        for path in ["status", "invalid", "large"] {
            let url = format!("http://{address}/{path}");
            let service = OperationsService::with_urls(url.clone(), url);
            assert_eq!(service.announcements().await.unwrap_err(), ExternalError::Unavailable);
            assert_eq!(service.announcements().await.unwrap_err(), ExternalError::Unavailable);
        }
        assert_eq!(requests.load(Ordering::Relaxed), 6);
    }

    async fn claim(app: &Router) {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::post("/api/v1/claim")
                    .header("X-Komga-Email", "admin@example.org")
                    .header("X-Komga-Password", "password")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    fn basic(email: &str, password: &str) -> String {
        format!("Basic {}", STANDARD.encode(format!("{email}:{password}")))
    }

    async fn request_json(app: &Router, path: &str, authorization: &str) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::get(path)
                    .header(axum::http::header::AUTHORIZATION, authorization)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }
}
