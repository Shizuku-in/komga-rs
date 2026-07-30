use super::*;

pub(super) async fn process_refresh_book_metadata_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<BookTaskPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid RefreshBookMetadata payload");
            return TaskOutcome::Completed;
        }
    };
    let book = match state.storage.find_book_by_id(&payload.book_id).await {
        Ok(Some(book)) if book.deleted_date.is_none() => book,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %payload.book_id, "could not load book for metadata refresh");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&book.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, library_id = %book.library_id, "could not load metadata settings");
            return TaskOutcome::Retry;
        }
    };
    let media = match state.storage.find_media_by_book_id(&book.id).await {
        Ok(Some(media)) if media.status == MediaStatus::Ready => media,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id = %book.id, "could not load media for metadata refresh");
            return TaskOutcome::Retry;
        }
    };
    let mut metadata = match state.storage.find_book_metadata(&book.id).await {
        Ok(Some(value)) => value,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, book_id=%book.id, "could not load book metadata");
            return TaskOutcome::Retry;
        }
    };
    let materialized = match state.mobi.materialize_book(&book, &media, &state.shutdown).await {
        Ok(materialized) => materialized,
        Err(crate::mobi::MaterializationError::Cancelled) => return TaskOutcome::Cancelled,
        Err(error) => {
            tracing::warn!(%error, book_id = %book.id, "could not materialize book for metadata refresh");
            return TaskOutcome::Completed;
        }
    };
    let extraction_book = materialized.book.clone();
    let extraction_media = media.clone();
    let append_volume = library.import_comic_info_series_append_volume;
    let comic_enabled = has_capability(
        &payload.capabilities,
        &[
            MetadataCapability::Title,
            MetadataCapability::Summary,
            MetadataCapability::Number,
            MetadataCapability::NumberSort,
            MetadataCapability::ReleaseDate,
            MetadataCapability::Authors,
            MetadataCapability::Tags,
            MetadataCapability::Isbn,
            MetadataCapability::Links,
            MetadataCapability::ReadLists,
            MetadataCapability::Collections,
        ],
    );
    let comic = if comic_enabled {
        tokio::task::spawn_blocking(move || {
            extract_comic_info_metadata(&extraction_book, &extraction_media, append_volume)
        })
        .await
    } else {
        Ok(Ok(None))
    };
    if let Ok(Ok(Some(value))) = comic {
        if library.import_comic_info_book {
            match metadata.apply_provider_patch(value.book) {
                Ok(v) => metadata = v,
                Err(error) => tracing::warn!(%error, book_id=%book.id, "invalid ComicInfo book metadata"),
            }
        }
        let collections = if library.import_comic_info_collection {
            value.containers.collections
        } else {
            Vec::new()
        };
        let read_lists = if library.import_comic_info_read_list {
            value
                .containers
                .read_lists
                .into_iter()
                .map(|v| (v.name, v.number))
                .collect()
        } else {
            Vec::new()
        };
        let container_update = match state
            .storage
            .append_comic_info_containers(&book.series_id, &book.id, &collections, &read_lists)
            .await
        {
            Ok(update) => update,
            Err(error) => {
                tracing::error!(%error, book_id=%book.id, "could not persist ComicInfo containers");
                return TaskOutcome::Retry;
            }
        };
        for event in &container_update.collection_events {
            state.publish_domain_event(if container_update.collections_added.contains(&event.container_id) {
                DomainEvent::CollectionAdded(event.clone())
            } else {
                DomainEvent::CollectionUpdated(event.clone())
            });
        }
        for event in &container_update.read_list_events {
            state.publish_domain_event(if container_update.read_lists_added.contains(&event.container_id) {
                DomainEvent::ReadListAdded(event.clone())
            } else {
                DomainEvent::ReadListUpdated(event.clone())
            });
        }
        let collection_ids = container_update.collection_ids.into_iter().collect::<Vec<_>>();
        let read_list_ids = container_update.read_list_ids.into_iter().collect::<Vec<_>>();
        if !state.sync_collections(&collection_ids).await || !state.sync_read_lists(&read_list_ids).await {
            state.recover_search_after_http_failure().await;
            return TaskOutcome::Retry;
        }
    } else if let Ok(Err(error)) = comic {
        tracing::warn!(%error, book_id=%book.id, "could not import ComicInfo metadata");
    }
    if library.import_epub_book
        && has_capability(
            &payload.capabilities,
            &[
                MetadataCapability::Title,
                MetadataCapability::Summary,
                MetadataCapability::Number,
                MetadataCapability::NumberSort,
                MetadataCapability::ReleaseDate,
                MetadataCapability::Authors,
                MetadataCapability::Tags,
                MetadataCapability::Isbn,
            ],
        )
    {
        let extraction_book = materialized.book.clone();
        let extraction_media = media.clone();
        let result =
            tokio::task::spawn_blocking(move || extract_epub_metadata(&extraction_book, &extraction_media)).await;
        if let Ok(Ok(Some(value))) = result {
            match metadata.apply_provider_patch(value.book) {
                Ok(v) => metadata = v,
                Err(error) => tracing::warn!(%error, book_id=%book.id, "invalid EPUB metadata"),
            }
        } else if let Ok(Err(error)) = result {
            tracing::warn!(%error, book_id=%book.id, "could not import EPUB metadata");
        }
    }
    if library.import_barcode_isbn && has_capability(&payload.capabilities, &[MetadataCapability::Isbn]) {
        let extraction_book = materialized.book.clone();
        let extraction_media = media.clone();
        match tokio::task::spawn_blocking(move || extract_isbn_barcode(&extraction_book, &extraction_media)).await {
            Ok(Some(isbn)) => match metadata.apply_provider_patch(BookMetadataPatch {
                isbn: MetadataUpdate::Set(isbn),
                ..Default::default()
            }) {
                Ok(value) => metadata = value,
                Err(error) => tracing::warn!(%error, book_id=%book.id, "invalid barcode ISBN metadata"),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, book_id=%book.id, "barcode ISBN extraction panicked"),
        }
    }
    if let Err(error) = state.storage.save_book_metadata(&metadata).await {
        tracing::error!(%error, book_id=%book.id, "could not persist book metadata");
        return TaskOutcome::Retry;
    }
    state.publish_domain_event(DomainEvent::BookUpdated(BookEvent::from(&book)));
    if !state.sync_books(std::slice::from_ref(&book.id)).await {
        state.recover_search_after_http_failure().await;
        return TaskOutcome::Retry;
    }
    if let Err(error) = state
        .enqueue_task(&TaskRecord::refresh_series_metadata(
            &book.series_id,
            task.priority.saturating_sub(1),
        ))
        .await
    {
        tracing::error!(%error, series_id=%book.series_id, "could not enqueue series metadata refresh");
        return TaskOutcome::Retry;
    }
    TaskOutcome::Completed
}

