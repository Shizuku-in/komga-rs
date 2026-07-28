use std::collections::BTreeSet;

use axum::{Router, http::Method};
use serde::Deserialize;

use super::*;

const MANIFEST: &str = include_str!("../../../../docs/api-parity.json");

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
    assert_eq!(manifest.baseline, "4ef86df");
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
            "public" | "protected" | "opds-basic" | "koreader-key" | "kobo-key"
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
        }
    }

    assert_eq!(route_count, 207, "update the reviewed baseline count with the manifest");
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
    let allow = response
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
    for method in expected {
        assert!(
            allow.contains(method.as_str()),
            "{path} did not allow {method}: {allow:?}"
        );
    }
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
}
