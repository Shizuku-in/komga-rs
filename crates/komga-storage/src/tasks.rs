use std::{collections::BTreeMap, path::Path, str::FromStr, time::Duration};

use serde::Serialize;
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::Result;

/// One opaque task row using Komga's persistent queue contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecord {
    /// Stable deduplication key.
    pub id: String,
    /// Higher values are processed first.
    pub priority: i32,
    /// Optional serialization group for mutually exclusive processing.
    pub group_id: Option<String>,
    /// JVM class identifier expected by Kotlin.
    pub class: String,
    /// Short task type used for counters.
    pub simple_type: String,
    /// Serialized Kotlin-compatible task payload.
    pub payload: String,
    /// Current processor owner, if claimed.
    pub owner: Option<String>,
}

impl TaskRecord {
    /// Builds a Kotlin-compatible library scan task.
    pub fn scan_library(library_id: &str, scan_deep: bool, priority: i32) -> Self {
        let id = format!("SCAN_LIBRARY_{library_id}_DEEP_{scan_deep}");
        Self {
            id: id.clone(),
            priority,
            group_id: None,
            class: "org.gotson.komga.application.tasks.Task$ScanLibrary".into(),
            simple_type: "ScanLibrary".into(),
            payload: serde_json::json!({
                "libraryId": library_id,
                "scanDeep": scan_deep,
                "priority": priority,
                "groupId": null,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }

    /// Builds a Kotlin-compatible empty-trash task.
    pub fn empty_trash(library_id: &str, priority: i32) -> Self {
        let id = format!("EMPTY_TRASH_{library_id}");
        Self {
            id: id.clone(),
            priority,
            group_id: None,
            class: "org.gotson.komga.application.tasks.Task$EmptyTrash".into(),
            simple_type: "EmptyTrash".into(),
            payload: serde_json::json!({
                "libraryId": library_id,
                "priority": priority,
                "groupId": null,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }

    /// Builds a Kotlin-compatible book analysis task.
    pub fn analyze_book(book_id: &str, series_id: &str, priority: i32) -> Self {
        let id = format!("ANALYZE_BOOK_{book_id}");
        Self {
            id: id.clone(),
            priority,
            group_id: Some(series_id.to_owned()),
            class: "org.gotson.komga.application.tasks.Task$AnalyzeBook".into(),
            simple_type: "AnalyzeBook".into(),
            payload: serde_json::json!({
                "bookId": book_id,
                "priority": priority,
                "groupId": series_id,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }

    /// Builds a Kotlin-compatible book metadata refresh task.
    pub fn refresh_book_metadata(book_id: &str, series_id: &str, priority: i32) -> Self {
        let id = format!("REFRESH_BOOK_METADATA_{book_id}");
        Self {
            id: id.clone(),
            priority,
            group_id: Some(series_id.to_owned()),
            class: "org.gotson.komga.application.tasks.Task$RefreshBookMetadata".into(),
            simple_type: "RefreshBookMetadata".into(),
            payload: serde_json::json!({
                "bookId": book_id,
                "capabilities": [
                    "TITLE", "SUMMARY", "NUMBER", "NUMBER_SORT", "RELEASE_DATE",
                    "AUTHORS", "TAGS", "ISBN", "READ_LISTS", "THUMBNAILS", "LINKS"
                ],
                "priority": priority,
                "groupId": series_id,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }

    /// Builds a Kotlin-compatible series metadata refresh task.
    pub fn refresh_series_metadata(series_id: &str, priority: i32) -> Self {
        let id = format!("REFRESH_SERIES_METADATA_{series_id}");
        Self {
            id: id.clone(), priority, group_id: Some(series_id.to_owned()),
            class: "org.gotson.komga.application.tasks.Task$RefreshSeriesMetadata".into(),
            simple_type: "RefreshSeriesMetadata".into(),
            payload: serde_json::json!({
                "seriesId": series_id,
                "capabilities": ["STATUS", "TITLE", "SUMMARY", "READING_DIRECTION", "PUBLISHER", "AGE_RATING", "LANGUAGE", "GENRES", "TOTAL_BOOK_COUNT", "COLLECTIONS"],
                "priority": priority, "groupId": series_id, "uniqueId": id,
            }).to_string(), owner: None,
        }
    }

    /// Builds a Kotlin-compatible generated book-thumbnail task.
    pub fn generate_book_thumbnail(book_id: &str, priority: i32) -> Self {
        let id = format!("GENERATE_BOOK_THUMBNAIL_{book_id}");
        Self {
            id: id.clone(),
            priority,
            group_id: None,
            class: "org.gotson.komga.application.tasks.Task$GenerateBookThumbnail".into(),
            simple_type: "GenerateBookThumbnail".into(),
            payload: serde_json::json!({
                "bookId": book_id,
                "priority": priority,
                "groupId": null,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }

    /// Builds a Kotlin-compatible generated-thumbnail discovery task.
    pub fn find_book_thumbnails_to_regenerate(for_bigger_result_only: bool, priority: i32) -> Self {
        let id = "FIND_BOOK_THUMBNAILS_TO_REGENERATE".to_owned();
        Self {
            id: id.clone(),
            priority,
            group_id: None,
            class: "org.gotson.komga.application.tasks.Task$FindBookThumbnailsToRegenerate".into(),
            simple_type: "FindBookThumbnailsToRegenerate".into(),
            payload: serde_json::json!({
                "forBiggerResultOnly": for_bigger_result_only,
                "priority": priority,
                "groupId": null,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }

    /// Builds a Kotlin-compatible series metadata aggregation task.
    pub fn aggregate_series_metadata(series_id: &str, priority: i32) -> Self {
        let id = format!("AGGREGATE_SERIES_METADATA_{series_id}");
        Self {
            id: id.clone(),
            priority,
            group_id: Some(series_id.to_owned()),
            class: "org.gotson.komga.application.tasks.Task$AggregateSeriesMetadata".into(),
            simple_type: "AggregateSeriesMetadata".into(),
            payload: serde_json::json!({
                "seriesId": series_id,
                "priority": priority,
                "groupId": series_id,
                "uniqueId": id,
            })
            .to_string(),
            owner: None,
        }
    }
}

/// Cloneable handle to Komga's separate persistent task queue.
#[derive(Clone)]
pub struct TaskStorage {
    pool: SqlitePool,
}

impl TaskStorage {
    /// Opens a task database file, migrates it, and releases stale owners.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be opened or migrated.
    pub async fn connect_path(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./tasks_migrations").run(&pool).await?;
        let storage = Self { pool };
        storage.disown_all().await?;
        Ok(storage)
    }

    /// Creates an isolated in-memory task database for tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema cannot be initialized.
    pub async fn connect_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Memory);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./tasks_migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Inserts or refreshes a task by unique identifier.
    ///
    /// The current owner is intentionally preserved on conflict, matching the
    /// Kotlin repository's behavior.
    ///
    /// # Errors
    ///
    /// Returns a database error when the task cannot be saved.
    pub async fn save(&self, task: &TaskRecord) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO TASK (ID, PRIORITY, GROUP_ID, CLASS, SIMPLE_TYPE, PAYLOAD)
               VALUES (?, ?, ?, ?, ?, ?)
               ON CONFLICT(ID) DO UPDATE SET
                 GROUP_ID = excluded.GROUP_ID,
                 PRIORITY = excluded.PRIORITY,
                 CLASS = excluded.CLASS,
                 SIMPLE_TYPE = excluded.SIMPLE_TYPE,
                 PAYLOAD = excluded.PAYLOAD,
                 LAST_MODIFIED_DATE = CURRENT_TIMESTAMP"#,
        )
        .bind(&task.id)
        .bind(task.priority)
        .bind(&task.group_id)
        .bind(&task.class)
        .bind(&task.simple_type)
        .bind(&task.payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Lists queue rows in scheduling order.
    ///
    /// # Errors
    ///
    /// Returns a database error when rows cannot be read.
    pub async fn list(&self) -> Result<Vec<TaskRecord>> {
        Ok(sqlx::query_as::<_, TaskRecord>(
            r#"SELECT ID AS id, PRIORITY AS priority, GROUP_ID AS group_id,
                      CLASS AS class, SIMPLE_TYPE AS simple_type, PAYLOAD AS payload,
                      OWNER AS owner
                 FROM TASK ORDER BY PRIORITY DESC, LAST_MODIFIED_DATE"#,
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Claims the highest-priority task whose group is not already owned.
    ///
    /// # Errors
    ///
    /// Returns a database error when claiming fails.
    pub async fn take_first(&self, owner: &str) -> Result<Option<TaskRecord>> {
        self.take_first_matching(owner, None).await
    }

    /// Atomically claims the first task with the supplied simple type.
    ///
    /// Unsupported task rows remain unowned and untouched for a future port or
    /// the Kotlin server.
    ///
    /// # Errors
    ///
    /// Returns a database error when claiming fails.
    pub async fn take_first_of_type(&self, owner: &str, simple_type: &str) -> Result<Option<TaskRecord>> {
        self.take_first_matching(owner, Some(simple_type)).await
    }

    /// Atomically claims the highest-priority task among supported simple types.
    ///
    /// # Errors
    ///
    /// Returns a database error when claiming fails.
    pub async fn take_first_of_types(&self, owner: &str, simple_types: &[&str]) -> Result<Option<TaskRecord>> {
        if simple_types.is_empty() {
            return Ok(None);
        }
        let placeholders = std::iter::repeat_n("?", simple_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            r#"UPDATE TASK SET OWNER = ?
                 WHERE ID = (
                    SELECT ID FROM TASK
                     WHERE OWNER IS NULL
                       AND SIMPLE_TYPE IN ({placeholders})
                       AND (GROUP_ID IS NULL OR GROUP_ID NOT IN
                           (SELECT GROUP_ID FROM TASK WHERE OWNER IS NOT NULL AND GROUP_ID IS NOT NULL))
                     ORDER BY PRIORITY DESC, LAST_MODIFIED_DATE
                     LIMIT 1
                 )
                   AND OWNER IS NULL
             RETURNING ID AS id, PRIORITY AS priority, GROUP_ID AS group_id,
                       CLASS AS class, SIMPLE_TYPE AS simple_type, PAYLOAD AS payload,
                       OWNER AS owner"#,
        );
        let mut query = sqlx::query_as::<_, TaskRecord>(&sql).bind(owner);
        for simple_type in simple_types {
            query = query.bind(simple_type);
        }
        Ok(query.fetch_optional(&self.pool).await?)
    }

    async fn take_first_matching(&self, owner: &str, simple_type: Option<&str>) -> Result<Option<TaskRecord>> {
        Ok(sqlx::query_as::<_, TaskRecord>(
            r#"UPDATE TASK SET OWNER = ?
                 WHERE ID = (
                    SELECT ID FROM TASK
                     WHERE OWNER IS NULL
                       AND (? IS NULL OR SIMPLE_TYPE = ?)
                       AND (GROUP_ID IS NULL OR GROUP_ID NOT IN
                           (SELECT GROUP_ID FROM TASK WHERE OWNER IS NOT NULL AND GROUP_ID IS NOT NULL))
                     ORDER BY PRIORITY DESC, LAST_MODIFIED_DATE
                     LIMIT 1
                 )
                   AND OWNER IS NULL
             RETURNING ID AS id, PRIORITY AS priority, GROUP_ID AS group_id,
                       CLASS AS class, SIMPLE_TYPE AS simple_type, PAYLOAD AS payload,
                       OWNER AS owner"#,
        )
        .bind(owner)
        .bind(simple_type)
        .bind(simple_type)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Deletes a completed task by ID.
    ///
    /// # Errors
    ///
    /// Returns a database error when deletion fails.
    pub async fn delete(&self, task_id: &str) -> Result<bool> {
        Ok(sqlx::query("DELETE FROM TASK WHERE ID = ?")
            .bind(task_id)
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 1)
    }

    /// Releases a task only when it is owned by the supplied worker.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update fails.
    pub async fn release(&self, task_id: &str, owner: &str) -> Result<bool> {
        Ok(sqlx::query("UPDATE TASK SET OWNER = NULL WHERE ID = ? AND OWNER = ?")
            .bind(task_id)
            .bind(owner)
            .execute(&self.pool)
            .await?
            .rows_affected()
            == 1)
    }

    /// Counts all queued tasks by Kotlin simple type.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn count_by_simple_type(&self) -> Result<BTreeMap<String, i64>> {
        Ok(sqlx::query_as::<_, (String, i64)>(
            "SELECT SIMPLE_TYPE, count(*) FROM TASK GROUP BY SIMPLE_TYPE ORDER BY SIMPLE_TYPE",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect())
    }

    /// Deletes every task not currently owned and returns the affected count.
    ///
    /// # Errors
    ///
    /// Returns a database error when deletion fails.
    pub async fn delete_all_unowned(&self) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM TASK WHERE OWNER IS NULL")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    /// Releases all task owners and returns the affected count.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update fails.
    pub async fn disown_all(&self) -> Result<u64> {
        Ok(sqlx::query("UPDATE TASK SET OWNER = NULL WHERE OWNER IS NOT NULL")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}
