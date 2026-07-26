//! Restricted forwarding of Kobo device API requests to the official store.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use reqwest::redirect::Policy;
use serde_json::Value;
use thiserror::Error;

const STORE_BASE_URL: &str = "https://storeapi.kobo.com";
const X_KOBO_SYNC_TOKEN: &str = "x-kobo-synctoken";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// A completed upstream response containing only headers safe to return to Kobo.
#[derive(Debug)]
pub(super) struct ProxyResponse {
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap,
    pub(super) body: Bytes,
}

impl ProxyResponse {
    pub(super) fn json_value(&self) -> Result<Value, ProxyError> {
        serde_json::from_slice(&self.body).map_err(|error| ProxyError::InvalidJson(error.to_string()))
    }

    pub(super) fn header(&self, name: &str) -> Option<&HeaderValue> {
        self.headers.get(name)
    }

    pub(super) fn into_response(self) -> Response {
        let mut response = Body::from(self.body).into_response();
        *response.status_mut() = self.status;
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for name in self.headers.keys() {
            for value in self.headers.get_all(name) {
                response.headers_mut().append(name, value.clone());
            }
        }
        response
    }
}

/// Failures before a usable official Kobo response is available.
#[derive(Debug, Error)]
pub(super) enum ProxyError {
    #[error("Kobo Store proxying is disabled")]
    Disabled,
    #[error("could not derive the Kobo Store request path")]
    InvalidPath,
    #[error("official Kobo sync token is missing")]
    MissingSyncToken,
    #[error("Kobo Store request failed: {0}")]
    Request(String),
    #[error("Kobo Store returned HTTP {0}")]
    Upstream(StatusCode),
    #[error("Kobo Store returned invalid JSON: {0}")]
    InvalidJson(String),
}

/// Cloneable, production-pinned Kobo Store proxy service.
#[derive(Clone)]
pub(crate) struct KoboProxy {
    enabled: Arc<AtomicBool>,
    client: reqwest::Client,
    base_url: Arc<str>,
}

impl KoboProxy {
    pub(crate) fn new(enabled: bool) -> Self {
        Self::with_base_url(enabled, STORE_BASE_URL)
    }

    pub(super) fn with_base_url(enabled: bool, base_url: &str) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(REQUEST_TIMEOUT)
            .read_timeout(REQUEST_TIMEOUT)
            .redirect(Policy::none())
            .build()
            .expect("constant Kobo Store HTTP client configuration is valid");
        Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            client,
            base_url: Arc::from(base_url.trim_end_matches('/')),
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub(super) async fn forward(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
        body: Option<Bytes>,
        raw_sync_token: Option<&str>,
    ) -> Result<ProxyResponse, ProxyError> {
        if !self.is_enabled() {
            return Err(ProxyError::Disabled);
        }
        let upstream_path = upstream_path(uri.path()).ok_or(ProxyError::InvalidPath)?;
        let url = match uri.query() {
            Some(query) => format!("{}{upstream_path}?{query}", self.base_url),
            None => format!("{}{upstream_path}", self.base_url),
        };
        let mut request = self.client.request(method.clone(), url);
        for name in headers.keys().filter(|name| forwarded_request_header(name.as_str())) {
            for value in headers.get_all(name) {
                request = request.header(name, value);
            }
        }
        if let Some(raw_sync_token) = raw_sync_token {
            if raw_sync_token.trim().is_empty() {
                return Err(ProxyError::MissingSyncToken);
            }
            request = request.header(X_KOBO_SYNC_TOKEN, raw_sync_token);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| ProxyError::Request(request_error_kind(&error).to_owned()))?;
        let status = response.status();
        let mut response_headers = HeaderMap::new();
        for name in response.headers().keys().filter(|name| is_kobo_header(name.as_str())) {
            for value in response.headers().get_all(name) {
                response_headers.append(name, value.clone());
            }
        }
        if status.is_client_error() || status.is_server_error() {
            return Err(ProxyError::Upstream(status));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| ProxyError::Request(request_error_kind(&error).to_owned()))?;
        Ok(ProxyResponse {
            status,
            headers: response_headers,
            body,
        })
    }
}

