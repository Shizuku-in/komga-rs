//! Axum composition, authentication, and process configuration for Komga.
//!
//! [`build_app`] assembles the compatibility routes around an [`AppState`].
//! Domain policy and SQLite access remain in `komga-core` and `komga-storage`
//! respectively, which keeps HTTP concerns out of those crates.

#![warn(missing_docs)]

mod auth;
mod book_file_routes;
/// Command-line and environment configuration.
pub mod config;
mod container_routes;
mod content_access;
mod content_routes;
mod dto;
mod error;
mod filesystem_routes;
mod legacy_content_query;
mod library_routes;
mod media_routes;
mod metadata_routes;
mod opds_v1;
mod page_hash_routes;
mod pagination;
mod read_list_file_routes;
mod read_list_match_routes;
mod read_progress_routes;
mod referential_routes;
mod routes;
mod runtime;
mod search_query;
mod settings_routes;
mod sse_routes;
mod thumbnail_routes;
mod webpub_routes;

pub use auth::{REMEMBER_ME_COOKIE, SESSION_COOKIE, SESSION_HEADER};
pub use runtime::{AppRuntime, ShutdownHandle};

use std::{num::NonZeroUsize, sync::Arc};

use auth::SessionStore;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{delete, get, patch, post},
};
use komga_search::{
    EntityKind, IndexState, SearchBatch, SearchIndex, SearchMutation, book_document, collection_document,
    read_list_document, series_document,
};
use komga_storage::{Storage, StorageError, TaskRecord, TaskStorage};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

/// Shared dependencies used by the HTTP router.
///
/// Construct this state with [`AppState::new`] and pass it to [`build_app`].
/// Session state is intentionally process-local, matching the current Kotlin
/// Caffeine session repository: restarting the process expires active sessions.
#[derive(Clone)]
pub struct AppState {
    storage: Storage,
    search: SearchIndex,
    tasks: TaskStorage,
    sessions: SessionStore,
    claim_lock: Arc<Mutex<()>>,
    task_notify: Arc<Notify>,
    schedule_notify: Arc<Notify>,
    shutdown: CancellationToken,
    page_hashing: NonZeroUsize,
}

impl AppState {
    /// Creates application state around initialized main and task databases.
    ///
    /// # Errors
    ///
    /// Returns a storage error when server settings cannot be loaded.
    pub async fn new(storage: Storage, tasks: TaskStorage) -> Result<Self, StorageError> {
        let search = SearchIndex::in_memory().expect("in-memory search index creation must succeed");
        Self::with_search(storage, tasks, search).await
    }

    /// Creates application state with an explicitly configured index and
    /// synchronously rebuilds it for deterministic embedding and tests.
    pub async fn with_search(storage: Storage, tasks: TaskStorage, search: SearchIndex) -> Result<Self, StorageError> {
        let state = Self::build(storage, tasks, search).await?;
        state.rebuild_search(&all_entity_kinds()).await;
        Ok(state)
    }

    /// Creates application state around a file index without blocking startup.
    ///
    /// Empty or incompatible indexes enqueue one deduplicated priority-8 full
    /// rebuild task. A current index is immediately available to requests.
    pub async fn with_file_search(
        storage: Storage,
        tasks: TaskStorage,
        search: SearchIndex,
    ) -> Result<Self, StorageError> {
        let needs_rebuild = search.state() != IndexState::Current;
        let state = Self::build(storage, tasks, search).await?;
        if needs_rebuild {
            state.enqueue_task(&TaskRecord::rebuild_index(None, 8)).await?;
        }
        Ok(state)
    }

    async fn build(storage: Storage, tasks: TaskStorage, search: SearchIndex) -> Result<Self, StorageError> {
        let remember_me_duration_days = storage.remember_me_duration_days().await?;
        Ok(Self {
            storage,
            search,
            tasks,
            sessions: SessionStore::new(remember_me_duration_days),
            claim_lock: Arc::new(Mutex::new(())),
            task_notify: Arc::new(Notify::new()),
            schedule_notify: Arc::new(Notify::new()),
            shutdown: CancellationToken::new(),
            page_hashing: NonZeroUsize::new(3).expect("page hashing default is nonzero"),
        })
    }

    /// Overrides the number of pages hashed at each edge of ZIP books.
    #[must_use]
    pub fn with_page_hashing(mut self, page_hashing: NonZeroUsize) -> Self {
        self.page_hashing = page_hashing;
        self
    }

