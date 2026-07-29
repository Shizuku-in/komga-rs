//! Permanent content deletion and empty-container housekeeping.

use komga_core::compare_book_names;
use sqlx::{Row, Sqlite, Transaction};

use crate::{DeleteLibraryResult, Result, Storage};

/// One book whose embedded numbering should be refreshed after reordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberingRefreshTarget {
    /// Book identifier.
    pub book_id: String,
    /// Owning series identifier used as the task serialization group.
    pub series_id: String,
}

/// Counts and follow-up work produced by emptying one library's trash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmptyTrashResult {
    /// Permanently removed series identifiers.
    pub series_ids: Vec<String>,
    /// Permanently removed book identifiers.
    pub book_ids: Vec<String>,
    /// Series kept after book deletion and therefore rebuilt or reordered.
    pub affected_series_ids: Vec<String>,
    /// Permanently removed collection identifiers.
    pub collection_ids: Vec<String>,
    /// Permanently removed read-list identifiers.
    pub read_list_ids: Vec<String>,
    /// Permanently removed series rows.
    pub series_deleted: u64,
    /// Permanently removed book rows.
    pub books_deleted: u64,
    /// Empty collections removed according to server settings.
    pub collections_deleted: u64,
    /// Empty read lists removed according to server settings.
    pub read_lists_deleted: u64,
    /// Books whose unlocked numbering changed during natural reordering.
    pub numbering_refreshes: Vec<NumberingRefreshTarget>,
}