pub(super) async fn process_refresh_series_metadata_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<SeriesTaskPayload>(&task.payload) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(%error, "invalid RefreshSeriesMetadata payload");
            return TaskOutcome::Completed;
        }
    };
    let series = match state.storage.find_series_by_id(&payload.series_id).await {
        Ok(Some(v)) if v.deleted_date.is_none() => v,
        Ok(_) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, "could not load series");
            return TaskOutcome::Retry;
        }
    };
    let library = match state.storage.find_library_by_id(&series.library_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, "could not load library");
            return TaskOutcome::Retry;
        }
    };
    let mut target = match state.storage.find_series_metadata(&series.id).await {
        Ok(Some(v)) => v,
        Ok(None) => return TaskOutcome::Completed,
        Err(error) => {
            tracing::error!(%error, "could not load series metadata");
            return TaskOutcome::Retry;
        }
    };
    let previous_target = target.clone();
    let mut explicit_title_sort = false;
    let books = match state.storage.list_books_by_series_id(&series.id).await {
        Ok(v) => v
            .into_iter()
            .filter(|book| book.deleted_date.is_none())
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::error!(%error, "could not list series books");
            return TaskOutcome::Retry;
        }
    };
    let mut comic_patches = Vec::new();
    let mut epub_patches = Vec::new();
    for book in &books {
        let media = match state.storage.find_media_by_book_id(&book.id).await {
            Ok(Some(v)) if v.status == MediaStatus::Ready => v,
            Ok(_) => continue,
            Err(error) => {
                tracing::error!(%error, "could not load media");
                return TaskOutcome::Retry;
            }
        };
        let materialized = match state.mobi.materialize_book(book, &media, &state.shutdown).await {
            Ok(materialized) => materialized,
            Err(crate::mobi::MaterializationError::Cancelled) => return TaskOutcome::Cancelled,
            Err(error) => {
                tracing::warn!(%error, book_id=%book.id, "could not materialize book for series metadata");
                continue;
            }
        };
        let extraction_book = &materialized.book;
        if library.import_comic_info_series
            && has_capability(
                &payload.capabilities,
                &[
                    MetadataCapability::Title,
                    MetadataCapability::Summary,
                    MetadataCapability::ReadingDirection,
                    MetadataCapability::Publisher,
                    MetadataCapability::AgeRating,
                    MetadataCapability::Language,
                    MetadataCapability::Genres,
                    MetadataCapability::TotalBookCount,
                    MetadataCapability::Collections,
                ],
            )
        {
            match extract_comic_info_metadata(extraction_book, &media, library.import_comic_info_series_append_volume) {
                Ok(Some(v)) => comic_patches.push(v.series),
                Err(error) => tracing::warn!(%error, book_id=%book.id, "could not import ComicInfo series metadata"),
                _ => {}
            }
        }
        if library.import_epub_series
            && has_capability(
                &payload.capabilities,
                &[
                    MetadataCapability::Title,
                    MetadataCapability::Summary,
                    MetadataCapability::Publisher,
                    MetadataCapability::Language,
                    MetadataCapability::Genres,
                ],
            )
        {
            match extract_epub_metadata(extraction_book, &media) {
                Ok(Some(v)) => epub_patches.push(v.series),
                Err(error) => tracing::warn!(%error, book_id=%book.id, "could not import EPUB series metadata"),
                _ => {}
            }
        }
    }
    if let Some(patch) = aggregate_series_provider_patches(&comic_patches) {
        let patch_has_title_sort = patch.title_sort.is_some() && !target.title_sort_lock;
        if let Ok(updated) = target.apply_provider_patch(patch) {
            target = updated;
            explicit_title_sort |= patch_has_title_sort;
        }
    }
    if let Some(patch) = aggregate_series_provider_patches(&epub_patches) {
        let patch_has_title_sort = patch.title_sort.is_some() && !target.title_sort_lock;
        if let Ok(updated) = target.apply_provider_patch(patch) {
            target = updated;
            explicit_title_sort |= patch_has_title_sort;
        }
    }
    if series.oneshot
        && let Some(book) = books.first()
    {
        match state.storage.find_book_metadata(&book.id).await {
            Ok(Some(metadata)) => {
                let patch = oneshot_series_metadata_patch(&metadata);
                let patch_has_title_sort = patch.title_sort.is_some() && !target.title_sort_lock;
                match target.apply_provider_patch(patch) {
                    Ok(value) => {
                        target = value;
                        explicit_title_sort |= patch_has_title_sort;
                    }
                    Err(error) => tracing::warn!(%error, series_id=%series.id, "invalid one-shot series metadata"),
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, book_id=%book.id, "could not load one-shot book metadata");
                return TaskOutcome::Retry;
            }
        }
    }
    if library.import_mylar_series
        && has_capability(
            &payload.capabilities,
            &[
                MetadataCapability::Status,
                MetadataCapability::Title,
                MetadataCapability::Summary,
                MetadataCapability::Publisher,
                MetadataCapability::AgeRating,
                MetadataCapability::TotalBookCount,
            ],
        )
    {
        match extract_mylar_metadata(&series) {
            Ok(Some(patch)) => {
                let patch_has_title_sort = patch.title_sort.is_some() && !target.title_sort_lock;
                match target.apply_provider_patch(patch) {
                    Ok(v) => {
                        target = v;
                        explicit_title_sort |= patch_has_title_sort;
                    }
                    Err(error) => tracing::warn!(%error, "invalid Mylar metadata"),
                }
            }
            Err(error) => tracing::warn!(%error, series_id=%series.id, "could not import Mylar metadata"),
            _ => {}
        }
    }
    apply_series_title_sort(&previous_target, &mut target, explicit_title_sort);
    if let Err(error) = state.storage.save_series_metadata(&target).await {
        tracing::error!(%error, "could not persist series metadata");
        return TaskOutcome::Retry;
    }
    state.publish_domain_event(DomainEvent::SeriesUpdated(SeriesEvent::from(&series)));
    if !state.sync_series(std::slice::from_ref(&series.id)).await {
        state.recover_search_after_http_failure().await;
        return TaskOutcome::Retry;
    }
    if let Err(error) = state
        .enqueue_task(&TaskRecord::aggregate_series_metadata(
            &series.id,
            task.priority.saturating_sub(1),
        ))
        .await
    {
        tracing::error!(%error, "could not enqueue series aggregation");
        return TaskOutcome::Retry;
    }
    TaskOutcome::Completed
}

