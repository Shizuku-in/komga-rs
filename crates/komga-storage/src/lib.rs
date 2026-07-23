//! SQLite persistence compatible with Komga's current Kotlin schema.
//!
//! Opening a database applies additive SQLx migrations. Those migrations use
//! `IF NOT EXISTS` for tables owned by already-released Flyway migrations, so a
//! current Kotlin-created database remains readable. Kotlin and Rust processes
//! must not write to the same database concurrently.

#![warn(missing_docs)]

use std::{collections::BTreeSet, path::Path, str::FromStr, time::Duration};

use komga_core::{AgeRestriction, AllowExclude, ApiKey, ContentRestrictions, User, UserRole};
use sqlx::{
    FromRow, Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use thiserror::Error;

/// Errors produced by SQLite access or compatibility migrations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// A case-insensitive email lookup found an existing user.
    #[error("a user with this email already exists")]
    DuplicateEmail,
    /// A user already owns an API key with the requested comment.
    #[error("an API key with this comment already exists for the user")]
    DuplicateApiKeyComment,
    /// A query, transaction, connection, or SQLite constraint failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// An embedded SQLx migration failed.
    #[error("database migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
}

/// Result type returned by storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Cloneable handle to the configured SQLite connection pool.
///
/// Cloning this value shares the underlying pool. All write methods preserve
/// relations transactionally where multiple tables are involved.
#[derive(Clone)]
pub struct Storage {
    pool: SqlitePool,
}

#[derive(Debug, FromRow)]
struct UserRow {
    id: String,
    email: String,
    password: String,
    shared_all_libraries: bool,
    age_restriction: Option<i64>,
    age_restriction_allow_only: Option<bool>,
}

#[derive(Debug, FromRow)]
struct ApiKeyRow {
    id: String,
    user_id: String,
    api_key: String,
    comment: String,
    created_date: String,
    last_modified_date: String,
}