impl Storage {
    /// Permanently deletes soft-deleted content in one library.
    ///
    /// All main-database changes are committed atomically. Source files,
    /// historical events, sync snapshots, and page hashes are not touched.
    pub async fn empty_trash(&self, library_id: &str) -> Result<Option<EmptyTrashResult>> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM LIBRARY WHERE ID=?)")
            .bind(library_id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            tx.rollback().await?;
            return Ok(None);
        }

        let affected_series = sqlx::query_scalar::<_, String>(
            r#"SELECT DISTINCT B.SERIES_ID FROM BOOK B
               JOIN SERIES S ON S.ID=B.SERIES_ID
               WHERE B.LIBRARY_ID=? AND B.DELETED_DATE IS NOT NULL AND S.DELETED_DATE IS NULL"#,
        )
        .bind(library_id)
        .fetch_all(&mut *tx)
        .await?;
        let series_ids =
            sqlx::query_scalar::<_, String>("SELECT ID FROM SERIES WHERE LIBRARY_ID=? AND DELETED_DATE IS NOT NULL")
                .bind(library_id)
                .fetch_all(&mut *tx)
                .await?;
        let book_ids = sqlx::query_scalar::<_, String>(
            r#"SELECT ID FROM BOOK WHERE LIBRARY_ID=? AND
               (DELETED_DATE IS NOT NULL OR SERIES_ID IN
                 (SELECT ID FROM SERIES WHERE LIBRARY_ID=? AND DELETED_DATE IS NOT NULL))"#,
        )
        .bind(library_id)
        .bind(library_id)
        .fetch_all(&mut *tx)
        .await?;

        let book_subquery = r#"SELECT ID FROM BOOK WHERE LIBRARY_ID=? AND
            (DELETED_DATE IS NOT NULL OR SERIES_ID IN
              (SELECT ID FROM SERIES WHERE LIBRARY_ID=? AND DELETED_DATE IS NOT NULL))"#;
        delete_book_dependents(&mut tx, book_subquery, &[library_id, library_id]).await?;
        sqlx::query(&format!("DELETE FROM BOOK WHERE ID IN ({book_subquery})"))
            .bind(library_id)
            .bind(library_id)
            .execute(&mut *tx)
            .await?;

        let series_subquery = "SELECT ID FROM SERIES WHERE LIBRARY_ID=? AND DELETED_DATE IS NOT NULL";
        delete_series_dependents(&mut tx, series_subquery, &[library_id]).await?;
        sqlx::query(&format!("DELETE FROM SERIES WHERE ID IN ({series_subquery})"))
            .bind(library_id)
            .execute(&mut *tx)
            .await?;

        let mut numbering_refreshes = Vec::new();
        for series_id in &affected_series {
            reorder_series(&mut tx, series_id, &mut numbering_refreshes).await?;
        }
        let (collection_ids, read_list_ids) = cleanup_empty_containers(&mut tx).await?;
        tx.commit().await?;
        Ok(Some(EmptyTrashResult {
            series_deleted: series_ids.len() as u64,
            books_deleted: book_ids.len() as u64,
            collections_deleted: collection_ids.len() as u64,
            read_lists_deleted: read_list_ids.len() as u64,
            series_ids,
            book_ids,
            affected_series_ids: affected_series,
            collection_ids,
            read_list_ids,
            numbering_refreshes,
        }))
    }

    /// Deletes empty collections and read lists according to server settings.
    pub async fn cleanup_empty_containers(&self) -> Result<(u64, u64)> {
        let (collections, read_lists) = self.cleanup_empty_container_ids().await?;
        Ok((collections.len() as u64, read_lists.len() as u64))
    }

    /// Deletes empty collections and read lists and returns their identifiers.
    pub async fn cleanup_empty_container_ids(&self) -> Result<(Vec<String>, Vec<String>)> {
        let mut tx = self.pool.begin().await?;
        let (collections, read_lists) = cleanup_empty_containers(&mut tx).await?;
        tx.commit().await?;
        Ok((collections, read_lists))
    }

    /// Permanently deletes a library and all database-owned content.
    ///
    /// Source files and global containers are preserved.
    pub async fn delete_library(&self, library_id: &str) -> Result<DeleteLibraryResult> {
        let mut tx = self.pool.begin().await?;
        let exists = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM LIBRARY WHERE ID=?)")
            .bind(library_id)
            .fetch_one(&mut *tx)
            .await?;
        if !exists {
            tx.rollback().await?;
            return Ok(DeleteLibraryResult::NotFound);
        }

        let book_ids = sqlx::query_scalar::<_, String>("SELECT ID FROM BOOK WHERE LIBRARY_ID=?")
            .bind(library_id)
            .fetch_all(&mut *tx)
            .await?;
        let series_ids = sqlx::query_scalar::<_, String>("SELECT ID FROM SERIES WHERE LIBRARY_ID=?")
            .bind(library_id)
            .fetch_all(&mut *tx)
            .await?;
        let book_subquery = "SELECT ID FROM BOOK WHERE LIBRARY_ID=?";
        delete_book_dependents(&mut tx, book_subquery, &[library_id]).await?;
        sqlx::query(&format!("DELETE FROM BOOK WHERE ID IN ({book_subquery})"))
            .bind(library_id)
            .execute(&mut *tx)
            .await?;
        let series_subquery = "SELECT ID FROM SERIES WHERE LIBRARY_ID=?";
        delete_series_dependents(&mut tx, series_subquery, &[library_id]).await?;
        sqlx::query(&format!("DELETE FROM SERIES WHERE ID IN ({series_subquery})"))
            .bind(library_id)
            .execute(&mut *tx)
            .await?;

        for table in ["SIDECAR", "LIBRARY_EXCLUSIONS", "USER_LIBRARY_SHARING"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE LIBRARY_ID=?"))
                .bind(library_id)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM LIBRARY WHERE ID=?")
            .bind(library_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(DeleteLibraryResult::Deleted { book_ids, series_ids })
    }
}

