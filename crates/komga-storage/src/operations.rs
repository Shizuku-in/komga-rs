//! Persistence used by the operational HTTP surface.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Result, Storage};

/// One consistent snapshot of the database-backed Komga application gauges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentMetricsSnapshot {
    /// Number of libraries.
    pub libraries: i64,
    /// Number of collections.
    pub collections: i64,
    /// Number of read lists.
    pub read_lists: i64,
    /// Series counts keyed by library ID.
    pub series_by_library: BTreeMap<String, i64>,
    /// Book counts keyed by library ID.
    pub books_by_library: BTreeMap<String, i64>,
    /// Cumulative book file size keyed by library ID.
    pub book_file_size_by_library: BTreeMap<String, i64>,
    /// Sidecar counts keyed by library ID.
    pub sidecars_by_library: BTreeMap<String, i64>,
}

impl Storage {
    /// Lists announcement identifiers marked as read by one user.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn announcement_ids_read(&self, user_id: &str) -> Result<BTreeSet<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT ANNOUNCEMENT_ID FROM ANNOUNCEMENTS_READ WHERE USER_ID = ? ORDER BY ANNOUNCEMENT_ID",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    /// Adds announcement identifiers to a user's read set atomically.
    ///
    /// Existing identifiers are ignored. An empty set is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns a database error and rolls back every insertion when a write fails.
    pub async fn mark_announcements_read(&self, user_id: &str, announcement_ids: &BTreeSet<String>) -> Result<()> {
        if announcement_ids.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for announcement_id in announcement_ids {
            sqlx::query("INSERT OR IGNORE INTO ANNOUNCEMENTS_READ (USER_ID, ANNOUNCEMENT_ID) VALUES (?, ?)")
                .bind(user_id)
                .bind(announcement_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Reads all database-backed application gauges in one transaction.
    ///
    /// Counts intentionally include soft-deleted Series and Books, matching the
    /// unfiltered Kotlin repositories used by its metrics publisher.
    ///
    /// # Errors
    ///
    /// Returns a database error when any count cannot be read.
    pub async fn content_metrics_snapshot(&self) -> Result<ContentMetricsSnapshot> {
        let mut tx = self.pool.begin().await?;
        let libraries = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM LIBRARY")
            .fetch_one(&mut *tx)
            .await?;
        let collections = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM COLLECTION")
            .fetch_one(&mut *tx)
            .await?;
        let read_lists = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM READLIST")
            .fetch_one(&mut *tx)
            .await?;
        let series_by_library =
            grouped_metric(&mut tx, "SELECT LIBRARY_ID, COUNT(*) FROM SERIES GROUP BY LIBRARY_ID").await?;
        let books_by_library =
            grouped_metric(&mut tx, "SELECT LIBRARY_ID, COUNT(*) FROM BOOK GROUP BY LIBRARY_ID").await?;
        let book_file_size_by_library = grouped_metric(
            &mut tx,
            "SELECT LIBRARY_ID, COALESCE(SUM(FILE_SIZE), 0) FROM BOOK GROUP BY LIBRARY_ID",
        )
        .await?;
        let sidecars_by_library =
            grouped_metric(&mut tx, "SELECT LIBRARY_ID, COUNT(*) FROM SIDECAR GROUP BY LIBRARY_ID").await?;
        tx.commit().await?;
        Ok(ContentMetricsSnapshot {
            libraries,
            collections,
            read_lists,
            series_by_library,
            books_by_library,
            book_file_size_by_library,
            sidecars_by_library,
        })
    }
}

async fn grouped_metric(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, query: &str) -> Result<BTreeMap<String, i64>> {
    Ok(sqlx::query_as::<_, (String, i64)>(query)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect())
}

#[cfg(test)]
mod tests {
    use komga_core::User;

    use super::*;

    async fn user(storage: &Storage, id: &str) {
        let mut user = User::new(format!("{id}@example.org"), "password".into());
        user.id = id.to_owned();
        storage.create_user(&user).await.unwrap();
    }

    #[tokio::test]
    async fn announcement_read_state_is_additive_idempotent_and_deleted_with_the_user() {
        let storage = Storage::connect_memory().await.unwrap();
        user(&storage, "reader").await;

        storage
            .mark_announcements_read("reader", &BTreeSet::from(["one".into()]))
            .await
            .unwrap();
        storage
            .mark_announcements_read("reader", &BTreeSet::from(["one".into(), "two".into()]))
            .await
            .unwrap();
        assert_eq!(
            storage.announcement_ids_read("reader").await.unwrap(),
            BTreeSet::from(["one".into(), "two".into()])
        );

        assert!(storage.delete_user("reader").await.unwrap());
        assert!(storage.announcement_ids_read("reader").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn announcement_batch_rolls_back_when_one_insert_fails() {
        let storage = Storage::connect_memory().await.unwrap();
        user(&storage, "reader").await;
        sqlx::query(
            "CREATE TRIGGER fail_announcement BEFORE INSERT ON ANNOUNCEMENTS_READ \
             WHEN NEW.ANNOUNCEMENT_ID = 'two' BEGIN SELECT RAISE(FAIL, 'forced'); END",
        )
        .execute(storage.pool())
        .await
        .unwrap();

        assert!(
            storage
                .mark_announcements_read("reader", &BTreeSet::from(["one".into(), "two".into()]))
                .await
                .is_err()
        );
        assert!(storage.announcement_ids_read("reader").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn content_metrics_include_soft_deleted_rows_and_group_by_library() {
        let storage = Storage::connect_memory().await.unwrap();
        for (id, name) in [("library-a", "A"), ("library-b", "B")] {
            sqlx::query("INSERT INTO LIBRARY (ID, NAME, ROOT) VALUES (?, ?, ?)")
                .bind(id)
                .bind(name)
                .bind(format!("file:///tmp/{id}"))
                .execute(storage.pool())
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO SERIES (ID, FILE_LAST_MODIFIED, NAME, URL, LIBRARY_ID, DELETED_DATE) \
             VALUES ('series-a', CURRENT_TIMESTAMP, 'A', 'file:///tmp/a', 'library-a', CURRENT_TIMESTAMP), \
                    ('series-b', CURRENT_TIMESTAMP, 'B', 'file:///tmp/b', 'library-b', NULL)",
        )
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO BOOK (ID, FILE_LAST_MODIFIED, NAME, URL, SERIES_ID, FILE_SIZE, NUMBER, LIBRARY_ID, DELETED_DATE) \
             VALUES ('book-a1', CURRENT_TIMESTAMP, 'A1', 'file:///tmp/a1.cbz', 'series-a', 10, 1, 'library-a', NULL), \
                    ('book-a2', CURRENT_TIMESTAMP, 'A2', 'file:///tmp/a2.cbz', 'series-a', 20, 2, 'library-a', CURRENT_TIMESTAMP), \
                    ('book-b', CURRENT_TIMESTAMP, 'B', 'file:///tmp/b.cbz', 'series-b', 7, 1, 'library-b', NULL)",
        )
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO SIDECAR (URL, PARENT_URL, LAST_MODIFIED_TIME, LIBRARY_ID) \
             VALUES ('file:///tmp/a/series.json', 'file:///tmp/a', CURRENT_TIMESTAMP, 'library-a')",
        )
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO COLLECTION (ID, NAME, ORDERED) VALUES ('collection', 'Collection', 0)")
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO READLIST (ID, NAME, SUMMARY, ORDERED) VALUES ('readlist', 'Read list', '', 1)")
            .execute(storage.pool())
            .await
            .unwrap();

        assert_eq!(
            storage.content_metrics_snapshot().await.unwrap(),
            ContentMetricsSnapshot {
                libraries: 2,
                collections: 1,
                read_lists: 1,
                series_by_library: BTreeMap::from([("library-a".into(), 1), ("library-b".into(), 1)]),
                books_by_library: BTreeMap::from([("library-a".into(), 2), ("library-b".into(), 1)]),
                book_file_size_by_library: BTreeMap::from([("library-a".into(), 30), ("library-b".into(), 7),]),
                sidecars_by_library: BTreeMap::from([("library-a".into(), 1)]),
            }
        );
    }
}
