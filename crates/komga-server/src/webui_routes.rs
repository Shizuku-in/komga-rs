use std::path::Path;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{Method, Request, Response, StatusCode, header},
    response::IntoResponse,
};
use include_dir::{Dir, include_dir};
use tower_http::compression::CompressionLayer;

const BASE_URL_TOKEN: &str = "__KOMGA_BASE_URL__";
const NO_STORE: &str = "no-store";
const IMMUTABLE_ASSET_CACHE: &str = "public, max-age=31536000";
static WEBUI: Dir<'_> = include_dir!("$OUT_DIR/webui");

pub(crate) fn router(context_path: &str) -> Router {
    let base_url = if context_path.is_empty() {
        "/".to_owned()
    } else {
        format!("{context_path}/")
    };
    Router::new()
        .fallback(serve)
        .with_state(base_url)
        .layer(CompressionLayer::new().br(true).gzip(true))
}

async fn serve(State(base_url): State<String>, request: Request<Body>) -> Response<Body> {
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        return not_found();
    }

    let path = request.uri().path().trim_start_matches('/');
    if invalid_path(path) {
        return not_found();
    }
    if path.is_empty() || path == "index.html" {
        return index_response(&base_url, head);
    }
    if let Some(file) = WEBUI.get_file(path) {
        return asset_response(path, file.contents(), head);
    }
    if reserved_backend_path(path) || static_resource_path(path) {
        return not_found();
    }
    index_response(&base_url, head)
}

fn index_response(base_url: &str, head: bool) -> Response<Body> {
    let Some(index) = WEBUI.get_file("index.html").and_then(|file| file.contents_utf8()) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let rendered = index.replace(BASE_URL_TOKEN, base_url);
    response(Bytes::from(rendered), "text/html; charset=utf-8", NO_STORE, head)
}

fn asset_response(path: &str, contents: &'static [u8], head: bool) -> Response<Body> {
    let content_type = mime_guess::from_path(path).first_or_octet_stream().to_string();
    let cache_control = if hashed_asset_path(path) {
        IMMUTABLE_ASSET_CACHE
    } else {
        NO_STORE
    };
    response(Bytes::from_static(contents), &content_type, cache_control, head)
}

fn response(contents: Bytes, content_type: &str, cache_control: &str, head: bool) -> Response<Body> {
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control);
    if !head {
        builder = builder.header(header::CONTENT_LENGTH, contents.len().to_string());
    }
    builder
        .body(if head { Body::empty() } else { Body::from(contents) })
        .expect("static WebUI response headers are valid")
}

fn not_found() -> Response<Body> {
    StatusCode::NOT_FOUND.into_response()
}

fn invalid_path(path: &str) -> bool {
    path.contains('\\') || path.split('/').any(|segment| matches!(segment, "." | ".."))
}

fn reserved_backend_path(path: &str) -> bool {
    let mut segments = path.split('/');
    let first = segments.next().unwrap_or_default();
    if ["api", "opds", "sse", "actuator", "kobo", "koreader", "oauth2"]
        .iter()
        .any(|prefix| first.eq_ignore_ascii_case(prefix))
    {
        return true;
    }
    first.eq_ignore_ascii_case("login")
        && segments
            .next()
            .is_some_and(|segment| segment.eq_ignore_ascii_case("oauth2"))
}

fn static_resource_path(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or_default();
    matches!(first, "css" | "fonts" | "img" | "js") || Path::new(path).extension().is_some()
}

fn hashed_asset_path(path: &str) -> bool {
    matches!(path.split('/').next(), Some("css" | "fonts" | "img" | "js"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_backend_and_static_paths_without_prefix_confusion() {
        assert!(reserved_backend_path("api/v1/missing"));
        assert!(reserved_backend_path("API/v1/missing"));
        assert!(reserved_backend_path("login/oauth2/code/provider"));
        assert!(!reserved_backend_path("apiary"));
        assert!(static_resource_path("js/missing.js"));
        assert!(static_resource_path("robots.txt"));
        assert!(!static_resource_path("book/ABC"));
    }

    #[test]
    fn rejects_ambiguous_embedded_asset_paths() {
        assert!(invalid_path("js/../index.html"));
        assert!(invalid_path("js\\app.js"));
        assert!(!invalid_path("js/app.123.js"));
    }
}