    pub(crate) async fn rebuild_search(&self, kinds: &[EntityKind]) -> bool {
        use komga_core::{ContentRestrictions, PageRequest, User, UserRole};
        use std::collections::BTreeSet;
        let admin = User {
            id: "search-index".into(),
            email: String::new(),
            password_hash: String::new(),
            roles: BTreeSet::from([UserRole::Admin]),
            shared_all_libraries: true,
            shared_library_ids: BTreeSet::new(),
            restrictions: ContentRestrictions::default(),
        };
        let kinds = if self.search.state() == IndexState::Current {
            kinds.to_vec()
        } else {
            all_entity_kinds().to_vec()
        };
        let (sender, receiver) = std::sync::mpsc::sync_channel::<SearchBatch>(2);
        let search = self.search.clone();
        let writer_kinds = kinds.clone();
        let writer = tokio::task::spawn_blocking(move || search.replace_stream(&writer_kinds, &receiver));
        let mut source_error = None;
        'entities: for kind in &kinds {
            let mut page_number = 0;
            loop {
                let page = PageRequest {
                    page: page_number,
                    size: 5_000,
                    ..PageRequest::default()
                };
                let documents = match kind {
                    EntityKind::Book => {
                        match self.storage.query_books(&Default::default(), &page, &admin, None).await {
                            Ok(values) => {
                                let mut documents = Vec::with_capacity(values.content.len());
                                for book in values.content {
                                    let series = if book.oneshot {
                                        match self.storage.find_series_dto_by_id(&book.series_id, &admin.id).await {
                                            Ok(series) => series,
                                            Err(error) => {
                                                source_error = Some(error.to_string());
                                                break 'entities;
                                            }
                                        }
                                    } else {
                                        None
                                    };
                                    documents.push(book_document(&book, series.as_ref()));
                                }
                                documents
                            }
                            Err(error) => {
                                source_error = Some(error.to_string());
                                break 'entities;
                            }
                        }
                    }
                    EntityKind::Series => match self
                        .storage
                        .query_series(&Default::default(), &page, &admin, None)
                        .await
                    {
                        Ok(values) => values.content.iter().map(series_document).collect(),
                        Err(error) => {
                            source_error = Some(error.to_string());
                            break 'entities;
                        }
                    },
                    EntityKind::Collection => {
                        match self.storage.list_collections(None, None, None, &page, &admin).await {
                            Ok(values) => values.content.iter().map(collection_document).collect(),
                            Err(error) => {
                                source_error = Some(error.to_string());
                                break 'entities;
                            }
                        }
                    }
                    EntityKind::ReadList => match self.storage.list_read_lists(None, None, None, &page, &admin).await {
                        Ok(values) => values.content.iter().map(read_list_document).collect(),
                        Err(error) => {
                            source_error = Some(error.to_string());
                            break 'entities;
                        }
                    },
                };
                let count = documents.len();
                if sender.send(Ok(documents)).is_err() {
                    source_error = Some("search writer stopped before the rebuild source completed".into());
                    break 'entities;
                }
                if count < 5_000 {
                    break;
                }
                page_number += 1;
            }
        }
        if let Some(error) = source_error {
            let _ = sender.send(Err(error));
        }
        drop(sender);
        match writer.await {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                tracing::error!(%error, "could not rebuild search index");
                false
            }
            Err(error) => {
                tracing::error!(%error, "search rebuild task panicked");
                false
            }
        }
    }

    pub(crate) async fn sync_books(&self, ids: &[String]) -> bool {
        let mutations = match self.book_mutations(ids).await {
            Ok(mutations) => mutations,
            Err(error) => {
                tracing::error!(%error, "could not load books for search synchronization");
                return false;
            }
        };
        self.apply_search_mutations(mutations).await
    }

    pub(crate) async fn sync_series(&self, ids: &[String]) -> bool {
        let mut mutations = Vec::new();
        let mut oneshot_books = std::collections::BTreeSet::new();
        for id in ids {
            match self.storage.find_series_dto_by_id(id, "search-index").await {
                Ok(Some(series)) => {
                    if series.oneshot {
                        match self.storage.list_books_by_series_id(id).await {
                            Ok(books) => oneshot_books.extend(books.into_iter().map(|book| book.id)),
                            Err(error) => {
                                tracing::error!(%error, %id, "could not load one-shot books for search synchronization");
                                return false;
                            }
                        }
                    }
                    mutations.push(SearchMutation::Upsert(series_document(&series)));
                }
                Ok(None) => mutations.push(SearchMutation::Delete {
                    kind: EntityKind::Series,
                    id: id.clone(),
                }),
                Err(error) => {
                    tracing::error!(%error, %id, "could not load series for search synchronization");
                    return false;
                }
            }
        }
        match self
            .book_mutations(&oneshot_books.into_iter().collect::<Vec<_>>())
            .await
        {
            Ok(book_mutations) => mutations.extend(book_mutations),
            Err(error) => {
                tracing::error!(%error, "could not load one-shot documents for search synchronization");
                return false;
            }
        }
        self.apply_search_mutations(mutations).await
    }

    pub(crate) async fn sync_collections(&self, ids: &[String]) -> bool {
        let admin = search_admin();
        let mut mutations = Vec::with_capacity(ids.len());
        for id in ids {
            match self.storage.find_collection(id, &admin).await {
                Ok(Some(collection)) => mutations.push(SearchMutation::Upsert(collection_document(&collection))),
                Ok(None) => mutations.push(SearchMutation::Delete {
                    kind: EntityKind::Collection,
                    id: id.clone(),
                }),
                Err(error) => {
                    tracing::error!(%error, %id, "could not load collection for search synchronization");
                    return false;
                }
            }
        }
        self.apply_search_mutations(mutations).await
    }

    pub(crate) async fn sync_read_lists(&self, ids: &[String]) -> bool {
        let admin = search_admin();
        let mut mutations = Vec::with_capacity(ids.len());
        for id in ids {
            match self.storage.find_read_list(id, &admin).await {
                Ok(Some(read_list)) => mutations.push(SearchMutation::Upsert(read_list_document(&read_list))),
                Ok(None) => mutations.push(SearchMutation::Delete {
                    kind: EntityKind::ReadList,
                    id: id.clone(),
                }),
                Err(error) => {
                    tracing::error!(%error, %id, "could not load read list for search synchronization");
                    return false;
                }
            }
        }
        self.apply_search_mutations(mutations).await
    }

    pub(crate) async fn delete_search_entities(&self, entities: Vec<(EntityKind, String)>) -> bool {
        self.apply_search_mutations(
            entities
                .into_iter()
                .map(|(kind, id)| SearchMutation::Delete { kind, id })
                .collect(),
        )
        .await
    }

    async fn book_mutations(&self, ids: &[String]) -> Result<Vec<SearchMutation>, StorageError> {
        let mut mutations = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(book) = self.storage.find_book_dto_by_id(id, "search-index").await? {
                let series = if book.oneshot {
                    self.storage
                        .find_series_dto_by_id(&book.series_id, "search-index")
                        .await?
                } else {
                    None
                };
                mutations.push(SearchMutation::Upsert(book_document(&book, series.as_ref())));
            } else {
                mutations.push(SearchMutation::Delete {
                    kind: EntityKind::Book,
                    id: id.clone(),
                });
            }
        }
        Ok(mutations)
    }

    async fn apply_search_mutations(&self, mutations: Vec<SearchMutation>) -> bool {
        if mutations.is_empty() {
            return true;
        }
        let search = self.search.clone();
        match tokio::task::spawn_blocking(move || search.mutate(mutations)).await {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                tracing::error!(%error, "could not update search index");
                false
            }
            Err(error) => {
                tracing::error!(%error, "search update task panicked");
                false
            }
        }
    }

    pub(crate) async fn recover_search_after_http_failure(&self) {
        if let Err(error) = self.enqueue_task(&TaskRecord::rebuild_index(None, 8)).await {
            tracing::error!(%error, "could not enqueue search index recovery rebuild");
        }
    }

    pub(crate) async fn enqueue_task(&self, task: &TaskRecord) -> Result<(), StorageError> {
        self.tasks.save(task).await?;
        self.task_notify.notify_waiters();
        Ok(())
    }

    pub(crate) async fn enqueue_missing_book_hashes(&self, library_id: &str) -> Result<(), StorageError> {
        for book_id in self.storage.list_books_without_file_hash(library_id).await? {
            self.enqueue_task(&TaskRecord::hash_book(&book_id, 0)).await?;
        }
        Ok(())
    }

    pub(crate) fn library_schedule_changed(&self) {
        self.schedule_notify.notify_one();
    }
}

