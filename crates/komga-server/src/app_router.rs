//! Top-level REST router and middleware composition.

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, Request, StatusCode, Uri, header, uri::Authority},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, patch, post},
};
use tower_http::{
    cors::{AllowHeaders, AllowMethods, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

#[cfg(feature = "webui")]
use crate::webui_routes;
use crate::{
    AppState, auth, authentication_activity_routes, book_file_routes, container_routes, content_routes,
    filesystem_routes, font_routes, history_routes, info_routes, kobo, koreader_routes, library_routes, media_routes,
    metadata_routes, metrics_routes, oauth, opds_router, operations_routes, page_hash_routes, read_list_file_routes,
    read_list_match_routes, read_progress_routes, referential_routes, routes, series_file_routes, settings_routes,
    source_file_routes, source_import_routes, sse_routes, thumbnail_routes, transient_book_routes, webpub_routes,
};

pub(crate) fn build(state: AppState) -> Router {
    let context_path = state.effective_server_context_path.clone();
    let cors = (!state.cors_allowed_origins.is_empty()).then(|| {
        CorsLayer::new()
            .allow_origin(state.cors_allowed_origins.to_vec())
            .allow_methods(AllowMethods::mirror_request())
            .allow_headers(AllowHeaders::mirror_request())
            .allow_credentials(true)
            .expose_headers([
                header::CONTENT_DISPOSITION,
                header::HeaderName::from_static("x-auth-token"),
            ])
            .max_age(std::time::Duration::from_secs(30 * 60))
    });
    let protected = protected_api()
        .merge(opds_router::protected())
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    let app = public_api()
        .merge(opds_router::public())
        .merge(koreader_routes::router(&state))
        .merge(kobo::router(&state))
        .merge(protected)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http().make_span_with(make_http_span))
        .layer(middleware::from_fn(suppress_ajax_basic_challenge))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            apply_server_request_settings,
        ));
    let app = if let Some(cors) = cors { app.layer(cors) } else { app };
    let app = app.with_state(state);
    #[cfg(feature = "webui")]
    let app = app.fallback_service(webui_routes::router(&context_path));
    if context_path.is_empty() {
        app
    } else {
        #[cfg(feature = "webui")]
        {
            let context_root = format!("{context_path}/");
            let root_context_path = context_path.clone();
            Router::new()
                .route(
                    &context_root,
                    get(move || {
                        let root_context_path = root_context_path.clone();
                        async move { webui_routes::index(&root_context_path) }
                    }),
                )
                .nest(&context_path, app)
        }
        #[cfg(not(feature = "webui"))]
        Router::new().nest(&context_path, app)
    }
}

async fn suppress_ajax_basic_challenge(request: Request<Body>, next: Next) -> Response {
    let ajax = is_xml_http_request(request.headers());
    let mut response = next.run(request).await;
    if ajax
        && response.status() == StatusCode::UNAUTHORIZED
        && response.headers().get(header::WWW_AUTHENTICATE) == Some(&HeaderValue::from_static("Basic realm=\"Komga\""))
    {
        response.headers_mut().remove(header::WWW_AUTHENTICATE);
    }
    response
}

fn is_xml_http_request(headers: &HeaderMap) -> bool {
    headers
        .get("x-requested-with")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "XMLHttpRequest")
}

async fn apply_server_request_settings(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let has_forwarded_header = has_forwarded_header(request.headers());
    if !state.effective_server_context_path.is_empty()
        && !request.headers().contains_key("x-forwarded-prefix")
        && let Ok(value) = HeaderValue::from_str(&state.effective_server_context_path)
    {
        request.headers_mut().insert("x-forwarded-prefix", value);
    }
    if request.uri().path().starts_with("/kobo/")
        && !has_forwarded_header
        && let Some(port) = state.kobo_public_port().or(state.effective_server_port)
        && let Some(authority) = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<Authority>().ok())
    {
        let host = authority.host();
        let host = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        if let Ok(value) = HeaderValue::from_str(&host) {
            request.headers_mut().insert(header::HOST, value);
        }
    }
    next.run(request).await
}