pub(super) fn error_response(error: &ProxyError) -> Response {
    let status = match error {
        ProxyError::Upstream(status) => *status,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    status.into_response()
}

fn upstream_path(path: &str) -> Option<&str> {
    let after_prefix = path.strip_prefix("/kobo/")?;
    let slash = after_prefix.find('/')?;
    Some(&after_prefix[slash..])
}

fn forwarded_request_header(name: &str) -> bool {
    !name.eq_ignore_ascii_case(X_KOBO_SYNC_TOKEN)
        && (is_kobo_header(name)
            || [
                header::AUTHORIZATION.as_str(),
                header::USER_AGENT.as_str(),
                header::ACCEPT.as_str(),
                header::ACCEPT_LANGUAGE.as_str(),
                header::CONTENT_TYPE.as_str(),
            ]
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed)))
}

fn is_kobo_header(name: &str) -> bool {
    name.len() >= 7 && name[..7].eq_ignore_ascii_case("x-kobo-")
}

fn request_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else if error.is_body() {
        "response body failed"
    } else {
        "transport failed"
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        Json, Router,
        extract::Request,
        routing::{any, get},
    };
    use serde_json::json;
    use tokio::net::TcpListener;

    use super::*;

    async fn observed(request: Request) -> impl IntoResponse {
        let method = request.method().to_string();
        let uri = request.uri().to_string();
        let headers = request
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let body = axum::body::to_bytes(request.into_body(), usize::MAX).await.unwrap();
        (
            [("x-kobo-reply", "yes"), ("x-not-kobo", "no")],
            Json(json!({
                "method": method,
                "uri": uri,
                "headers": headers,
                "body": String::from_utf8_lossy(&body),
            })),
        )
    }

    async fn upstream() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/error", get(|| async { StatusCode::IM_A_TEAPOT }))
            .fallback(any(observed));
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), handle)
    }

    #[tokio::test]
    async fn forwards_only_kobo_headers_and_preserves_path_query_and_body() {
        let (base_url, handle) = upstream().await;
        let proxy = KoboProxy::with_base_url(true, &base_url);
        let uri = "/kobo/local-secret/v1/test%20path?one=two%20words".parse().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer official"));
        headers.insert(header::COOKIE, HeaderValue::from_static("private=true"));
        headers.insert("x-kobo-client", HeaderValue::from_static("device"));
        headers.insert(X_KOBO_SYNC_TOKEN, HeaderValue::from_static("wrapped-secret"));
        let response = proxy
            .forward(
                &Method::PATCH,
                &uri,
                &headers,
                Some(Bytes::from_static(b"payload")),
                Some("official.token"),
            )
            .await
            .unwrap();
        assert_eq!(response.header("x-kobo-reply").unwrap(), "yes");
        assert!(response.header("x-not-kobo").is_none());
        let body = response.json_value().unwrap();
        assert_eq!(body["method"], "PATCH");
        assert_eq!(body["uri"], "/v1/test%20path?one=two%20words");
        assert_eq!(body["body"], "payload");
        assert_eq!(body["headers"]["authorization"], "Bearer official");
        assert_eq!(body["headers"]["x-kobo-client"], "device");
        assert_eq!(body["headers"][X_KOBO_SYNC_TOKEN], "official.token");
        assert!(body["headers"].get("cookie").is_none());
        handle.abort();
    }

    #[tokio::test]
    async fn preserves_upstream_error_status_without_its_body() {
        let (base_url, handle) = upstream().await;
        let proxy = KoboProxy::with_base_url(true, &base_url);
        let error = proxy
            .forward(
                &Method::GET,
                &"/kobo/local-secret/error".parse().unwrap(),
                &HeaderMap::new(),
                None,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ProxyError::Upstream(StatusCode::IM_A_TEAPOT)));
        handle.abort();
    }

    #[tokio::test]
    async fn proxy_enablement_changes_without_replacing_the_upstream_client() {
        let (base_url, handle) = upstream().await;
        let proxy = KoboProxy::with_base_url(false, &base_url);
        let uri = "/kobo/local-secret/v1/resources".parse().unwrap();
        assert!(matches!(
            proxy.forward(&Method::GET, &uri, &HeaderMap::new(), None, None).await,
            Err(ProxyError::Disabled)
        ));
        proxy.set_enabled(true);
        assert!(
            proxy
                .forward(&Method::GET, &uri, &HeaderMap::new(), None, None)
                .await
                .is_ok()
        );
        proxy.set_enabled(false);
        assert!(!proxy.is_enabled());
        handle.abort();
    }
}