fn all_entity_kinds() -> [EntityKind; 4] {
    [
        EntityKind::Book,
        EntityKind::Series,
        EntityKind::Collection,
        EntityKind::ReadList,
    ]
}

fn search_admin() -> komga_core::User {
    use komga_core::{ContentRestrictions, User, UserRole};
    User {
        id: "search-index".into(),
        email: String::new(),
        password_hash: String::new(),
        roles: std::collections::BTreeSet::from([UserRole::Admin]),
        shared_all_libraries: true,
        shared_library_ids: std::collections::BTreeSet::new(),
        restrictions: ContentRestrictions::default(),
    }
}

/// Builds the Komga HTTP router and its middleware stack.
///
/// The returned router includes public health and claim endpoints, authenticated
/// user/session endpoints, CORS handling, request IDs, a 2 MiB JSON body limit,
/// and request tracing. It can be served directly with `axum::serve` or cloned
/// for in-process contract tests.
pub fn build_app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v2/users/me", get(routes::current_user))
        .route("/api/v2/users/me/password", patch(routes::update_my_password))
        .route(
            "/api/v2/users/me/api-keys",
            get(routes::list_api_keys).post(routes::create_api_key),
        )
        .route("/api/v2/users/me/api-keys/{key_id}", delete(routes::delete_api_key))
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
            get(book_file_routes::download_book_file),
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
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

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
        .merge(protected)
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(CorsLayer::permissive())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
