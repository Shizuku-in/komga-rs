use std::collections::BTreeSet;

use axum::{Router, http::Method};
use serde::Deserialize;

use super::*;

const MANIFEST: &str = include_str!("../../../../docs/api-parity.json");
const KOTLIN_OPENAPI: &str = include_str!("../../../../komga/komga/docs/openapi.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiManifest {
    baseline: String,
    scope: String,
    method_probe: String,
    exclusions: Vec<Exclusion>,
    groups: Vec<RouteGroup>,
}

#[derive(Debug, Deserialize)]
struct Exclusion {
    paths: Vec<String>,
    #[serde(default)]
    except: Vec<String>,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteGroup {
    area: String,
    kotlin_source: String,
    access: String,
    behavior: String,
    routes: Vec<ApiRoute>,
}

#[derive(Debug, Deserialize)]
struct ApiRoute {
    methods: Vec<String>,
    path: String,
}

#[tokio::test]
async fn core_api_manifest_is_valid_and_every_declared_route_is_registered() {
    let manifest: ApiManifest = serde_json::from_str(MANIFEST).unwrap();
    assert_eq!(manifest.baseline, "cba53ed");
    assert_eq!(manifest.scope, "Core Komga HTTP protocols supported by komga-rs");
    assert!(manifest.method_probe.contains("Allow header"));
    assert!(!manifest.exclusions.is_empty());

    for exclusion in &manifest.exclusions {
        assert!(!exclusion.paths.is_empty());
        assert!(!exclusion.reason.trim().is_empty());
        assert!(exclusion.except.iter().all(|path| path.starts_with('/')));
    }

    let root = test_app().await;
    let context = app_with_context_path("/komga").await;
    let mut routes = BTreeSet::new();
    let mut route_count = 0;

    for group in &manifest.groups {
        assert!(!group.area.trim().is_empty());
        assert!(!group.kotlin_source.trim().is_empty());
        assert!(matches!(
            group.access.as_str(),
            "public" | "protected" | "conditional" | "opds-basic" | "koreader-key" | "kobo-key"
        ));
        assert!(matches!(group.behavior.as_str(), "implemented" | "partial"));
        assert!(!group.routes.is_empty());

        for route in &group.routes {
            route_count += 1;
            assert!(route.path.starts_with('/'), "invalid path: {}", route.path);
            assert!(!route.methods.is_empty(), "missing methods: {}", route.path);
            assert!(routes.insert(route.path.clone()), "duplicate route: {}", route.path);

            let method_set = route.methods.iter().map(String::as_str).collect::<BTreeSet<_>>();
            assert_eq!(
                method_set.len(),
                route.methods.len(),
                "duplicate method: {}",
                route.path
            );
            assert!(
                method_set
                    .iter()
                    .all(|method| matches!(*method, "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "ANY")),
                "unknown method: {}",
                route.path
            );

            if group.area == "kobo" {
                continue;
            }
            assert!(
                !method_set.contains("ANY"),
                "ANY is only supported by the Kobo catch-all"
            );

            let path = sample_path(&route.path);
            assert_route_methods(&root, &path, &route.methods).await;
            assert_route_methods(&context, &format!("/komga{path}"), &route.methods).await;
            assert_route_access(&root, &path, &route.methods, &group.access).await;
        }
    }

    assert_eq!(route_count, 216, "update the reviewed baseline count with the manifest");
}

#[test]
fn manifest_contains_every_operation_in_the_pinned_kotlin_openapi() {
    let manifest: ApiManifest = serde_json::from_str(MANIFEST).unwrap();
    let openapi: serde_json::Value = serde_json::from_str(KOTLIN_OPENAPI).unwrap();
    let paths = openapi["paths"]
        .as_object()
        .expect("Kotlin OpenAPI paths must be an object");
    let mut kotlin_operations = BTreeSet::new();
    for (path, path_item) in paths {
        let path_item = path_item
            .as_object()
            .expect("Kotlin OpenAPI path item must be an object");
        for method in path_item.keys().filter(|method| is_http_method(method)) {
            kotlin_operations.insert((method.to_ascii_uppercase(), normalize_template(path)));
        }
    }
    assert_eq!(
        kotlin_operations.len(),
        174,
        "the pinned Kotlin OpenAPI baseline changed"
    );

    let manifest_operations = manifest
        .groups
        .iter()
        .flat_map(|group| &group.routes)
        .flat_map(|route| {
            route
                .methods
                .iter()
                .filter(|method| method.as_str() != "ANY")
                .map(|method| (method.clone(), normalize_template(&route.path)))
        })
        .collect::<BTreeSet<_>>();
    let missing = kotlin_operations
        .difference(&manifest_operations)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "Kotlin OpenAPI operations missing from the API manifest: {missing:?}"
    );
}

async fn app_with_context_path(context_path: &str) -> Router {
    let storage = Storage::connect_memory().await.unwrap();
    let tasks = TaskStorage::connect_memory().await.unwrap();
    let state = AppState::new(storage, tasks)
        .await
        .unwrap()
        .with_server_bind_settings(ServerBindSettings {
            configuration_port: 25_600,
            configuration_context_path: Some(context_path.to_owned()),
            effective_port: Some(25_600),
            effective_context_path: context_path.to_owned(),
        });
    build_app(state)
}

async fn assert_route_methods(app: &Router, path: &str, expected: &[String]) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::TRACE)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut allow = response
        .headers()
        .get(header::ALLOW)
        .unwrap_or_else(|| {
            panic!(
                "registered route did not return Allow for {path}: {}",
                response.status()
            )
        })
        .to_str()
        .unwrap()
        .split(',')
        .map(str::trim)
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if expected.contains("GET") {
        allow.remove("HEAD");
    }
    assert_eq!(allow, expected, "{path} method set drifted");
}