impl Storage {
    /// Opens a SQLite URL and applies embedded compatibility migrations.
    ///
    /// The URL follows SQLx syntax, for example `sqlite:///srv/komga/database.sqlite`.
    /// The database is created when it does not exist. Connections use foreign
    /// keys, WAL journaling, and a 30-second busy timeout.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Database`] when the URL or connection is invalid,
    /// or [`StorageError::Migration`] when the schema cannot be initialized.
    pub async fn connect(database_path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_path)?
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Opens a SQLite file and applies embedded compatibility migrations.
    ///
    /// The file is created when absent. Existing current Komga databases retain
    /// their Flyway history; SQLx records its additive migrations separately.
    /// Callers should back up an existing database before its first Rust start.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the file cannot be opened or migrated.
    pub async fn connect_path(database_path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(database_path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(30))
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// Creates a migrated, single-connection in-memory database.
    ///
    /// This constructor exists for tests and ephemeral tools. A single
    /// connection is required because independent SQLite in-memory connections
    /// do not share state.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the in-memory schema cannot be initialized.
    pub async fn connect_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Memory);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Checks that the database accepts a trivial query.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Database`] when no connection can execute the query.
    pub async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// Returns the number of persisted users.
    ///
    /// # Errors
    ///
    /// Returns a database error when the count query fails.
    pub async fn count_users(&self) -> Result<i64> {
        Ok(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM USER")
            .fetch_one(&self.pool)
            .await?)
    }

    /// Finds a user and all role, sharing, and content-restriction relations by ID.
    ///
    /// # Errors
    ///
    /// Returns a database error when any part of the aggregate cannot be loaded.
    pub async fn find_user_by_id(&self, id: &str) -> Result<Option<User>> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"SELECT ID AS id, EMAIL AS email, PASSWORD AS password,
                      SHARED_ALL_LIBRARIES AS shared_all_libraries,
                      AGE_RESTRICTION AS age_restriction,
                      AGE_RESTRICTION_ALLOW_ONLY AS age_restriction_allow_only
               FROM USER WHERE ID = ?"#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        self.hydrate_user(row).await
    }

    /// Finds a user by email using a case-insensitive comparison.
    ///
    /// # Errors
    ///
    /// Returns a database error when any part of the aggregate cannot be loaded.
    pub async fn find_user_by_email(&self, email: &str) -> Result<Option<User>> {
        let row = sqlx::query_as::<_, UserRow>(
            r#"SELECT ID AS id, EMAIL AS email, PASSWORD AS password,
                      SHARED_ALL_LIBRARIES AS shared_all_libraries,
                      AGE_RESTRICTION AS age_restriction,
                      AGE_RESTRICTION_ALLOW_ONLY AS age_restriction_allow_only
               FROM USER WHERE lower(EMAIL) = lower(?)"#,
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        self.hydrate_user(row).await
    }

    /// Finds the owner of a persisted API key digest.
    ///
    /// `api_key_hash` must be the lowercase SHA-512 digest used by Kotlin's
    /// `Sha512DigestUtils.shaHex` implementation.
    ///
    /// # Errors
    ///
    /// Returns a database error when the key or user lookup fails.
    pub async fn find_user_by_api_key_hash(&self, api_key_hash: &str) -> Result<Option<User>> {
        let id = sqlx::query_scalar::<_, String>("SELECT USER_ID FROM USER_API_KEY WHERE API_KEY = ?")
            .bind(api_key_hash)
            .fetch_optional(&self.pool)
            .await?;
        match id {
            Some(id) => self.find_user_by_id(&id).await,
            None => Ok(None),
        }
    }

    /// Lists complete user aggregates ordered by email without case sensitivity.
    ///
    /// # Errors
    ///
    /// Returns a database error when the base rows or any relation cannot be loaded.
    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows = sqlx::query_as::<_, UserRow>(
            r#"SELECT ID AS id, EMAIL AS email, PASSWORD AS password,
                      SHARED_ALL_LIBRARIES AS shared_all_libraries,
                      AGE_RESTRICTION AS age_restriction,
                      AGE_RESTRICTION_ALLOW_ONLY AS age_restriction_allow_only
               FROM USER ORDER BY EMAIL COLLATE NOCASE"#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut users = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(user) = self.hydrate_user(Some(row)).await? {
                users.push(user);
            }
        }
        Ok(users)
    }

    async fn hydrate_user(&self, row: Option<UserRow>) -> Result<Option<User>> {
        let Some(row) = row else {
            return Ok(None);
        };

        let roles = sqlx::query_scalar::<_, String>("SELECT ROLE FROM USER_ROLE WHERE USER_ID = ?")
            .bind(&row.id)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .filter_map(|role| UserRole::try_from(role.as_str()).ok())
            .collect();
        let shared_library_ids =
            sqlx::query_scalar::<_, String>("SELECT LIBRARY_ID FROM USER_LIBRARY_SHARING WHERE USER_ID = ?")
                .bind(&row.id)
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .collect();
        let labels = sqlx::query("SELECT LABEL, ALLOW FROM USER_SHARING WHERE USER_ID = ?")
            .bind(&row.id)
            .fetch_all(&self.pool)
            .await?;
        let mut labels_allow = BTreeSet::new();
        let mut labels_exclude = BTreeSet::new();
        for label in labels {
            let value: String = label.try_get("LABEL")?;
            if label.try_get::<bool, _>("ALLOW")? {
                labels_allow.insert(value);
            } else {
                labels_exclude.insert(value);
            }
        }
        let age_restriction = match (row.age_restriction, row.age_restriction_allow_only) {
            (Some(age), Some(allow_only)) if age >= 0 => Some(AgeRestriction {
                age: age as u32,
                restriction: if allow_only {
                    AllowExclude::AllowOnly
                } else {
                    AllowExclude::Exclude
                },
            }),
            _ => None,
        };

        Ok(Some(User {
            id: row.id,
            email: row.email,
            password_hash: row.password,
            roles,
            shared_all_libraries: row.shared_all_libraries,
            shared_library_ids,
            restrictions: ContentRestrictions::new(age_restriction, labels_allow, labels_exclude),
        }))
    }

    /// Inserts a user and its role, library-sharing, and restriction relations.
    ///
    /// Shared library IDs that do not exist in `LIBRARY` are ignored, matching
    /// the Kotlin controller's repository filtering. Email uniqueness is
    /// enforced case-insensitively both before and during insertion.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::DuplicateEmail`] for an existing email or a
    /// database error when the transaction fails.
    pub async fn create_user(&self, user: &User) -> Result<()> {
        if self.find_user_by_email(&user.email).await?.is_some() {
            return Err(StorageError::DuplicateEmail);
        }

        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            r#"INSERT INTO USER
                 (ID, EMAIL, PASSWORD, SHARED_ALL_LIBRARIES, AGE_RESTRICTION, AGE_RESTRICTION_ALLOW_ONLY)
               VALUES (?, ?, ?, ?, ?, ?)"#,
        )
        .bind(&user.id)
        .bind(&user.email)
        .bind(&user.password_hash)
        .bind(user.shared_all_libraries)
        .bind(user.restrictions.age_restriction.as_ref().map(|value| value.age as i64))
        .bind(
            user.restrictions
                .age_restriction
                .as_ref()
                .map(|value| value.restriction == AllowExclude::AllowOnly),
        )
        .execute(&mut *tx)
        .await;

        if let Err(error) = inserted {
            return if is_unique_violation(&error) {
                Err(StorageError::DuplicateEmail)
            } else {
                Err(error.into())
            };
        }
        write_user_relations(&mut tx, user).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces a user's scalar fields and all owned relation rows atomically.
    ///
    /// The caller must first establish that the user exists. This method does
    /// not use its update count to distinguish a missing user from a no-op.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update or relation replacement fails.
    pub async fn update_user(&self, user: &User) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"UPDATE USER SET EMAIL = ?, PASSWORD = ?, SHARED_ALL_LIBRARIES = ?,
                   AGE_RESTRICTION = ?, AGE_RESTRICTION_ALLOW_ONLY = ?,
                   LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
               WHERE ID = ?"#,
        )
        .bind(&user.email)
        .bind(&user.password_hash)
        .bind(user.shared_all_libraries)
        .bind(user.restrictions.age_restriction.as_ref().map(|value| value.age as i64))
        .bind(
            user.restrictions
                .age_restriction
                .as_ref()
                .map(|value| value.restriction == AllowExclude::AllowOnly),
        )
        .bind(&user.id)
        .execute(&mut *tx)
        .await?;

        for table in ["USER_ROLE", "USER_LIBRARY_SHARING", "USER_SHARING"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE USER_ID = ?"))
                .bind(&user.id)
                .execute(&mut *tx)
                .await?;
        }
        write_user_relations(&mut tx, user).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Replaces a user's BCrypt password hash.
    ///
    /// Returns `true` when exactly one user was updated and `false` when the ID
    /// does not exist.
    ///
    /// # Errors
    ///
    /// Returns a database error when the update fails.
    pub async fn update_password(&self, user_id: &str, password_hash: &str) -> Result<bool> {
        let result = sqlx::query("UPDATE USER SET PASSWORD = ?, LAST_MODIFIED_DATE = CURRENT_TIMESTAMP WHERE ID = ?")
            .bind(password_hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Deletes a user and all known Kotlin-schema dependants atomically.
    ///
    /// Optional tables from not-yet-ported slices are detected at runtime. This
    /// allows deletion from both the smaller Rust foundation schema and a full
    /// Kotlin-created database. Returns `false` without writing when the user ID
    /// is absent.
    ///
    /// # Errors
    ///
    /// Returns a database error when dependency cleanup or deletion fails.
    pub async fn delete_user(&self, user_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let email = sqlx::query_scalar::<_, String>("SELECT EMAIL FROM USER WHERE ID = ?")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(email) = email else {
            return Ok(false);
        };

        // These tables belong to later Komga slices. Clean them when opening a
        // Kotlin-created database, while keeping the foundation schema small.
        for table in [
            "CLIENT_SETTINGS_USER",
            "READ_PROGRESS",
            "READ_PROGRESS_SERIES",
            "ANNOUNCEMENTS_READ",
        ] {
            delete_optional_by_user(&mut tx, table, user_id).await?;
        }
        if table_exists(&mut tx, "AUTHENTICATION_ACTIVITY").await? {
            sqlx::query("DELETE FROM AUTHENTICATION_ACTIVITY WHERE USER_ID = ? OR EMAIL = ?")
                .bind(user_id)
                .bind(&email)
                .execute(&mut *tx)
                .await?;
        }
        if table_exists(&mut tx, "SYNC_POINT").await? {
            for table in [
                "SYNC_POINT_READLIST_REMOVED_SYNCED",
                "SYNC_POINT_READLIST_BOOK",
                "SYNC_POINT_READLIST",
                "SYNC_POINT_BOOK_REMOVED_SYNCED",
                "SYNC_POINT_BOOK",
            ] {
                if table_exists(&mut tx, table).await? {
                    sqlx::query(&format!(
                        "DELETE FROM {table} WHERE SYNC_POINT_ID IN (SELECT ID FROM SYNC_POINT WHERE USER_ID = ?)"
                    ))
                    .bind(user_id)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            sqlx::query("DELETE FROM SYNC_POINT WHERE USER_ID = ?")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
        for table in ["USER_API_KEY", "USER_SHARING", "USER_LIBRARY_SHARING", "USER_ROLE"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE USER_ID = ?"))
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
        let result = sqlx::query("DELETE FROM USER WHERE ID = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() == 1)
    }

    /// Lists API key records owned by a user in creation order.
    ///
    /// Returned [`ApiKey::key`] values are persisted digests. HTTP callers must
    /// redact them before serialization.
    ///
    /// # Errors
    ///
    /// Returns a database error when the query fails.
    pub async fn list_api_keys(&self, user_id: &str) -> Result<Vec<ApiKey>> {
        let rows = sqlx::query_as::<_, ApiKeyRow>(
            r#"SELECT ID AS id, USER_ID AS user_id, API_KEY AS api_key, COMMENT AS comment,
                      CAST(CREATED_DATE AS TEXT) AS created_date,
                      CAST(LAST_MODIFIED_DATE AS TEXT) AS last_modified_date
               FROM USER_API_KEY WHERE USER_ID = ? ORDER BY CREATED_DATE"#,
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(ApiKeyRow::into_domain).collect())
    }

    /// Checks whether a user already owns an API key with an exact comment.
    ///
    /// # Errors
    ///
    /// Returns a database error when the existence query fails.
    pub async fn api_key_comment_exists(&self, user_id: &str, comment: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM USER_API_KEY WHERE USER_ID = ? AND COMMENT = ?)",
            )
            .bind(user_id)
            .bind(comment)
            .fetch_one(&self.pool)
            .await?,
        )
    }

    /// Persists an API key digest and returns the stored record.
    ///
    /// `key_hash` must be a deterministic digest, never the plaintext key.
    /// Comments are unique per user according to Komga lifecycle policy.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::DuplicateApiKeyComment`] for a duplicate comment,
    /// or a database error when insertion or reload fails.
    pub async fn create_api_key(&self, id: &str, user_id: &str, key_hash: &str, comment: &str) -> Result<ApiKey> {
        if self.api_key_comment_exists(user_id, comment).await? {
            return Err(StorageError::DuplicateApiKeyComment);
        }
        sqlx::query("INSERT INTO USER_API_KEY (ID, USER_ID, API_KEY, COMMENT) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(user_id)
            .bind(key_hash)
            .bind(comment)
            .execute(&self.pool)
            .await?;
        let row = sqlx::query_as::<_, ApiKeyRow>(
            r#"SELECT ID AS id, USER_ID AS user_id, API_KEY AS api_key, COMMENT AS comment,
                      CAST(CREATED_DATE AS TEXT) AS created_date,
                      CAST(LAST_MODIFIED_DATE AS TEXT) AS last_modified_date
               FROM USER_API_KEY WHERE ID = ?"#,
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.into_domain())
    }

    /// Deletes an API key only when it belongs to the supplied user.
    ///
    /// Returns `true` when a record was removed and `false` for a missing key or
    /// an ownership mismatch.
    ///
    /// # Errors
    ///
    /// Returns a database error when deletion fails.
    pub async fn delete_api_key(&self, user_id: &str, key_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM USER_API_KEY WHERE ID = ? AND USER_ID = ?")
            .bind(key_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }
}

async fn table_exists(tx: &mut Transaction<'_, Sqlite>, table: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?)")
            .bind(table)
            .fetch_one(&mut **tx)
            .await?,
    )
}

async fn delete_optional_by_user(tx: &mut Transaction<'_, Sqlite>, table: &str, user_id: &str) -> Result<()> {
    if table_exists(tx, table).await? {
        sqlx::query(&format!("DELETE FROM {table} WHERE USER_ID = ?"))
            .bind(user_id)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

async fn write_user_relations(tx: &mut Transaction<'_, Sqlite>, user: &User) -> Result<()> {
    for role in &user.roles {
        sqlx::query("INSERT INTO USER_ROLE (USER_ID, ROLE) VALUES (?, ?)")
            .bind(&user.id)
            .bind(role.as_str())
            .execute(&mut **tx)
            .await?;
    }
    for library_id in &user.shared_library_ids {
        sqlx::query("INSERT INTO USER_LIBRARY_SHARING (USER_ID, LIBRARY_ID) SELECT ?, ID FROM LIBRARY WHERE ID = ?")
            .bind(&user.id)
            .bind(library_id)
            .execute(&mut **tx)
            .await?;
    }
    for (allowed, labels) in [
        (true, &user.restrictions.labels_allow),
        (false, &user.restrictions.labels_exclude),
    ] {
        for label in labels {
            sqlx::query("INSERT INTO USER_SHARING (LABEL, ALLOW, USER_ID) VALUES (?, ?, ?)")
                .bind(label)
                .bind(allowed)
                .bind(&user.id)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

impl ApiKeyRow {
    fn into_domain(self) -> ApiKey {
        ApiKey {
            id: self.id,
            user_id: self.user_id,
            key: self.api_key,
            comment: self.comment,
            created_date: sqlite_datetime_to_utc(self.created_date),
            last_modified_date: sqlite_datetime_to_utc(self.last_modified_date),
        }
    }
}

fn sqlite_datetime_to_utc(value: String) -> String {
    if value.ends_with('Z') || value.contains('+') {
        value
    } else {
        format!("{}Z", value.replace(' ', "T"))
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use komga_core::{ALL_ROLES, new_id};

    #[tokio::test]
    async fn users_round_trip_with_komga_relations() {
        let storage = Storage::connect_memory().await.unwrap();
        sqlx::query("INSERT INTO LIBRARY (ID, NAME, ROOT) VALUES ('library-1', 'Test', '/tmp')")
            .execute(storage.pool())
            .await
            .unwrap();
        let mut user = User::new("Reader@Example.org".into(), "$2b$10$hash".into());
        user.roles = ALL_ROLES.into_iter().collect();
        user.shared_all_libraries = false;
        user.shared_library_ids.insert("library-1".into());
        user.shared_library_ids.insert("missing".into());
        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 16,
                restriction: AllowExclude::Exclude,
            }),
            ["kids".into()],
            ["adult".into()],
        );

        storage.create_user(&user).await.unwrap();
        let loaded = storage.find_user_by_email("reader@example.ORG").await.unwrap().unwrap();

        assert_eq!(loaded.email, user.email);
        assert_eq!(loaded.roles, user.roles);
        assert_eq!(loaded.shared_library_ids, BTreeSet::from(["library-1".into()]));
        assert_eq!(loaded.restrictions, user.restrictions);
    }

    #[tokio::test]
    async fn api_keys_are_found_by_hash_and_deleted_by_owner() {
        let storage = Storage::connect_memory().await.unwrap();
        let user = User::new("reader@example.org".into(), "hash".into());
        storage.create_user(&user).await.unwrap();
        let key_id = new_id();
        storage
            .create_api_key(&key_id, &user.id, "sha512", "integration")
            .await
            .unwrap();

        assert_eq!(
            storage.find_user_by_api_key_hash("sha512").await.unwrap().unwrap().id,
            user.id
        );
        assert!(!storage.delete_api_key(&user.id, "different").await.unwrap());
        assert!(storage.delete_api_key(&user.id, &key_id).await.unwrap());
    }

    #[tokio::test]
    async fn opens_an_existing_kotlin_schema_without_recreating_it() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("database.sqlite");
        let options = SqliteConnectOptions::new()
            .filename(&database)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::raw_sql(
            r#"
            CREATE TABLE LIBRARY (
                ID varchar NOT NULL PRIMARY KEY,
                CREATED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                NAME varchar NOT NULL,
                ROOT varchar NOT NULL,
                UNAVAILABLE boolean NOT NULL DEFAULT 0
            );
            CREATE TABLE USER (
                ID varchar NOT NULL PRIMARY KEY,
                CREATED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                EMAIL varchar NOT NULL UNIQUE,
                PASSWORD varchar NOT NULL,
                SHARED_ALL_LIBRARIES boolean NOT NULL DEFAULT 1,
                AGE_RESTRICTION integer,
                AGE_RESTRICTION_ALLOW_ONLY boolean
            );
            CREATE TABLE USER_ROLE (
                USER_ID varchar NOT NULL,
                ROLE varchar NOT NULL,
                PRIMARY KEY (USER_ID, ROLE),
                FOREIGN KEY (USER_ID) REFERENCES USER (ID)
            );
            CREATE TABLE USER_LIBRARY_SHARING (
                USER_ID varchar NOT NULL,
                LIBRARY_ID varchar NOT NULL,
                PRIMARY KEY (USER_ID, LIBRARY_ID),
                FOREIGN KEY (USER_ID) REFERENCES USER (ID),
                FOREIGN KEY (LIBRARY_ID) REFERENCES LIBRARY (ID)
            );
            CREATE TABLE USER_SHARING (
                LABEL varchar NOT NULL,
                ALLOW boolean NOT NULL,
                USER_ID varchar NOT NULL,
                PRIMARY KEY (LABEL, ALLOW, USER_ID),
                FOREIGN KEY (USER_ID) REFERENCES USER (ID)
            );
            CREATE TABLE USER_API_KEY (
                ID varchar NOT NULL PRIMARY KEY,
                USER_ID varchar NOT NULL,
                CREATED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                API_KEY varchar NOT NULL UNIQUE,
                COMMENT varchar NOT NULL,
                FOREIGN KEY (USER_ID) REFERENCES USER (ID)
            );
            CREATE TABLE CLIENT_SETTINGS_USER (
                USER_ID varchar NOT NULL,
                KEY varchar NOT NULL,
                VALUE varchar NOT NULL,
                FOREIGN KEY (USER_ID) REFERENCES USER (ID),
                PRIMARY KEY (KEY, USER_ID)
            );
            CREATE TABLE SYNC_POINT (
                ID varchar NOT NULL PRIMARY KEY,
                CREATED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
                USER_ID varchar NOT NULL,
                API_KEY_ID varchar,
                FOREIGN KEY (USER_ID) REFERENCES USER (ID)
            );
            CREATE TABLE SYNC_POINT_BOOK (
                SYNC_POINT_ID varchar NOT NULL,
                BOOK_ID varchar NOT NULL,
                PRIMARY KEY (SYNC_POINT_ID, BOOK_ID),
                FOREIGN KEY (SYNC_POINT_ID) REFERENCES SYNC_POINT (ID)
            );
            INSERT INTO USER (ID, EMAIL, PASSWORD) VALUES ('kotlin-user', 'reader@example.org', '$2a$10$hash');
            INSERT INTO USER_ROLE (USER_ID, ROLE) VALUES ('kotlin-user', 'PAGE_STREAMING');
            INSERT INTO CLIENT_SETTINGS_USER (USER_ID, KEY, VALUE) VALUES ('kotlin-user', 'reader.mode', 'paged');
            INSERT INTO SYNC_POINT (ID, USER_ID) VALUES ('sync-1', 'kotlin-user');
            INSERT INTO SYNC_POINT_BOOK (SYNC_POINT_ID, BOOK_ID) VALUES ('sync-1', 'book-1');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let storage = Storage::connect_path(&database).await.unwrap();
        let user = storage.find_user_by_email("READER@example.org").await.unwrap().unwrap();
        assert_eq!(user.id, "kotlin-user");
        assert_eq!(user.roles, BTreeSet::from([UserRole::PageStreaming]));
        assert!(storage.delete_user("kotlin-user").await.unwrap());
        assert_eq!(storage.count_users().await.unwrap(), 0);
    }
}
