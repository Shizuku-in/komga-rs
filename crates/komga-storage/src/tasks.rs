use std::{path::Path, str::FromStr, time::Duration};

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
        let mut tx = self.pool.begin().await?;
        let task = sqlx::query_as::<_, TaskRecord>(
            r#"SELECT ID AS id, PRIORITY AS priority, GROUP_ID AS group_id,
                      CLASS AS class, SIMPLE_TYPE AS simple_type, PAYLOAD AS payload,
                      OWNER AS owner
                 FROM TASK
                WHERE OWNER IS NULL
                  AND (GROUP_ID IS NULL OR GROUP_ID NOT IN
                      (SELECT GROUP_ID FROM TASK WHERE OWNER IS NOT NULL AND GROUP_ID IS NOT NULL))
                ORDER BY PRIORITY DESC, LAST_MODIFIED_DATE
                LIMIT 1"#,
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some(mut task) = task else {
            return Ok(None);
        };
        sqlx::query("UPDATE TASK SET OWNER = ? WHERE ID = ? AND OWNER IS NULL")
            .bind(owner)
            .bind(&task.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        task.owner = Some(owner.to_owned());
        Ok(Some(task))
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