async fn assert_route_access(app: &Router, path: &str, methods: &[String], access: &str) {
    let method = methods
        .iter()
        .find(|method| method.as_str() != "ANY")
        .map_or(Method::PATCH, |method| Method::from_bytes(method.as_bytes()).unwrap());
    let response = app
        .clone()
        .oneshot(Request::builder().method(method).uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    match access {
        "protected" => assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path} was not protected"),
        "conditional" => assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} did not protect the non-public resource sample"
        ),
        "opds-basic" => {
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path} did not require OPDS authentication"
            );
            assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));
        }
        "koreader-key" => assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{path} did not require a KOReader key"
        ),
        "kobo-key" => assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} did not require a Kobo key"
        ),
        "public" => assert!(
            !matches!(response.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN),
            "public route {path} unexpectedly required authentication"
        ),
        other => panic!("unsupported access class: {other}"),
    }
}

fn is_http_method(value: &str) -> bool {
    matches!(value, "get" | "post" | "put" | "patch" | "delete")
}

fn normalize_template(path: &str) -> String {
    let mut normalized = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        normalized.push_str(&rest[..start]);
        let parameter = &rest[start + 1..];
        let end = parameter.find('}').expect("route parameter must close");
        normalized.push_str("{}");
        rest = &parameter[end + 1..];
    }
    normalized.push_str(rest);
    normalized.replace("/*", "/{}")
}

fn sample_path(template: &str) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let before = &rest[..start];
        result.push_str(before);
        let parameter = &rest[start + 1..];
        let end = parameter.find('}').expect("manifest route parameter must close");
        let name = parameter[..end].trim_start_matches('*');
        result.push_str(match name {
            "page" | "page_number" | "width" | "height" | "quality" => "1",
            "is_greyscale" => "false",
            "file_name" => "book.cbz",
            "resource" => "META-INF/container.xml",
            "registration_id" => "provider",
            "auth_token" => "invalid-api-key",
            "path" => "unsupported",
            _ => "parity-id",
        });
        rest = &parameter[end + 1..];
    }
    result.push_str(rest);
    result
}

#[test]
fn sample_paths_replace_segments_and_catch_alls() {
    assert_eq!(
        sample_path("/api/v1/books/{book_id}/resource/{*resource}"),
        "/api/v1/books/parity-id/resource/META-INF/container.xml"
    );
    assert_eq!(
        sample_path("/kobo/{auth_token}/v1/books/{thumbnail_id}/thumbnail/{width}/{height}/{is_greyscale}/image.jpg"),
        "/kobo/invalid-api-key/v1/books/parity-id/thumbnail/1/1/false/image.jpg"
    );
    assert_eq!(
        normalize_template("/api/v1/books/{bookId}/file/*"),
        "/api/v1/books/{}/file/{}"
    );
    assert_eq!(
        normalize_template("/api/v1/books/{book_id}/file/{*file_name}"),
        "/api/v1/books/{}/file/{}"
    );
}