fn aggregate_series_provider_patches(patches: &[SeriesMetadataPatch]) -> Option<SeriesMetadataPatch> {
    if patches.is_empty() {
        return None;
    }
    let mut genres = std::collections::BTreeSet::new();
    let mut ages = Vec::new();
    let mut totals = Vec::new();
    for patch in patches {
        if let MetadataUpdate::Set(values) = &patch.genres {
            genres.extend(values.iter().cloned());
        }
        if let MetadataUpdate::Set(value) = patch.age_rating {
            ages.push(value);
        }
        if let MetadataUpdate::Set(value) = patch.total_book_count {
            totals.push(value);
        }
    }
    Some(SeriesMetadataPatch {
        status: mode(patches.iter().filter_map(|p| p.status)),
        title: mode(patches.iter().filter_map(|p| p.title.clone())),
        title_sort: mode(patches.iter().filter_map(|p| p.title_sort.clone())),
        summary: mode(patches.iter().filter_map(|p| p.summary.clone())),
        reading_direction: mode(patches.iter().filter_map(|p| match p.reading_direction {
            MetadataUpdate::Set(v) => Some(v),
            _ => None,
        }))
        .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        publisher: mode(patches.iter().filter_map(|p| p.publisher.clone())),
        age_rating: ages.into_iter().max().map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        language: mode(patches.iter().filter_map(|p| p.language.clone())),
        genres: if genres.is_empty() {
            MetadataUpdate::Keep
        } else {
            MetadataUpdate::Set(genres)
        },
        total_book_count: totals
            .into_iter()
            .max()
            .map_or(MetadataUpdate::Keep, MetadataUpdate::Set),
        ..Default::default()
    })
}

fn mode<T: Clone + PartialEq>(values: impl Iterator<Item = T>) -> Option<T> {
    let values = values.collect::<Vec<_>>();
    let mut best = None;
    let mut best_count = 0;
    for value in &values {
        let count = values.iter().filter(|candidate| *candidate == value).count();
        if count > best_count {
            best = Some(value.clone());
            best_count = count;
        }
    }
    best
}
