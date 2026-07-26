//! Top-level REST router and middleware composition.

use axum::{
    Router,
    body::Body,
    extract::DefaultBodyLimit,
    http::{Request, Uri},
    middleware,
    routing::{delete, get, patch, post},
};
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{
    AppState, auth, book_file_routes, container_routes, content_routes, filesystem_routes, history_routes, kobo,
    koreader_routes, library_routes, media_routes, metadata_routes, opds_router, page_hash_routes,
    read_list_file_routes, read_list_match_routes, read_progress_routes, referential_routes, routes, settings_routes,
    source_file_routes, sse_routes, thumbnail_routes, webpub_routes,
};

pub(crate) fn build(state: AppState) -> Router {
    let protected = protected_api()
        .merge(opds_router::protected())
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    public_api()
        .merge(opds_router::public())
        .merge(koreader_routes::router(&state))
        .merge(kobo::router(&state))
        .merge(protected)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http().make_span_with(make_http_span))
        .with_state(state)
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
        .route("/api/v2/users/me", get(routes::current_user))
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
            "/api/v1/series/{series_id}/file",
            axum::routing::delete(source_file_routes::delete_series_files),
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
        .route("/api/v1/oauth2/providers", get(settings_routes::oauth_providers))
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
}