fn has_forwarded_header(headers: &axum::http::HeaderMap) -> bool {
    [
        "forwarded",
        "x-forwarded-host",
        "x-forwarded-port",
        "x-forwarded-proto",
        "x-forwarded-prefix",
        "x-forwarded-ssl",
        "x-forwarded-for",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
}

fn make_http_span(request: &Request<Body>) -> tracing::Span {
    tracing::info_span!(
        "request",
        method = %request.method(),
        uri = %redacted_request_uri(request.uri())
    )
}

fn redacted_request_uri(uri: &Uri) -> String {
    let path = uri.path();
    if path.contains("/login/oauth2/code/") {
        return path.to_owned();
    }
    let redacted = path.strip_prefix("/kobo/").map_or_else(
        || path.to_owned(),
        |rest| match rest.split_once('/') {
            Some((_, suffix)) => format!("/kobo/{{authToken}}/{suffix}"),
            None => "/kobo/{authToken}".to_owned(),
        },
    );
    match uri.query() {
        Some(query) => format!("{redacted}?{query}"),
        None => redacted,
    }
}

fn protected_api() -> Router<AppState> {
    Router::new()
        .route("/actuator/info", get(info_routes::info))
        .route("/actuator/metrics", get(metrics_routes::list))
        .route("/actuator/metrics/{metric}", get(metrics_routes::get))
        .route(
            "/api/v1/announcements",
            get(operations_routes::announcements).put(operations_routes::mark_announcements_read),
        )
        .route("/api/v1/releases", get(operations_routes::releases))
        .route("/api/v1/fonts/families", get(font_routes::families))
        .route("/api/v2/users/me", get(routes::current_user))
        .route(
            "/api/v1/users/me/authentication-activity",
            get(authentication_activity_routes::current_user_activity),
        )
        .route(
            "/api/v2/users/me/authentication-activity",
            get(authentication_activity_routes::current_user_activity),
        )
        .route(
            "/api/v1/users/authentication-activity",
            get(authentication_activity_routes::all_activity),
        )
        .route(
            "/api/v2/users/authentication-activity",
            get(authentication_activity_routes::all_activity),
        )
        .route(
            "/api/v1/users/{user_id}/authentication-activity/latest",
            get(authentication_activity_routes::latest_user_activity),
        )
        .route(
            "/api/v2/users/{user_id}/authentication-activity/latest",
            get(authentication_activity_routes::latest_user_activity),
        )
        .route("/api/v2/users/me/password", patch(routes::update_my_password))
        .route(
            "/api/v2/users/me/api-keys",
            get(routes::list_api_keys).post(routes::create_api_key),
        )
        .route("/api/v2/users/me/api-keys/{key_id}", delete(routes::delete_api_key))
        .route("/api/v1/syncpoints/me", delete(routes::delete_my_sync_points))
        .route("/api/v2/users", get(routes::list_users).post(routes::create_user))
        .route(
            "/api/v2/users/{user_id}",
            patch(routes::patch_user).delete(routes::delete_user),
        )
        .route("/api/v2/users/{user_id}/password", patch(routes::update_user_password))
        .route("/api/v1/login/set-cookie", get(routes::set_cookie))
        .route("/api/logout", get(routes::logout).post(routes::logout))
        .route("/api/v1/authors", get(referential_routes::authors_v1))
        .route("/api/v2/authors", get(referential_routes::authors_v2))
        .route("/api/v1/authors/names", get(referential_routes::author_names))
        .route("/api/v1/authors/roles", get(referential_routes::author_roles))
        .route("/api/v1/genres", get(referential_routes::genres))
        .route("/api/v1/sharing-labels", get(referential_routes::sharing_labels))
        .route("/api/v1/tags", get(referential_routes::tags))
        .route("/api/v1/tags/book", get(referential_routes::book_tags))
        .route("/api/v1/tags/series", get(referential_routes::series_tags))
        .route("/api/v1/languages", get(referential_routes::languages))
        .route("/api/v1/publishers", get(referential_routes::publishers))
        .route("/api/v1/age-ratings", get(referential_routes::age_ratings))
        .route(
            "/api/v1/series/release-dates",
            get(referential_routes::series_release_dates),
        )
        .route("/api/v1/series/list", post(content_routes::list_series))
        .route(
            "/api/v1/series/list/alphabetical-groups",
            post(content_routes::series_alphabetical_groups),
        )
        .route("/api/v1/series", get(content_routes::legacy_list_series))
        .route(
            "/api/v1/series/alphabetical-groups",
            get(content_routes::legacy_series_alphabetical_groups),
        )
        .route("/api/v1/series/latest", get(content_routes::latest_series))
        .route("/api/v1/series/new", get(content_routes::new_series))
        .route("/api/v1/series/updated", get(content_routes::updated_series))
        .route("/api/v1/series/{series_id}", get(content_routes::get_series))
        .route(
            "/api/v1/series/{series_id}/books",
            get(content_routes::legacy_series_books),
        )
        .route("/api/v1/series/{series_id}/analyze", post(media_routes::analyze_series))
        .route(
            "/api/v1/series/{series_id}/file",
            get(series_file_routes::download_series).delete(source_file_routes::delete_series_files),
        )
        .route(
            "/api/v1/series/{series_id}/collections",
            get(container_routes::series_collections),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnail",
            get(thumbnail_routes::get_series_thumbnail),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnails",
            get(thumbnail_routes::list_series_thumbnails)
                .post(thumbnail_routes::upload_series_thumbnail)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnails/{thumbnail_id}",
            get(thumbnail_routes::get_series_thumbnail_by_id).delete(thumbnail_routes::delete_series_thumbnail),
        )
        .route(
            "/api/v1/series/{series_id}/thumbnails/{thumbnail_id}/selected",
            axum::routing::put(thumbnail_routes::select_series_thumbnail),
        )
        .route(
            "/api/v1/series/{series_id}/read-progress",
            post(read_progress_routes::mark_series_read_completed)
                .delete(read_progress_routes::delete_series_read_progress),
        )
        .route(
            "/api/v2/series/{series_id}/read-progress/tachiyomi",
            get(read_progress_routes::get_tachiyomi_series_read_progress)
                .put(read_progress_routes::update_tachiyomi_series_read_progress),
        )
        .route(
            "/api/v1/series/{series_id}/metadata",
            patch(metadata_routes::update_series_metadata),
        )
        .route(
            "/api/v1/series/{series_id}/metadata/refresh",
            post(metadata_routes::refresh_series_metadata),
        )
        .route("/api/v1/books/list", post(content_routes::list_books))
        .route("/api/v1/books/import", post(source_import_routes::import_books))
        .route("/api/v1/transient-books", post(transient_book_routes::scan))
        .route(
            "/api/v1/transient-books/{id}/analyze",
            post(transient_book_routes::analyze),
        )
        .route(
            "/api/v1/transient-books/{id}/pages/{page}",
            get(transient_book_routes::page),
        )
        .route("/api/v1/books", get(content_routes::legacy_list_books))
        .route("/api/v1/books/latest", get(content_routes::latest_books))
        .route("/api/v1/books/ondeck", get(content_routes::books_on_deck))
        .route("/api/v1/books/duplicates", get(content_routes::duplicate_books))
        .route("/api/v1/history", get(history_routes::list))
        .route(
            "/api/v1/page-hashes",
            get(page_hash_routes::known).put(page_hash_routes::create_or_update),
        )
        .route("/api/v1/page-hashes/unknown", get(page_hash_routes::unknown))
        .route(
            "/api/v1/page-hashes/unknown/{page_hash}/thumbnail",
            get(page_hash_routes::unknown_thumbnail),
        )
        .route(
            "/api/v1/page-hashes/{page_hash}/thumbnail",
            get(page_hash_routes::known_thumbnail),
        )
        .route("/api/v1/page-hashes/{page_hash}", get(page_hash_routes::matches))
        .route(
            "/api/v1/page-hashes/{page_hash}/delete-all",
            post(page_hash_routes::delete_all),
        )
        .route(
            "/api/v1/page-hashes/{page_hash}/delete-match",
            post(page_hash_routes::delete_match),
        )
        .route(
            "/api/v1/books/thumbnails",
            axum::routing::put(thumbnail_routes::regenerate_book_thumbnails),
        )
        .route(
            "/api/v1/books/metadata",
            patch(metadata_routes::update_book_metadata_batch),
        )
        .route("/api/v1/books/{book_id}", get(content_routes::get_book))
        .route(
            "/api/v1/books/{book_id}/readlists",
            get(container_routes::book_read_lists),
        )
        .route(
            "/api/v1/collections",
            get(container_routes::list_collections).post(container_routes::create_collection),
        )
        .route(
            "/api/v1/collections/{id}",
            get(container_routes::get_collection)
                .patch(container_routes::patch_collection)
                .delete(container_routes::delete_collection),
        )
        .route(
            "/api/v1/collections/{id}/series",
            get(container_routes::collection_series),
        )
        .route(
            "/api/v1/collections/{id}/thumbnail",
            get(container_routes::get_collection_thumbnail),
        )
        .route(
            "/api/v1/collections/{id}/thumbnails",
            get(container_routes::list_collection_thumbnails)
                .post(container_routes::upload_collection_thumbnail)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route(
            "/api/v1/collections/{id}/thumbnails/{thumbnail_id}",
            get(container_routes::get_collection_thumbnail_id).delete(container_routes::delete_collection_thumbnail),
        )
        .route(
            "/api/v1/collections/{id}/thumbnails/{thumbnail_id}/selected",
            axum::routing::put(container_routes::select_collection_thumbnail),
        )
        .route(
            "/api/v1/readlists",
            get(container_routes::list_read_lists).post(container_routes::create_read_list),
        )
        .route(
            "/api/v1/readlists/match/comicrack",
            axum::routing::post(read_list_match_routes::match_comicrack_read_list),
        )
        .route(
            "/api/v1/readlists/{id}",
            get(container_routes::get_read_list)
                .patch(container_routes::patch_read_list)
                .delete(container_routes::delete_read_list),
        )
        .route("/api/v1/readlists/{id}/books", get(container_routes::read_list_books))
        .route(
            "/api/v1/readlists/{id}/file",
            get(read_list_file_routes::download_read_list),
        )
        .route(
            "/api/v1/readlists/{id}/thumbnail",
            get(container_routes::get_read_list_thumbnail),
        )
        .route(
            "/api/v1/readlists/{id}/thumbnails",
            get(container_routes::list_read_list_thumbnails)
                .post(container_routes::upload_read_list_thumbnail)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route(
            "/api/v1/readlists/{id}/thumbnails/{thumbnail_id}",
            get(container_routes::get_read_list_thumbnail_id).delete(container_routes::delete_read_list_thumbnail),
        )
        .route(
            "/api/v1/readlists/{id}/thumbnails/{thumbnail_id}/selected",
            axum::routing::put(container_routes::select_read_list_thumbnail),
        )
        .route(
            "/api/v1/readlists/{id}/books/{book_id}/previous",
            get(container_routes::previous),
        )
        .route(
            "/api/v1/readlists/{id}/books/{book_id}/next",
            get(container_routes::next),
        )
        .route(
            "/api/v1/readlists/{id}/read-progress/tachiyomi",
            get(container_routes::get_progress).put(container_routes::put_progress),
        )
        .route(
            "/api/v1/books/{book_id}/metadata",
            patch(metadata_routes::update_book_metadata),
        )
        .route(
            "/api/v1/books/{book_id}/metadata/refresh",
            post(metadata_routes::refresh_book_metadata),
        )
        .route(
            "/api/v1/books/{book_id}/read-progress",
            patch(read_progress_routes::update_read_progress).delete(read_progress_routes::delete_read_progress),
        )
        .route(
            "/api/v1/books/{book_id}/progression",
            get(read_progress_routes::get_progression).put(read_progress_routes::update_progression),
        )
        .route(
            "/api/v1/books/{book_id}/file",
            get(book_file_routes::download_book_file).delete(source_file_routes::delete_book_file),
        )
        .route(
            "/api/v1/books/{book_id}/file/{*file_name}",
            get(book_file_routes::download_book_file_with_name),
        )
        .route("/api/v1/books/{book_id}/analyze", post(media_routes::analyze_book))
        .route("/api/v1/books/{book_id}/pages", get(media_routes::list_pages))
        .route("/api/v1/books/{book_id}/positions", get(media_routes::get_positions))
        .route("/api/v1/books/{book_id}/manifest", get(webpub_routes::get_manifest))
        .route(
            "/api/v1/books/{book_id}/manifest/divina",
            get(webpub_routes::get_divina_manifest),
        )
        .route(
            "/api/v1/books/{book_id}/manifest/pdf",
            get(webpub_routes::get_pdf_manifest),
        )
        .route(
            "/api/v1/books/{book_id}/manifest/epub",
            get(webpub_routes::get_epub_manifest),
        )
        .route(
            "/api/v1/books/{book_id}/pages/{page_number}",
            get(media_routes::get_page),
        )
        .route(
            "/api/v1/books/{book_id}/pages/{page_number}/thumbnail",
            get(media_routes::get_page_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/pages/{page_number}/raw",
            get(media_routes::get_raw_page),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnail",
            get(thumbnail_routes::get_book_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnails",
            get(thumbnail_routes::list_book_thumbnails)
                .post(thumbnail_routes::upload_book_thumbnail)
                .layer(DefaultBodyLimit::max(10 * 1024 * 1024)),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnails/{thumbnail_id}",
            get(thumbnail_routes::get_book_thumbnail_by_id).delete(thumbnail_routes::delete_book_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/thumbnails/{thumbnail_id}/selected",
            axum::routing::put(thumbnail_routes::select_book_thumbnail),
        )
        .route(
            "/api/v1/books/{book_id}/previous",
            get(content_routes::get_previous_book),
        )
        .route("/api/v1/books/{book_id}/next", get(content_routes::get_next_book))
        .route(
            "/api/v1/libraries",
            get(library_routes::list_libraries).post(library_routes::create_library),
        )
        .route(
            "/api/v1/libraries/{library_id}",
            get(library_routes::get_library)
                .put(library_routes::update_library)
                .patch(library_routes::update_library)
                .delete(library_routes::delete_library),
        )
        .route(
            "/api/v1/libraries/{library_id}/scan",
            post(library_routes::scan_library),
        )
        .route(
            "/api/v1/libraries/{library_id}/analyze",
            post(media_routes::analyze_library),
        )
        .route(
            "/api/v1/libraries/{library_id}/metadata/refresh",
            post(metadata_routes::refresh_library_metadata),
        )
        .route(
            "/api/v1/libraries/{library_id}/empty-trash",
            post(library_routes::empty_trash),
        )
        .route("/api/v1/filesystem", post(filesystem_routes::directory_listing))
        .route(
            "/api/v1/client-settings/user/list",
            get(settings_routes::list_user_settings),
        )
        .route(
            "/api/v1/client-settings/global",
            patch(settings_routes::save_global_settings).delete(settings_routes::delete_global_settings),
        )
        .route(
            "/api/v1/client-settings/user",
            patch(settings_routes::save_user_settings).delete(settings_routes::delete_user_settings),
        )
        .route(
            "/api/v1/settings",
            get(settings_routes::server_settings).patch(settings_routes::update_server_settings),
        )
        .route("/api/v1/tasks", delete(settings_routes::clear_tasks))
        .route("/sse/v1/events", get(sse_routes::events))
}

fn public_api() -> Router<AppState> {
    Router::new()
        .route("/actuator/health", get(routes::health))
        .route("/api/v1/claim", get(routes::claim_status).post(routes::claim_server))
        .route(
            "/api/v1/client-settings/global/list",
            get(settings_routes::list_global_settings),
        )
        .route("/api/v1/oauth2/providers", get(oauth::provider_list))
        .route("/oauth2/authorization/{registration_id}", get(oauth::authorize))
        .route("/login/oauth2/code/{registration_id}", get(oauth::callback))
        .route("/api/v1/fonts/resource/{family}/css", get(font_routes::css))
        .route("/api/v1/fonts/resource/{family}/{file}", get(font_routes::resource))
        .route(
            "/api/v1/books/{book_id}/resource/{*resource}",
            get(media_routes::get_epub_resource),
        )
}

#[cfg(test)]
mod tests {
    use super::redacted_request_uri;

    #[test]
    fn tracing_redacts_only_the_kobo_path_token() {
        let uri = "/kobo/plain-secret/v1/library/sync?x=1".parse().unwrap();
        let redacted = redacted_request_uri(&uri);
        assert_eq!(redacted, "/kobo/{authToken}/v1/library/sync?x=1");
        assert!(!redacted.contains("plain-secret"));
        assert_eq!(redacted_request_uri(&"/api/v1/books".parse().unwrap()), "/api/v1/books");
    }

    #[test]
    fn tracing_redacts_the_oauth_callback_query() {
        let uri = "/login/oauth2/code/test?code=authorization-code&state=random-state"
            .parse()
            .unwrap();

        let redacted = redacted_request_uri(&uri);

        assert_eq!(redacted, "/login/oauth2/code/test");
        assert!(!redacted.contains("authorization-code"));
        assert!(!redacted.contains("random-state"));
    }
}