pub(crate) async fn delete_book_dependents(
    tx: &mut Transaction<'_, Sqlite>,
    book_subquery: &str,
    bindings: &[&str],
) -> Result<()> {
    for table in [
        "READ_PROGRESS",
        "READLIST_BOOK",
        "MEDIA_PAGE",
        "MEDIA_FILE",
        "THUMBNAIL_BOOK",
        "BOOK_METADATA_AUTHOR",
        "BOOK_METADATA_TAG",
        "BOOK_METADATA_LINK",
        "MEDIA",
        "BOOK_METADATA",
    ] {
        let statement = format!("DELETE FROM {table} WHERE BOOK_ID IN ({book_subquery})");
        let mut query = sqlx::query(&statement);
        for value in bindings {
            query = query.bind(*value);
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}

pub(crate) async fn delete_series_dependents(
    tx: &mut Transaction<'_, Sqlite>,
    series_subquery: &str,
    bindings: &[&str],
) -> Result<()> {
    for table in [
        "READ_PROGRESS_SERIES",
        "COLLECTION_SERIES",
        "THUMBNAIL_SERIES",
        "SERIES_METADATA_GENRE",
        "SERIES_METADATA_TAG",
        "SERIES_METADATA_SHARING",
        "SERIES_METADATA_LINK",
        "SERIES_METADATA_ALTERNATE_TITLE",
        "BOOK_METADATA_AGGREGATION_AUTHOR",
        "BOOK_METADATA_AGGREGATION_TAG",
        "SERIES_METADATA",
        "BOOK_METADATA_AGGREGATION",
    ] {
        let statement = format!("DELETE FROM {table} WHERE SERIES_ID IN ({series_subquery})");
        let mut query = sqlx::query(&statement);
        for value in bindings {
            query = query.bind(*value);
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}

pub(crate) async fn reorder_series(
    tx: &mut Transaction<'_, Sqlite>,
    series_id: &str,
    refreshes: &mut Vec<NumberingRefreshTarget>,
) -> Result<()> {
    let mut books = sqlx::query(
        r#"SELECT B.ID,B.NAME,BM.NUMBER,BM.NUMBER_SORT,BM.NUMBER_LOCK,BM.NUMBER_SORT_LOCK
           FROM BOOK B JOIN BOOK_METADATA BM ON BM.BOOK_ID=B.ID
           WHERE B.SERIES_ID=? AND B.DELETED_DATE IS NULL"#,
    )
    .bind(series_id)
    .fetch_all(&mut **tx)
    .await?;
    books.sort_by(|left, right| compare_book_names(left.get("NAME"), right.get("NAME")));
    for (index, row) in books.iter().enumerate() {
        let number = i32::try_from(index + 1).unwrap_or(i32::MAX);
        let book_id: String = row.get("ID");
        sqlx::query("UPDATE BOOK SET NUMBER=?,LAST_MODIFIED_DATE=CURRENT_TIMESTAMP WHERE ID=?")
            .bind(number)
            .bind(&book_id)
            .execute(&mut **tx)
            .await?;
        let number_lock = row.get::<bool, _>("NUMBER_LOCK");
        let number_sort_lock = row.get::<bool, _>("NUMBER_SORT_LOCK");
        let number_changed = !number_lock && row.get::<String, _>("NUMBER") != number.to_string();
        let sort_changed = !number_sort_lock && (row.get::<f32, _>("NUMBER_SORT") - number as f32).abs() > f32::EPSILON;
        if !number_lock || !number_sort_lock {
            sqlx::query(
                r#"UPDATE BOOK_METADATA SET
                   NUMBER=CASE WHEN NUMBER_LOCK=0 THEN ? ELSE NUMBER END,
                   NUMBER_SORT=CASE WHEN NUMBER_SORT_LOCK=0 THEN ? ELSE NUMBER_SORT END,
                   LAST_MODIFIED_DATE=CURRENT_TIMESTAMP WHERE BOOK_ID=?"#,
            )
            .bind(number.to_string())
            .bind(number as f32)
            .bind(&book_id)
            .execute(&mut **tx)
            .await?;
        }
        if number_changed || sort_changed {
            refreshes.push(NumberingRefreshTarget {
                book_id,
                series_id: series_id.to_owned(),
            });
        }
    }
    sqlx::query("UPDATE SERIES SET BOOK_COUNT=?,LAST_MODIFIED_DATE=CURRENT_TIMESTAMP WHERE ID=?")
        .bind(i32::try_from(books.len()).unwrap_or(i32::MAX))
        .bind(series_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn cleanup_empty_containers(tx: &mut Transaction<'_, Sqlite>) -> Result<(Vec<String>, Vec<String>)> {
    let collections = if setting_enabled(tx, "DELETE_EMPTY_COLLECTIONS").await? {
        let ids = sqlx::query_scalar::<_, String>("SELECT ID FROM COLLECTION WHERE NOT EXISTS (SELECT 1 FROM COLLECTION_SERIES CS WHERE CS.COLLECTION_ID=COLLECTION.ID)")
            .fetch_all(&mut **tx).await?;
        sqlx::query("DELETE FROM THUMBNAIL_COLLECTION WHERE COLLECTION_ID IN (SELECT ID FROM COLLECTION WHERE NOT EXISTS (SELECT 1 FROM COLLECTION_SERIES CS WHERE CS.COLLECTION_ID=COLLECTION.ID))")
            .execute(&mut **tx).await?;
        sqlx::query("DELETE FROM COLLECTION WHERE NOT EXISTS (SELECT 1 FROM COLLECTION_SERIES CS WHERE CS.COLLECTION_ID=COLLECTION.ID)")
            .execute(&mut **tx).await?;
        ids
    } else {
        Vec::new()
    };
    let read_lists = if setting_enabled(tx, "DELETE_EMPTY_READLISTS").await? {
        let ids = sqlx::query_scalar::<_, String>("SELECT ID FROM READLIST WHERE NOT EXISTS (SELECT 1 FROM READLIST_BOOK RB WHERE RB.READLIST_ID=READLIST.ID)")
            .fetch_all(&mut **tx).await?;
        sqlx::query("DELETE FROM THUMBNAIL_READLIST WHERE READLIST_ID IN (SELECT ID FROM READLIST WHERE NOT EXISTS (SELECT 1 FROM READLIST_BOOK RB WHERE RB.READLIST_ID=READLIST.ID))")
            .execute(&mut **tx).await?;
        sqlx::query(
            "DELETE FROM READLIST WHERE NOT EXISTS (SELECT 1 FROM READLIST_BOOK RB WHERE RB.READLIST_ID=READLIST.ID)",
        )
        .execute(&mut **tx)
        .await?;
        ids
    } else {
        Vec::new()
    };
    Ok((collections, read_lists))
}

async fn setting_enabled(tx: &mut Transaction<'_, Sqlite>, key: &str) -> Result<bool> {
    let value = sqlx::query_scalar::<_, String>("SELECT VALUE FROM SERVER_SETTINGS WHERE KEY=?")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(value.is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true")))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use komga_core::{CollectionCreation, Library, ReadListCreation, scan_library_files};
    use url::Url;

    use super::*;

    #[tokio::test]
    async fn empty_trash_deletes_dependents_reorders_and_cleans_empty_sets() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Keep")).unwrap();
        fs::create_dir_all(root.join("Gone")).unwrap();
        for name in ["Book 2.cbz", "Book 3.cbz", "Book 10.cbz"] {
            fs::write(root.join("Keep").join(name), [1]).unwrap();
        }
        fs::write(root.join("Gone/Gone.cbz"), [1]).unwrap();
        let library = Library::new("Lifecycle".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap();
        let keep = series.iter().find(|value| value.name == "Keep").unwrap();
        let gone = series.iter().find(|value| value.name == "Gone").unwrap();
        let books = storage.list_books_by_series_id(&keep.id).await.unwrap();
        let removed = books.iter().find(|value| value.name == "Book 2").unwrap();
        let locked = books.iter().find(|value| value.name == "Book 3").unwrap();
        let gone_book = storage.list_books_by_series_id(&gone.id).await.unwrap().remove(0);

        storage
            .create_collection(
                "collection",
                &CollectionCreation {
                    name: "Gone collection".into(),
                    ordered: false,
                    series_ids: vec![gone.id.clone()],
                },
            )
            .await
            .unwrap();
        storage
            .create_read_list(
                "readlist",
                &ReadListCreation {
                    name: "Gone read list".into(),
                    summary: String::new(),
                    ordered: true,
                    book_ids: vec![removed.id.clone()],
                },
            )
            .await
            .unwrap();
        sqlx::query("UPDATE BOOK_METADATA SET NUMBER='locked',NUMBER_LOCK=1 WHERE BOOK_ID=?")
            .bind(&locked.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO THUMBNAIL_BOOK (ID,SELECTED,TYPE,BOOK_ID) VALUES ('thumb',1,'GENERATED',?)")
            .bind(&gone_book.id)
            .execute(storage.pool())
            .await
            .unwrap();

        fs::remove_file(root.join("Keep/Book 2.cbz")).unwrap();
        fs::remove_dir_all(root.join("Gone")).unwrap();
        sqlx::query("UPDATE BOOK SET DELETED_DATE=CURRENT_TIMESTAMP WHERE ID=?")
            .bind(&removed.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE SERIES SET DELETED_DATE=CURRENT_TIMESTAMP WHERE ID=?")
            .bind(&gone.id)
            .execute(storage.pool())
            .await
            .unwrap();
        let result = storage.empty_trash(&library.id).await.unwrap().unwrap();
        assert_eq!(result.series_deleted, 1);
        assert_eq!(result.books_deleted, 2);
        assert_eq!(result.collections_deleted, 1);
        assert_eq!(result.read_lists_deleted, 1);
        assert_eq!(result.numbering_refreshes.len(), 2);
        assert_eq!(result.series_ids, std::slice::from_ref(&gone.id));
        assert_eq!(
            result.book_ids.iter().collect::<std::collections::BTreeSet<_>>(),
            [&removed.id, &gone_book.id].into()
        );
        assert_eq!(result.affected_series_ids, std::slice::from_ref(&keep.id));
        assert_eq!(result.collection_ids, ["collection"]);
        assert_eq!(result.read_list_ids, ["readlist"]);

        let remaining = storage.list_books_by_series_id(&keep.id).await.unwrap();
        assert_eq!(remaining.iter().map(|book| book.number).collect::<Vec<_>>(), [1, 2]);
        let locked_metadata = storage.find_book_metadata(&locked.id).await.unwrap().unwrap();
        assert_eq!(locked_metadata.number, "locked");
        assert_eq!(locked_metadata.number_sort, 1.0);
        assert_eq!(storage.find_series_by_id(&gone.id).await.unwrap(), None);
        assert_eq!(storage.find_book_by_id(&removed.id).await.unwrap(), None);
        assert_eq!(storage.find_book_by_id(&gone_book.id).await.unwrap(), None);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM COLLECTION")
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM READLIST")
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM THUMBNAIL_BOOK")
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            0
        );
        assert!(root.join("Keep/Book 3.cbz").exists());
        assert!(root.join("Keep/Book 10.cbz").exists());
        assert_eq!(
            storage.empty_trash(&library.id).await.unwrap().unwrap(),
            EmptyTrashResult::default()
        );
    }

    #[tokio::test]
    async fn cleanup_settings_can_preserve_empty_containers() {
        let storage = Storage::connect_memory().await.unwrap();
        sqlx::query("UPDATE SERVER_SETTINGS SET VALUE='false' WHERE KEY IN ('DELETE_EMPTY_COLLECTIONS','DELETE_EMPTY_READLISTS')")
            .execute(storage.pool()).await.unwrap();
        sqlx::query("INSERT INTO COLLECTION (ID,NAME,ORDERED,SERIES_COUNT) VALUES ('c','Empty',0,0)")
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO READLIST (ID,NAME,SUMMARY,ORDERED,BOOK_COUNT) VALUES ('r','Empty','',1,0)")
            .execute(storage.pool())
            .await
            .unwrap();
        assert_eq!(storage.cleanup_empty_containers().await.unwrap(), (0, 0));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM COLLECTION")
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM READLIST")
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn empty_trash_rolls_back_every_dependency_when_deletion_fails() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Book.cbz"), [1]).unwrap();
        let library = Library::new("Rollback".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        sqlx::query("UPDATE SERIES SET DELETED_DATE=CURRENT_TIMESTAMP WHERE ID=?")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER fail_book_delete BEFORE DELETE ON BOOK BEGIN SELECT RAISE(ABORT,'forced'); END")
            .execute(storage.pool())
            .await
            .unwrap();

        assert!(storage.empty_trash(&library.id).await.is_err());
        assert!(storage.find_series_by_id(&series.id).await.unwrap().is_some());
        assert!(storage.find_book_by_id(&book.id).await.unwrap().is_some());
        assert!(storage.find_media_by_book_id(&book.id).await.unwrap().is_some());
        assert!(storage.find_book_metadata(&book.id).await.unwrap().is_some());
    }
}
