//! Tantivy rebuild and post-commit synchronization for application state.

use komga_core::{ContentRestrictions, PageRequest, User, UserRole};
use komga_search::{
    EntityKind, IndexState, SearchBatch, SearchMutation, book_document, collection_document, read_list_document,
    series_document,
};
use komga_storage::{StorageError, TaskRecord};

use crate::AppState;

pub(crate) fn all_entity_kinds() -> [EntityKind; 4] {
    [
        EntityKind::Book,
        EntityKind::Series,
        EntityKind::Collection,
        EntityKind::ReadList,
    ]
}

impl AppState {
    pub(crate) async fn rebuild_search(&self, kinds: &[EntityKind]) -> bool {
        let admin = search_admin();
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
}

fn search_admin() -> User {
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
