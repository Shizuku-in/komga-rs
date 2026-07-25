//! OPDS route composition kept separate from protocol handlers.

use axum::{Router, routing::get};

use crate::{
    AppState, book_file_routes, media_routes, opds_v1, opds_v2, read_progress_routes, thumbnail_routes, webpub_routes,
};

pub(crate) fn protected() -> Router<AppState> {
    Router::new()
        .route("/opds/v1.2/catalog", get(opds_v1::catalog))
        .route("/opds/v1.2/search", get(opds_v1::search))
        .route("/opds/v1.2/ondeck", get(opds_v1::on_deck))
        .route("/opds/v1.2/keep-reading", get(opds_v1::keep_reading))
        .route("/opds/v1.2/series", get(opds_v1::all_series))
        .route("/opds/v1.2/series/latest", get(opds_v1::latest_series))
        .route("/opds/v1.2/books/latest", get(opds_v1::latest_books))
        .route("/opds/v1.2/libraries", get(opds_v1::libraries))
        .route("/opds/v1.2/collections", get(opds_v1::collections))
        .route("/opds/v1.2/readlists", get(opds_v1::read_lists))
        .route("/opds/v1.2/publishers", get(opds_v1::publishers))
        .route("/opds/v1.2/series/{id}", get(opds_v1::series))
        .route("/opds/v1.2/libraries/{id}", get(opds_v1::library))
        .route("/opds/v1.2/collections/{id}", get(opds_v1::collection))
        .route("/opds/v1.2/readlists/{id}", get(opds_v1::read_list))
        .route(
            "/opds/v1.2/books/{book_id}/thumbnail",
            get(thumbnail_routes::get_book_thumbnail),
        )
        .route(
            "/opds/v1.2/books/{book_id}/thumbnail/small",
            get(opds_v1::small_thumbnail),
        )
        .route(
            "/opds/v1.2/books/{book_id}/pages/{page_number}",
            get(media_routes::get_opds_v1_page),
        )
        .route(
            "/opds/v1.2/books/{book_id}/file/{*file_name}",
            get(book_file_routes::download_book_file_with_name),
        )
        .route("/opds/v2/catalog", get(opds_v2::catalog))
        .route("/opds/v2/libraries", get(opds_v2::libraries))
        .route("/opds/v2/libraries/{id}", get(opds_v2::library))
        .route("/opds/v2/libraries/keep-reading", get(opds_v2::keep_reading))
        .route(
            "/opds/v2/libraries/{id}/keep-reading",
            get(opds_v2::library_keep_reading),
        )
        .route("/opds/v2/libraries/on-deck", get(opds_v2::on_deck))
        .route("/opds/v2/libraries/{id}/on-deck", get(opds_v2::library_on_deck))
        .route("/opds/v2/libraries/books/latest", get(opds_v2::latest_books))
        .route(
            "/opds/v2/libraries/{id}/books/latest",
            get(opds_v2::library_latest_books),
        )
        .route("/opds/v2/libraries/series/latest", get(opds_v2::latest_series))
        .route(
            "/opds/v2/libraries/{id}/series/latest",
            get(opds_v2::library_latest_series),
        )
        .route("/opds/v2/libraries/browse", get(opds_v2::browse))
        .route("/opds/v2/libraries/{id}/browse", get(opds_v2::library_browse))
        .route("/opds/v2/libraries/collections", get(opds_v2::collections))
        .route("/opds/v2/libraries/{id}/collections", get(opds_v2::library_collections))
        .route("/opds/v2/libraries/readlists", get(opds_v2::read_lists))
        .route("/opds/v2/libraries/{id}/readlists", get(opds_v2::library_read_lists))
        .route("/opds/v2/collections/{id}", get(opds_v2::collection))
        .route("/opds/v2/readlists/{id}", get(opds_v2::read_list))
        .route("/opds/v2/series/{id}", get(opds_v2::series))
        .route("/opds/v2/search", get(opds_v2::search))
        .route(
            "/opds/v2/books/{book_id}/thumbnail",
            get(thumbnail_routes::get_book_thumbnail),
        )
        .route(
            "/opds/v2/books/{book_id}/pages/{page_number}",
            get(media_routes::get_page),
        )
        .route(
            "/opds/v2/books/{book_id}/pages/{page_number}/raw",
            get(media_routes::get_raw_page),
        )
        .route(
            "/opds/v2/books/{book_id}/file",
            get(book_file_routes::download_book_file),
        )
        .route(
            "/opds/v2/books/{book_id}/file/{*file_name}",
            get(book_file_routes::download_book_file_with_name),
        )
        .route(
            "/opds/v2/books/{book_id}/progression",
            get(read_progress_routes::get_progression).put(read_progress_routes::update_progression),
        )
        .route(
            "/opds/v2/books/{book_id}/manifest",
            get(webpub_routes::get_opds_manifest),
        )
        .route(
            "/opds/v2/books/{book_id}/manifest/divina",
            get(webpub_routes::get_opds_divina_manifest),
        )
        .route(
            "/opds/v2/books/{book_id}/manifest/pdf",
            get(webpub_routes::get_opds_pdf_manifest),
        )
        .route(
            "/opds/v2/books/{book_id}/manifest/epub",
            get(webpub_routes::get_opds_epub_manifest),
        )
}

pub(crate) fn public() -> Router<AppState> {
    Router::new().route("/opds/v2/auth", get(opds_v2::auth_document)).route(
        "/opds/v2/books/{book_id}/resource/{*resource}",
        get(media_routes::get_epub_resource),
    )
}
