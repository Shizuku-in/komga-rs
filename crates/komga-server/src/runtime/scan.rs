use super::*;

pub(super) async fn process_scan_task(state: &AppState, task: &TaskRecord) -> TaskOutcome {
    let payload = match serde_json::from_str::<ScanLibraryPayload>(&task.payload) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(%error, task_id = %task.id, "invalid ScanLibrary payload");
            return TaskOutcome::Completed;
        }
    };
    let library = match state.storage.find_library_by_id(&payload.library_id).await {
        Ok(Some(library)) => library,
        Ok(None) => {
            tracing::warn!(library_id = %payload.library_id, "scan task references a missing library");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load library for scan");
            return TaskOutcome::Retry;
        }
    };
    let deferred_page_removal_recovery = if payload.scan_deep {
        match super::duplicate_pages::recover_library_page_removal_artifacts(state, &library, "deep-scan-before", true)
            .await
        {
            Ok(deferred) => deferred,
            Err(outcome) => return outcome,
        }
    } else {
        false
    };
    let empty_trash_after_scan = library.empty_trash_after_scan;
    let hash_files = library.hash_files;
    let hash_koreader = library.hash_koreader;
    let repair_extensions = library.repair_extensions;
    let convert_to_cbz = library.convert_to_cbz;
    let cancellation = state.shutdown.clone();
    let scanned_library = library.clone();
    let scan =
        tokio::task::spawn_blocking(move || scan_library_files(&scanned_library, || cancellation.is_cancelled())).await;
    let mut scan = match scan {
        Ok(Ok(scan)) => scan,
        Ok(Err(ScanError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(ScanError::RootUnavailable(path))) => {
            match state.storage.set_library_unavailable(&payload.library_id, true).await {
                Ok(true) => state.publish_domain_event(DomainEvent::LibraryUpdated(LibraryEvent {
                    library_id: payload.library_id.clone(),
                })),
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(%error, library_id = %payload.library_id, "could not mark library unavailable");
                }
            }
            tracing::error!(library_id = %payload.library_id, root = %path.display(), "library scan failed");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "filesystem scanner panicked");
            return TaskOutcome::Retry;
        }
    };
    let existing_books = match state
        .storage
        .list_books_for_scan_reconciliation(&payload.library_id)
        .await
    {
        Ok(books) => books,
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not load books and restore candidates for scan hash reconciliation");
            return TaskOutcome::Retry;
        }
    };
    let cancellation = state.shutdown.clone();
    let reconciled = tokio::task::spawn_blocking(move || {
        reconcile_scan_hashes(&mut scan, &existing_books, || cancellation.is_cancelled()).map(|()| scan)
    })
    .await;
    let scan = match reconciled {
        Ok(Ok(scan)) => scan,
        Ok(Err(FileHashError::Cancelled)) => return TaskOutcome::Cancelled,
        Ok(Err(FileHashError::SourceChanged(error))) => {
            tracing::warn!(library_id = %payload.library_id, source = %error, "book changed during scan hash reconciliation");
            return TaskOutcome::Retry;
        }
        Ok(Err(error)) => {
            tracing::error!(%error, library_id = %payload.library_id, "scan hash reconciliation failed");
            return TaskOutcome::Completed;
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "scan hash reconciler panicked");
            return TaskOutcome::Retry;
        }
    };
    if state.shutdown.is_cancelled() {
        return TaskOutcome::Cancelled;
    }
    match state
        .storage
        .apply_library_scan(&payload.library_id, &scan, payload.scan_deep)
        .await
    {
        Ok(stats) => {
            tracing::info!(library_id = %payload.library_id, ?stats, "library scan completed");
            if deferred_page_removal_recovery
                && let Err(outcome) = super::duplicate_pages::recover_library_page_removal_artifacts(
                    state,
                    &library,
                    "deep-scan-after",
                    false,
                )
                .await
            {
                return outcome;
            }
            if stats.library_became_available {
                state.publish_domain_event(DomainEvent::LibraryUpdated(LibraryEvent {
                    library_id: payload.library_id.clone(),
                }));
            }
            for event in &stats.series_added_events {
                state.publish_domain_event(DomainEvent::SeriesAdded(event.clone()));
            }
            for event in &stats.series_updated_events {
                state.publish_domain_event(DomainEvent::SeriesUpdated(event.clone()));
            }
            for event in &stats.book_added_events {
                state.publish_domain_event(DomainEvent::BookAdded(event.clone()));
            }
            for event in &stats.book_updated_events {
                state.publish_domain_event(DomainEvent::BookUpdated(event.clone()));
            }
            for event in &stats.book_deleted_events {
                state.publish_domain_event(DomainEvent::BookDeleted(event.clone()));
            }
            for event in &stats.series_deleted_events {
                state.publish_domain_event(DomainEvent::SeriesDeleted(event.clone()));
            }
            for event in &stats.collection_updated_events {
                state.publish_domain_event(DomainEvent::CollectionUpdated(event.clone()));
            }
            for event in &stats.read_list_updated_events {
                state.publish_domain_event(DomainEvent::ReadListUpdated(event.clone()));
            }
            let book_ids = stats.book_ids.iter().cloned().collect::<Vec<_>>();
            let series_ids = stats.series_ids.iter().cloned().collect::<Vec<_>>();
            let collection_ids = stats.collection_ids.iter().cloned().collect::<Vec<_>>();
            let read_list_ids = stats.read_list_ids.iter().cloned().collect::<Vec<_>>();
            if !state.sync_series(&series_ids).await
                || !state.sync_books(&book_ids).await
                || !state.sync_collections(&collection_ids).await
                || !state.sync_read_lists(&read_list_ids).await
            {
                state.recover_search_after_http_failure().await;
                return TaskOutcome::Retry;
            }
            let mut book_metadata_refreshes = std::collections::BTreeMap::new();
            for event in stats.book_title_refresh_events {
                let book_id = event.book_id.clone();
                book_metadata_refreshes
                    .entry(book_id)
                    .or_insert((event, false, false))
                    .1 = true;
            }
            for event in stats.book_numbering_refresh_events {
                let book_id = event.book_id.clone();
                book_metadata_refreshes
                    .entry(book_id)
                    .or_insert((event, false, false))
                    .2 = true;
            }
            for (_, (event, refresh_title, refresh_numbering)) in book_metadata_refreshes {
                let refresh = match (refresh_title, refresh_numbering) {
                    (true, true) => {
                        TaskRecord::refresh_book_title_and_numbering(&event.book_id, &event.series_id, task.priority)
                    }
                    (true, false) => TaskRecord::refresh_book_title(&event.book_id, &event.series_id, task.priority),
                    (false, true) => {
                        TaskRecord::refresh_book_numbering(&event.book_id, &event.series_id, task.priority)
                    }
                    (false, false) => continue,
                };
                if let Err(error) = state.enqueue_task(&refresh).await {
                    tracing::error!(%error, book_id = %event.book_id, "could not enqueue restored book metadata refresh");
                    return TaskOutcome::Retry;
                }
            }
            let mut series_metadata_refresh_ids = stats.series_metadata_refresh_ids;
            series_metadata_refresh_ids.extend(stats.sidecars_changed);
            for series_id in series_metadata_refresh_ids {
                if let Err(error) = state
                    .enqueue_task(&TaskRecord::refresh_series_metadata(&series_id, task.priority))
                    .await
                {
                    tracing::error!(%error, %series_id, "could not enqueue sidecar metadata refresh");
                    return TaskOutcome::Retry;
                }
            }
            for book_id in stats.book_artwork_changed {
                if let Err(error) = state
                    .enqueue_task(&TaskRecord::refresh_book_local_artwork(&book_id, 4))
                    .await
                {
                    tracing::error!(%error, %book_id, "could not enqueue local artwork refresh");
                    return TaskOutcome::Retry;
                }
            }
            for series_id in stats.series_artwork_changed {
                if let Err(error) = state
                    .enqueue_task(&TaskRecord::refresh_series_local_artwork(&series_id, 4))
                    .await
                {
                    tracing::error!(%error, %series_id, "could not enqueue local series artwork refresh");
                    return TaskOutcome::Retry;
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not persist library scan");
            return TaskOutcome::Retry;
        }
    }
    if empty_trash_after_scan {
        if perform_empty_trash(state, &payload.library_id).await == TaskOutcome::Retry {
            return TaskOutcome::Retry;
        }
    } else {
        match state.storage.cleanup_empty_container_ids().await {
            Ok((collections, read_lists)) => {
                for id in &collections {
                    state.publish_domain_event(DomainEvent::CollectionDeleted(ContainerEvent {
                        container_id: id.clone(),
                        item_ids: Vec::new(),
                    }));
                }
                for id in &read_lists {
                    state.publish_domain_event(DomainEvent::ReadListDeleted(ContainerEvent {
                        container_id: id.clone(),
                        item_ids: Vec::new(),
                    }));
                }
                let deleted = collections
                    .into_iter()
                    .map(|id| (komga_search::EntityKind::Collection, id))
                    .chain(
                        read_lists
                            .into_iter()
                            .map(|id| (komga_search::EntityKind::ReadList, id)),
                    )
                    .collect();
                if !state.delete_search_entities(deleted).await {
                    state.recover_search_after_http_failure().await;
                    return TaskOutcome::Retry;
                }
            }
            Err(error) => {
                tracing::error!(%error, library_id = %payload.library_id, "could not clean empty collections and read lists");
                return TaskOutcome::Retry;
            }
        }
    }
    let targets = match state.storage.list_book_analysis_targets(&payload.library_id).await {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!(%error, library_id = %payload.library_id, "could not list books requiring analysis");
            return TaskOutcome::Retry;
        }
    };
    for (book_id, series_id) in targets {
        if let Err(error) = state
            .enqueue_task(&TaskRecord::analyze_book(&book_id, &series_id, task.priority))
            .await
        {
            tracing::error!(%error, %book_id, "could not enqueue book analysis");
            return TaskOutcome::Retry;
        }
    }
    if repair_extensions && let Err(error) = enqueue_extension_repairs(state, &payload.library_id, 2).await {
        tracing::error!(%error, library_id = %payload.library_id, "could not enqueue extension repairs");
        return TaskOutcome::Retry;
    }
    if convert_to_cbz
        && let Err(error) = state
            .enqueue_task(&TaskRecord::find_books_to_convert(&payload.library_id, 0))
            .await
    {
        tracing::error!(%error, library_id = %payload.library_id, "could not enqueue conversion discovery");
        return TaskOutcome::Retry;
    }
    if payload.scan_deep {
        let targets = match state.storage.list_ready_books_by_library_id(&payload.library_id).await {
            Ok(targets) => targets,
            Err(error) => {
                tracing::error!(%error, library_id = %payload.library_id, "could not list ready books for metadata refresh");
                return TaskOutcome::Retry;
            }
        };
        for (book_id, series_id) in targets {
            if let Err(error) = state
                .enqueue_task(&TaskRecord::refresh_book_metadata(&book_id, &series_id, task.priority))
                .await
            {
                tracing::error!(%error, %book_id, "could not enqueue book metadata refresh");
                return TaskOutcome::Retry;
            }
        }
    }
    if hash_files && let Err(error) = state.enqueue_missing_book_hashes(&payload.library_id).await {
        tracing::error!(%error, library_id = %payload.library_id, "could not enqueue missing book hashes");
        return TaskOutcome::Retry;
    }
    if hash_koreader && let Err(error) = state.enqueue_missing_koreader_hashes(&payload.library_id).await {
        tracing::error!(%error, library_id = %payload.library_id, "could not enqueue missing KOReader hashes");
        return TaskOutcome::Retry;
    }
    if let Err(error) = state
        .enqueue_task(&TaskRecord::find_books_with_missing_page_hash(&payload.library_id, 0))
        .await
    {
        tracing::error!(%error, library_id = %payload.library_id, "could not enqueue page hash discovery");
        return TaskOutcome::Retry;
    }
    if let Err(error) = state
        .enqueue_task(&TaskRecord::find_duplicate_pages_to_delete(&payload.library_id, 0))
        .await
    {
        tracing::error!(%error, library_id = %payload.library_id, "could not enqueue duplicate-page discovery");
        return TaskOutcome::Retry;
    }
    state.publish_domain_event(DomainEvent::LibraryScanned(LibraryEvent {
        library_id: payload.library_id,
    }));
    TaskOutcome::Completed
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{Library, ReadListCreation, ScanInterval, User};
    use komga_search::EntityKind;
    use komga_storage::{Storage, TaskStorage};
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn scans_enqueue_book_and_series_artwork_at_kotlin_default_priority() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        fs::write(series.join("Book.cbz"), b"book").unwrap();
        fs::write(series.join("Book.jpg"), b"artwork").unwrap();
        fs::write(series.join("cover.jpg"), b"artwork").unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        library.hash_files = false;
        library.hash_koreader = false;
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        storage.create_library(&library).await.unwrap();
        let state = AppState::new(storage, tasks.clone()).await.unwrap();

        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, false, 9)).await,
            TaskOutcome::Completed
        );
        let artwork = tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .filter(|task| {
                matches!(
                    task.simple_type.as_str(),
                    "RefreshBookLocalArtwork" | "RefreshSeriesLocalArtwork"
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(artwork.len(), 2);
        assert!(artwork.iter().all(|task| task.priority == 4));
    }

    #[tokio::test]
    async fn deep_scan_defers_legacy_temporary_until_the_new_snapshot_is_committed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let source = series.join("Book.cbz");
        fs::write(&source, b"old").unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        storage.create_library(&library).await.unwrap();
        let state = AppState::new(storage.clone(), tasks).await.unwrap();
        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, false, 0)).await,
            TaskOutcome::Completed
        );
        let book = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        let artifact = series.join(format!(".komga-page-remove-{}.tmp", book.id));
        fs::write(&artifact, b"orphan-temporary").unwrap();
        fs::write(&source, b"new-source-with-a-different-size").unwrap();

        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, true, 0)).await,
            TaskOutcome::Completed
        );

        let saved = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(
            saved.file_size,
            i64::try_from(fs::metadata(&source).unwrap().len()).unwrap()
        );
        assert!(!artifact.exists());
    }

    #[tokio::test]
    async fn corrupt_page_removal_journal_blocks_deep_scan_database_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let source = series.join("Book.cbz");
        fs::write(&source, b"old").unwrap();
        let mut library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        storage.create_library(&library).await.unwrap();
        let state = AppState::new(storage.clone(), tasks).await.unwrap();
        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, false, 0)).await,
            TaskOutcome::Completed
        );
        let book = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        let journal = series.join(format!(".komga-page-remove-{}.json", book.id));
        fs::write(&journal, b"{").unwrap();
        fs::write(&source, b"new-source-with-a-different-size").unwrap();

        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, true, 0)).await,
            TaskOutcome::Retry
        );

        let saved = storage.find_book_by_id(&book.id).await.unwrap().unwrap();
        assert_eq!(saved.file_size, book.file_size);
        assert!(journal.exists());
    }

    #[tokio::test]
    async fn scan_restores_renamed_book_relations_before_automatic_trash_cleanup() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series = root.join("Series");
        fs::create_dir_all(&series).unwrap();
        let source = series.join("Original.cbz");
        fs::write(&source, b"stable book content").unwrap();

        let mut library = Library::new("Rename restore".into(), Url::from_directory_path(&root).unwrap());
        library.scan_interval = ScanInterval::Disabled;
        library.empty_trash_after_scan = true;
        library.hash_files = false;
        library.hash_koreader = false;
        let storage = Storage::connect_memory().await.unwrap();
        let tasks = TaskStorage::connect_memory().await.unwrap();
        storage.create_library(&library).await.unwrap();
        let state = AppState::new(storage.clone(), tasks).await.unwrap();

        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, false, 0)).await,
            TaskOutcome::Completed
        );
        let original = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        let expected_hash = hash_file(&source, || false).unwrap();
        assert!(
            storage
                .update_book_file_hash_if_current(&original, &expected_hash)
                .await
                .unwrap()
        );
        let mut original_metadata = storage.find_book_metadata(&original.id).await.unwrap().unwrap();
        original_metadata.number = "99".into();
        original_metadata.number_sort = 99.0;
        storage.save_book_metadata(&original_metadata).await.unwrap();
        assert_eq!(
            state
                .search
                .search(EntityKind::Book, "Original")
                .into_iter()
                .map(|hit| hit.id)
                .collect::<Vec<_>>(),
            std::slice::from_ref(&original.id)
        );

        let reader = User::new("rename-reader@example.org".into(), "unused".into());
        storage.create_user(&reader).await.unwrap();
        storage
            .create_read_list(
                "rename-read-list",
                &ReadListCreation {
                    name: "Rename continuity".into(),
                    summary: String::new(),
                    ordered: true,
                    book_ids: vec![original.id.clone()],
                },
            )
            .await
            .unwrap();

        let mut events = state.subscribe_domain_events();
        let renamed = series.join("Renamed.cbz");
        fs::rename(&source, &renamed).unwrap();
        assert_eq!(
            process_scan_task(&state, &TaskRecord::scan_library(&library.id, false, 0)).await,
            TaskOutcome::Completed
        );

        let active = storage.list_active_books_by_library_id(&library.id).await.unwrap();
        assert_eq!(active.len(), 1);
        let restored = &active[0];
        assert_ne!(restored.id, original.id);
        assert_eq!(restored.name, "Renamed");
        assert_eq!(restored.file_hash, expected_hash);
        assert!(storage.find_book_by_id(&original.id).await.unwrap().is_none());
        let read_list = storage
            .find_read_list("rename-read-list", &reader)
            .await
            .unwrap()
            .expect("automatic trash cleanup must retain the restored read list");
        assert_eq!(read_list.book_ids, vec![restored.id.clone()]);

        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            matches!(
                event.as_ref(),
                DomainEvent::BookAdded(event)
                    if event.book_id == restored.id && event.series_id == restored.series_id
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event.as_ref(),
                DomainEvent::BookDeleted(event) if event.book_id == original.id
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event.as_ref(),
                DomainEvent::ReadListUpdated(event)
                    if event.container_id == "rename-read-list"
                        && event.item_ids == [restored.id.clone()]
            )
        }));

        assert!(
            state
                .search
                .search(EntityKind::Book, "Original")
                .into_iter()
                .all(|hit| hit.id != original.id)
        );
        assert_eq!(
            state
                .search
                .search(EntityKind::Book, "Renamed")
                .into_iter()
                .map(|hit| hit.id)
                .collect::<Vec<_>>(),
            std::slice::from_ref(&restored.id)
        );
        assert_eq!(
            state
                .search
                .search(EntityKind::ReadList, "Rename continuity")
                .into_iter()
                .map(|hit| hit.id)
                .collect::<Vec<_>>(),
            ["rename-read-list"]
        );
        let metadata_task = state
            .tasks
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|task| task.id == format!("REFRESH_BOOK_METADATA_{}", restored.id))
            .expect("restored book metadata refresh must be durable");
        let payload = serde_json::from_str::<serde_json::Value>(&metadata_task.payload).unwrap();
        assert_eq!(
            payload["capabilities"],
            serde_json::json!(["TITLE", "NUMBER", "NUMBER_SORT"])
        );
    }
}
