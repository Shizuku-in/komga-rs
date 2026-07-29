//! SQLite persistence compatible with Komga's current Kotlin schema.
//!
//! Opening a database applies additive SQLx migrations. Those migrations use
//! `IF NOT EXISTS` for tables owned by already-released Flyway migrations, so a
//! current Kotlin-created database remains readable. Kotlin and Rust processes
//! must not write to the same database concurrently.

#![warn(missing_docs)]

mod auth;
mod authentication_activity;
mod containers;
mod content;
mod content_queries;
mod history;
mod library;
mod lifecycle;
mod media;
mod metadata;
mod operations;
mod page_hash;
mod query_support;
mod read_progress;
mod referential;
mod schema;
mod settings;
mod source_conversion;
mod source_deletion;
mod source_import;
mod sync;
mod tasks;

pub use containers::{ComicInfoContainerUpdate, ContainerThumbnail};
pub use content::ScanApplyStats;
pub use content_queries::{LegacySeriesRegex, LegacySeriesRegexField, ReadListFile};
pub use library::DeleteLibraryResult;
pub use lifecycle::{EmptyTrashResult, NumberingRefreshTarget};
pub use media::{PageHashTarget, PageHashUpdate};
pub use operations::ContentMetricsSnapshot;
pub use referential::{ReferentialScope, author_page_request};
pub use settings::{ClientSetting, ServerSettings, ServerSettingsPatch, SettingUpdate};
pub use source_import::BookImportCommitResult;
pub use tasks::{RebuildIndexEntity, TaskRecord, TaskStorage};

pub(crate) use auth::sqlite_datetime_to_utc;

use std::{cmp::Ordering, collections::BTreeSet, path::Path, str::FromStr, time::Duration};

use komga_core::{AgeRestriction, AllowExclude, ContentRestrictions, User, UserRole};
use sqlx::{
    FromRow, Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const UNICODE_NOCASE_COLLATION: &str = "KOMGA_UNICODE_NOCASE";

fn unicode_nocase_compare(left: &str, right: &str) -> Ordering {
    let normalize = |value: &str| value.nfkc().flat_map(char::to_lowercase).collect::<String>();
    normalize(left).cmp(&normalize(right))
}

fn with_content_extensions(options: SqliteConnectOptions) -> SqliteConnectOptions {
    options
        .with_regexp()
        .collation(UNICODE_NOCASE_COLLATION, unicode_nocase_compare)
}

/// Errors produced by SQLite access or compatibility migrations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// A case-insensitive email lookup found an existing user.
    #[error("a user with this email already exists")]
    DuplicateEmail,
    /// A user already owns an API key with the requested comment.
    #[error("an API key with this comment already exists for the user")]
    DuplicateApiKeyComment,
    /// Persisted data could not be converted to the current domain model.
    #[error("invalid persisted data: {0}")]
    InvalidData(String),
    /// A content query requested a feature or value not supported by this slice.
    #[error("invalid content query: {0}")]
    InvalidQuery(String),
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
        let options = with_content_extensions(
            SqliteConnectOptions::from_str(database_path)?
                .create_if_missing(true)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(30))
                .journal_mode(SqliteJournalMode::Wal),
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        schema::reconcile_library_schema(&pool).await?;
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
        let options = with_content_extensions(
            SqliteConnectOptions::new()
                .filename(database_path)
                .create_if_missing(true)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(30))
                .journal_mode(SqliteJournalMode::Wal),
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        schema::reconcile_library_schema(&pool).await?;
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
        let options = with_content_extensions(
            SqliteConnectOptions::from_str("sqlite::memory:")?
                .foreign_keys(true)
                .journal_mode(SqliteJournalMode::Memory),
        );
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        schema::reconcile_library_schema(&pool).await?;
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

fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(database) if database.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use chrono::Local;
    use komga_core::{
        ALL_ROLES, AgeRestriction, AllowExclude, BookCondition, BookPage, BookSearch, CollectionCreation,
        ContentRestrictions, Dimension, EqualityOperator, Library, Media, MediaFile, MediaStatus, PageRequest,
        ReadListCreation, ReadProgress, ReadStatus, ScanInterval, ScanResult, SeriesSearch, SortDirection, SortOrder,
        ThumbnailBook, ThumbnailSeries, new_id, scan_library_files,
    };
    use url::Url;

    fn set_scanned_book_hash(scan: &mut ScanResult, book_name: &str, hash: &str) {
        let mut matches = scan
            .series
            .iter_mut()
            .flat_map(|series| series.books.iter_mut())
            .filter(|book| book.name == book_name);
        matches.next().unwrap().file_hash = hash.into();
        assert!(
            matches.next().is_none(),
            "book name must identify exactly one scan entry"
        );
    }

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
    async fn koreader_hash_updates_require_a_current_snapshot_and_enabled_library() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Book.epub"), b"hello").unwrap();
        let mut library = Library::new("KOReader".into(), Url::from_directory_path(&root).unwrap());
        library.hash_koreader = true;
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let book = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);

        let missing = storage.list_books_without_koreader_hash(&library.id).await.unwrap();
        assert_eq!(missing.as_slice(), std::slice::from_ref(&book.id));
        let mut stale = book.clone();
        stale.file_size += 1;
        assert!(
            !storage
                .update_book_koreader_hash_if_current(&stale, "stale")
                .await
                .unwrap()
        );
        assert!(
            storage
                .update_book_koreader_hash_if_current(&book, "partial-md5")
                .await
                .unwrap()
        );
        assert!(
            storage
                .list_books_without_koreader_hash(&library.id)
                .await
                .unwrap()
                .is_empty()
        );

        sqlx::query("UPDATE BOOK SET FILE_HASH_KOREADER='' WHERE ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        library.hash_koreader = false;
        storage.update_library(&library).await.unwrap();
        assert!(
            !storage
                .update_book_koreader_hash_if_current(&book, "disabled")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn koreader_hash_visibility_applies_age_and_sharing_labels() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Book.cbz"), b"content").unwrap();
        let library = Library::new("Restricted".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        storage
            .apply_library_scan(&library.id, &scan_library_files(&library, || false).unwrap(), false)
            .await
            .unwrap();
        let book = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        sqlx::query("UPDATE BOOK SET FILE_HASH_KOREADER='restricted-hash' WHERE ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE SERIES_METADATA SET AGE_RATING=18 WHERE SERIES_ID=?")
            .bind(&book.series_id)
            .execute(&storage.pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO SERIES_METADATA_SHARING (SERIES_ID,LABEL) VALUES (?,'adult')")
            .bind(&book.series_id)
            .execute(&storage.pool)
            .await
            .unwrap();

        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 12,
                restriction: AllowExclude::AllowOnly,
            }),
            Vec::<String>::new(),
            Vec::<String>::new(),
        );
        assert!(
            storage
                .find_visible_book_ids_by_koreader_hash("restricted-hash", &user)
                .await
                .unwrap()
                .is_empty()
        );

        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 12,
                restriction: AllowExclude::AllowOnly,
            }),
            ["adult".into()],
            Vec::<String>::new(),
        );
        let visible = storage
            .find_visible_book_ids_by_koreader_hash("restricted-hash", &user)
            .await
            .unwrap();
        assert_eq!(visible.as_slice(), std::slice::from_ref(&book.id));

        user.restrictions = ContentRestrictions::new(None, Vec::<String>::new(), ["adult".into()]);
        assert!(
            storage
                .find_visible_book_ids_by_koreader_hash("restricted-hash", &user)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scans_preserve_koreader_hash_for_identical_content_and_clear_it_when_changed() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        fs::create_dir_all(root.join("Series")).unwrap();
        fs::write(root.join("Series/Book.cbz"), b"content").unwrap();
        let library = Library::new("Scans".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let initial = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &initial, false).await.unwrap();
        let book = storage
            .list_active_books_by_library_id(&library.id)
            .await
            .unwrap()
            .remove(0);
        sqlx::query("UPDATE BOOK SET FILE_HASH='same',FILE_HASH_KOREADER='koreader' WHERE ID=?")
            .bind(&book.id)
            .execute(&storage.pool)
            .await
            .unwrap();

        let mut timestamp_only = scan_library_files(&library, || false).unwrap();
        timestamp_only.series[0].books[0].file_last_modified += chrono::Duration::seconds(1);
        timestamp_only.series[0].books[0].file_hash = "same".into();
        storage
            .apply_library_scan(&library.id, &timestamp_only, true)
            .await
            .unwrap();
        assert_eq!(
            storage
                .find_book_by_id(&book.id)
                .await
                .unwrap()
                .unwrap()
                .file_hash_koreader,
            "koreader"
        );

        let mut changed = timestamp_only;
        changed.series[0].books[0].file_size += 1;
        changed.series[0].books[0].file_hash.clear();
        storage.apply_library_scan(&library.id, &changed, true).await.unwrap();
        assert!(
            storage
                .find_book_by_id(&book.id)
                .await
                .unwrap()
                .unwrap()
                .file_hash_koreader
                .is_empty()
        );
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

    #[tokio::test]
    async fn libraries_settings_and_exclusions_round_trip() {
        let storage = Storage::connect_memory().await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut library = Library::new("Library".into(), Url::from_file_path(root.path()).unwrap());
        library.scan_interval = ScanInterval::Daily;
        library.scan_directory_exclusions = BTreeSet::from(["@eaDir".into(), "tmp".into()]);
        library.oneshots_directory = Some("One Shots".into());
        storage.create_library(&library).await.unwrap();

        let loaded = storage.find_library_by_id(&library.id).await.unwrap().unwrap();
        assert_eq!(loaded.name, "Library");
        assert_eq!(loaded.scan_interval, ScanInterval::Daily);
        assert_eq!(loaded.scan_directory_exclusions, library.scan_directory_exclusions);

        storage
            .save_global_client_settings(&[("ui.theme".into(), "dark".into(), true)])
            .await
            .unwrap();
        storage
            .save_global_client_settings(&[("ui.theme".into(), "light".into(), false)])
            .await
            .unwrap();
        let settings = storage.list_global_client_settings(false).await.unwrap();
        assert_eq!(settings["ui.theme"].value, "light");
        assert_eq!(settings["ui.theme"].allow_unauthorized, Some(true));

        assert!(matches!(
            storage.delete_library(&library.id).await.unwrap(),
            DeleteLibraryResult::Deleted { .. }
        ));
    }

    #[tokio::test]
    async fn task_queue_upserts_and_respects_group_ownership() {
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let mut first = TaskRecord::scan_library("library", false, 4);
        tasks.save(&first).await.unwrap();
        first.priority = 8;
        tasks.save(&first).await.unwrap();
        let saved = tasks.list().await.unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].priority, 8);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&saved[0].payload).unwrap()["libraryId"],
            "library"
        );

        let mut grouped_one = TaskRecord::empty_trash("one", 6);
        grouped_one.group_id = Some("group".into());
        let mut grouped_two = TaskRecord::empty_trash("two", 6);
        grouped_two.group_id = Some("group".into());
        tasks.save(&grouped_one).await.unwrap();
        tasks.save(&grouped_two).await.unwrap();
        let claimed = tasks.take_first("worker-1").await.unwrap().unwrap();
        assert_eq!(claimed.priority, 8);
        let grouped = tasks.take_first("worker-2").await.unwrap().unwrap();
        assert_eq!(grouped.group_id.as_deref(), Some("group"));
        assert_eq!(tasks.take_first("worker-3").await.unwrap(), None);
        assert_eq!(tasks.delete_all_unowned().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn type_filtered_task_claims_leave_other_task_types_untouched() {
        let tasks = TaskStorage::connect_memory().await.unwrap();
        tasks.save(&TaskRecord::empty_trash("library", 8)).await.unwrap();
        tasks
            .save(&TaskRecord::scan_library("library", false, 4))
            .await
            .unwrap();

        let scan = tasks
            .take_first_of_type("worker", "ScanLibrary")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(scan.simple_type, "ScanLibrary");
        assert!(
            tasks
                .take_first_of_type("other", "ScanLibrary")
                .await
                .unwrap()
                .is_none()
        );
        let counts = tasks.count_by_simple_type().await.unwrap();
        assert_eq!(counts["EmptyTrash"], 1);
        assert_eq!(counts["ScanLibrary"], 1);
        assert!(tasks.release(&scan.id, "worker").await.unwrap());
        assert!(tasks.delete(&scan.id).await.unwrap());
        assert_eq!(tasks.list().await.unwrap()[0].simple_type, "EmptyTrash");
    }

    #[tokio::test]
    async fn rebuild_index_task_matches_kotlin_payload_and_deduplicates() {
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let full = TaskRecord::rebuild_index(None, 8);
        assert_eq!(full.id, "REBUILD_INDEX");
        assert_eq!(full.class, "org.gotson.komga.application.tasks.Task$RebuildIndex");
        assert_eq!(full.simple_type, "RebuildIndex");
        let payload: serde_json::Value = serde_json::from_str(&full.payload).unwrap();
        assert_eq!(payload["entities"], serde_json::Value::Null);
        assert_eq!(payload["uniqueId"], "REBUILD_INDEX");
        tasks.save(&full).await.unwrap();

        let entities = BTreeSet::from([RebuildIndexEntity::Book, RebuildIndexEntity::Series]);
        tasks
            .save(&TaskRecord::rebuild_index(Some(&entities), 4))
            .await
            .unwrap();
        let queued = tasks.list().await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].priority, 4);
        let payload: serde_json::Value = serde_json::from_str(&queued[0].payload).unwrap();
        assert_eq!(payload["entities"], serde_json::json!(["Book", "Series"]));
    }

    #[test]
    fn hashing_tasks_match_kotlin_payloads() {
        let task = TaskRecord::hash_book("book", 0);
        assert_eq!(task.id, "HASH_BOOK_book");
        assert_eq!(task.priority, 0);
        assert_eq!(task.group_id, None);
        assert_eq!(task.class, "org.gotson.komga.application.tasks.Task$HashBook");
        assert_eq!(task.simple_type, "HashBook");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&task.payload).unwrap(),
            serde_json::json!({
                "bookId": "book",
                "priority": 0,
                "groupId": null,
                "uniqueId": "HASH_BOOK_book",
            })
        );

        let koreader = TaskRecord::hash_book_koreader("book", 2);
        assert_eq!(koreader.id, "HASH_BOOK_KOREADER_book");
        assert_eq!(koreader.priority, 2);
        assert_eq!(koreader.group_id, None);
        assert_eq!(
            koreader.class,
            "org.gotson.komga.application.tasks.Task$HashBookKoreader"
        );
        assert_eq!(koreader.simple_type, "HashBookKoreader");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&koreader.payload).unwrap(),
            serde_json::json!({
                "bookId": "book",
                "priority": 2,
                "groupId": null,
                "uniqueId": "HASH_BOOK_KOREADER_book",
            })
        );

        let discovery = TaskRecord::find_books_with_missing_page_hash("library", 0);
        assert_eq!(discovery.id, "FIND_BOOKS_WITH_MISSING_PAGE_HASH_library");
        assert_eq!(discovery.group_id, None);
        assert_eq!(
            discovery.class,
            "org.gotson.komga.application.tasks.Task$FindBooksWithMissingPageHash"
        );
        assert_eq!(discovery.simple_type, "FindBooksWithMissingPageHash");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&discovery.payload).unwrap(),
            serde_json::json!({
                "libraryId": "library",
                "priority": 0,
                "groupId": null,
                "uniqueId": "FIND_BOOKS_WITH_MISSING_PAGE_HASH_library",
            })
        );

        let pages = TaskRecord::hash_book_pages("book", 1);
        assert_eq!(pages.id, "HASH_BOOK_PAGES_book");
        assert_eq!(pages.group_id, None);
        assert_eq!(pages.class, "org.gotson.komga.application.tasks.Task$HashBookPages");
        assert_eq!(pages.simple_type, "HashBookPages");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&pages.payload).unwrap(),
            serde_json::json!({
                "bookId": "book",
                "priority": 1,
                "groupId": null,
                "uniqueId": "HASH_BOOK_PAGES_book",
            })
        );

        let discovery = TaskRecord::find_duplicate_pages_to_delete("library", 0);
        assert_eq!(discovery.id, "FIND_DUPLICATE_PAGES_TO_DELETE_library");
        assert_eq!(
            discovery.class,
            "org.gotson.komga.application.tasks.Task$FindDuplicatePagesToDelete"
        );
        assert_eq!(discovery.simple_type, "FindDuplicatePagesToDelete");
        let target = komga_core::HashedPageTarget {
            file_name: "1.png".into(),
            media_type: "image/png".into(),
            file_hash: "hash".into(),
            file_size: 42,
            page_number: 1,
        };
        let removal = TaskRecord::remove_hashed_pages("book", std::slice::from_ref(&target), 1);
        assert_eq!(removal.id, "REMOVE_HASHED_PAGES_book");
        assert_eq!(
            removal.class,
            "org.gotson.komga.application.tasks.Task$RemoveHashedPages"
        );
        assert_eq!(removal.simple_type, "RemoveHashedPages");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&removal.payload).unwrap(),
            serde_json::json!({
                "bookId": "book",
                "pages": [{
                    "fileName": "1.png",
                    "mediaType": "image/png",
                    "fileHash": "hash",
                    "fileSize": 42,
                    "pageNumber": 1,
                }],
                "priority": 1,
                "groupId": null,
                "uniqueId": "REMOVE_HASHED_PAGES_book",
            })
        );
    }

    #[test]
    fn source_deletion_tasks_match_kotlin_payloads() {
        let book = TaskRecord::delete_book("book");
        assert_eq!(book.id, "DELETE_BOOK_book");
        assert_eq!(book.priority, 8);
        assert_eq!(book.group_id, None);
        assert_eq!(book.class, "org.gotson.komga.application.tasks.Task$DeleteBook");
        assert_eq!(book.simple_type, "DeleteBook");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&book.payload).unwrap(),
            serde_json::json!({
                "bookId": "book",
                "priority": 8,
                "groupId": null,
                "uniqueId": "DELETE_BOOK_book",
            })
        );

        let series = TaskRecord::delete_series("series");
        assert_eq!(series.id, "DELETE_SERIES_series");
        assert_eq!(series.priority, 8);
        assert_eq!(series.group_id, None);
        assert_eq!(series.class, "org.gotson.komga.application.tasks.Task$DeleteSeries");
        assert_eq!(series.simple_type, "DeleteSeries");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&series.payload).unwrap(),
            serde_json::json!({
                "seriesId": "series",
                "priority": 8,
                "groupId": null,
                "uniqueId": "DELETE_SERIES_series",
            })
        );
    }

    #[test]
    fn source_import_tasks_match_kotlin_payloads() {
        let task = TaskRecord::import_book(
            "/imports/book.cbz",
            "series",
            komga_core::CopyMode::Hardlink,
            Some("Volume 2"),
            Some("old"),
        );
        assert_eq!(task.id, "IMPORT_BOOK_series_/imports/book.cbz");
        assert_eq!(task.priority, 8);
        assert_eq!(task.group_id.as_deref(), Some("series"));
        assert_eq!(task.class, "org.gotson.komga.application.tasks.Task$ImportBook");
        assert_eq!(task.simple_type, "ImportBook");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&task.payload).unwrap(),
            serde_json::json!({
                "sourceFile": "/imports/book.cbz",
                "seriesId": "series",
                "copyMode": "HARDLINK",
                "destinationName": "Volume 2",
                "upgradeBookId": "old",
                "priority": 8,
                "groupId": "series",
                "uniqueId": "IMPORT_BOOK_series_/imports/book.cbz",
            })
        );

        let artwork = TaskRecord::refresh_book_local_artwork("book", 4);
        assert_eq!(artwork.id, "REFRESH_BOOK_LOCAL_ARTWORK_book");
        assert_eq!(artwork.group_id, None);
        assert_eq!(artwork.simple_type, "RefreshBookLocalArtwork");

        let series_artwork = TaskRecord::refresh_series_local_artwork("series", 4);
        assert_eq!(series_artwork.id, "REFRESH_SERIES_LOCAL_ARTWORK_series");
        assert_eq!(series_artwork.priority, 4);
        assert_eq!(series_artwork.group_id, None);
        assert_eq!(
            series_artwork.class,
            "org.gotson.komga.application.tasks.Task$RefreshSeriesLocalArtwork"
        );
        assert_eq!(series_artwork.simple_type, "RefreshSeriesLocalArtwork");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&series_artwork.payload).unwrap(),
            serde_json::json!({
                "seriesId": "series",
                "priority": 4,
                "groupId": null,
                "uniqueId": "REFRESH_SERIES_LOCAL_ARTWORK_series",
            })
        );
    }

    #[tokio::test]
    async fn series_local_artwork_tasks_deduplicate_by_series_id() {
        let tasks = TaskStorage::connect_memory().await.unwrap();
        tasks
            .save(&TaskRecord::refresh_series_local_artwork("series", 4))
            .await
            .unwrap();
        tasks
            .save(&TaskRecord::refresh_series_local_artwork("series", 8))
            .await
            .unwrap();

        let queued = tasks.list().await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].id, "REFRESH_SERIES_LOCAL_ARTWORK_series");
        assert_eq!(queued[0].priority, 8);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&queued[0].payload).unwrap()["priority"],
            8
        );
    }

    #[tokio::test]
    async fn insert_if_absent_preserves_existing_priority_payload_and_owner() {
        let tasks = TaskStorage::connect_memory().await.unwrap();
        let existing = TaskRecord::analyze_book("book", "series", 8);
        tasks.save(&existing).await.unwrap();
        let claimed = tasks
            .take_first_of_type("worker", "AnalyzeBook")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, existing.id);

        assert!(
            !tasks
                .save_if_absent(&TaskRecord::analyze_book("book", "other-series", 0))
                .await
                .unwrap()
        );
        let queued = tasks.list().await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].priority, 8);
        assert_eq!(queued[0].group_id.as_deref(), Some("series"));
        assert_eq!(queued[0].owner.as_deref(), Some("worker"));
        assert_eq!(queued[0].payload, existing.payload);

        assert!(
            tasks
                .save_if_absent(&TaskRecord::analyze_book("new-book", "series", 0))
                .await
                .unwrap()
        );
    }

    #[test]
    fn conversion_tasks_match_kotlin_payloads_and_groups() {
        let discovery = TaskRecord::find_books_to_convert("library", 0);
        assert_eq!(discovery.id, "FIND_BOOKS_TO_CONVERT_library");
        assert_eq!(discovery.group_id, None);
        assert_eq!(
            discovery.class,
            "org.gotson.komga.application.tasks.Task$FindBooksToConvert"
        );
        assert_eq!(discovery.simple_type, "FindBooksToConvert");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&discovery.payload).unwrap(),
            serde_json::json!({
                "libraryId": "library",
                "priority": 0,
                "groupId": null,
                "uniqueId": "FIND_BOOKS_TO_CONVERT_library",
            })
        );

        let conversion = TaskRecord::convert_book("book", "series", 1);
        assert_eq!(conversion.id, "CONVERT_BOOK_book");
        assert_eq!(conversion.group_id.as_deref(), Some("series"));
        assert_eq!(conversion.class, "org.gotson.komga.application.tasks.Task$ConvertBook");
        assert_eq!(conversion.simple_type, "ConvertBook");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&conversion.payload).unwrap(),
            serde_json::json!({
                "bookId": "book",
                "priority": 1,
                "groupId": "series",
                "uniqueId": "CONVERT_BOOK_book",
            })
        );

        let repair = TaskRecord::repair_extension("book", "series", 2);
        assert_eq!(repair.id, "REPAIR_EXTENSION_book");
        assert_eq!(repair.group_id.as_deref(), Some("series"));
        assert_eq!(repair.class, "org.gotson.komga.application.tasks.Task$RepairExtension");
        assert_eq!(repair.simple_type, "RepairExtension");
    }

    #[tokio::test]
    async fn supported_task_claims_choose_global_priority_and_keep_kotlin_payloads() {
        let tasks = TaskStorage::connect_memory().await.unwrap();
        tasks.save(&TaskRecord::empty_trash("library", 8)).await.unwrap();
        tasks
            .save(&TaskRecord::scan_library("library", false, 4))
            .await
            .unwrap();
        tasks
            .save(&TaskRecord::analyze_book("book", "series", 6))
            .await
            .unwrap();
        tasks
            .save(&TaskRecord::generate_book_thumbnail("book", 7))
            .await
            .unwrap();

        let claimed = tasks
            .take_first_of_types(
                "worker",
                &["ScanLibrary", "EmptyTrash", "AnalyzeBook", "GenerateBookThumbnail"],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.simple_type, "EmptyTrash");
        assert_eq!(claimed.priority, 8);
        let payload: serde_json::Value = serde_json::from_str(&claimed.payload).unwrap();
        assert_eq!(payload["libraryId"], "library");
        assert_eq!(tasks.count_by_simple_type().await.unwrap()["EmptyTrash"], 1);

        let numbering = TaskRecord::refresh_book_numbering("book", "series", 4);
        let payload: serde_json::Value = serde_json::from_str(&numbering.payload).unwrap();
        assert_eq!(payload["capabilities"], serde_json::json!(["NUMBER", "NUMBER_SORT"]));

        let discovery = TaskRecord::find_book_thumbnails_to_regenerate(true, 0);
        assert_eq!(discovery.id, "FIND_BOOK_THUMBNAILS_TO_REGENERATE");
        assert_eq!(discovery.simple_type, "FindBookThumbnailsToRegenerate");
        let payload: serde_json::Value = serde_json::from_str(&discovery.payload).unwrap();
        assert_eq!(payload["forBiggerResultOnly"], true);
    }

    #[tokio::test]
    async fn scan_snapshots_preserve_ids_update_media_and_soft_delete_missing_content() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_directory = root.join("Series");
        fs::create_dir_all(&series_directory).unwrap();
        let book_path = series_directory.join("Volume 1.cbz");
        fs::write(&book_path, [1_u8]).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();

        let first_scan = scan_library_files(&library, || false).unwrap();
        let first_stats = storage
            .apply_library_scan(&library.id, &first_scan, false)
            .await
            .unwrap();
        assert_eq!(first_stats.series_added, 1);
        assert_eq!(first_stats.books_added, 1);
        let original_series = storage.list_series_by_library_id(&library.id).await.unwrap()[0].clone();
        let original_book = storage.list_books_by_series_id(&original_series.id).await.unwrap()[0].clone();
        assert_eq!(first_stats.series_ids, BTreeSet::from([original_series.id.clone()]));
        assert_eq!(first_stats.book_ids, BTreeSet::from([original_book.id.clone()]));
        assert_eq!(first_stats.series_added_events[0].series_id, original_series.id);
        assert_eq!(first_stats.book_added_events[0].book_id, original_book.id);

        let unchanged = scan_library_files(&library, || false).unwrap();
        assert_eq!(
            storage.apply_library_scan(&library.id, &unchanged, true).await.unwrap(),
            ScanApplyStats::default()
        );
        assert_eq!(
            storage.list_series_by_library_id(&library.id).await.unwrap()[0].id,
            original_series.id
        );
        assert_eq!(
            storage.list_books_by_series_id(&original_series.id).await.unwrap()[0].id,
            original_book.id
        );

        assert!(storage.set_library_unavailable(&library.id, true).await.unwrap());
        assert!(!storage.set_library_unavailable(&library.id, true).await.unwrap());
        let available_stats = storage
            .apply_library_scan(&library.id, &unchanged, false)
            .await
            .unwrap();
        assert!(available_stats.library_became_available);
        assert!(!storage.set_library_unavailable(&library.id, false).await.unwrap());

        fs::write(&book_path, [1_u8, 2, 3]).unwrap();
        let changed = scan_library_files(&library, || false).unwrap();
        let changed_stats = storage.apply_library_scan(&library.id, &changed, true).await.unwrap();
        assert_eq!(changed_stats.books_updated, 1);
        assert_eq!(changed_stats.book_ids, BTreeSet::from([original_book.id.clone()]));
        assert_eq!(changed_stats.book_updated_events[0].book_id, original_book.id);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT STATUS FROM MEDIA WHERE BOOK_ID = ?")
                .bind(&original_book.id)
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            "OUTDATED"
        );

        fs::remove_file(book_path).unwrap();
        let empty = scan_library_files(&library, || false).unwrap();
        let deleted_stats = storage.apply_library_scan(&library.id, &empty, false).await.unwrap();
        assert_eq!(deleted_stats.books_deleted, 1);
        assert_eq!(deleted_stats.series_deleted, 1);
        assert_eq!(deleted_stats.book_ids, BTreeSet::from([original_book.id.clone()]));
        assert_eq!(deleted_stats.series_ids, BTreeSet::from([original_series.id.clone()]));
        assert_eq!(deleted_stats.book_updated_events[0].book_id, original_book.id);
        assert_eq!(deleted_stats.series_updated_events[0].series_id, original_series.id);
        assert!(
            storage.list_series_by_library_id(&library.id).await.unwrap()[0]
                .deleted_date
                .is_some()
        );
        assert!(
            storage.list_books_by_series_id(&original_series.id).await.unwrap()[0]
                .deleted_date
                .is_some()
        );
    }

    #[tokio::test]
    async fn scan_restores_soft_deleted_book_relations_after_rename() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_directory = root.join("Series");
        fs::create_dir_all(&series_directory).unwrap();
        fs::write(series_directory.join("Keep.cbz"), b"keep").unwrap();
        let old_path = series_directory.join("Old.cbz");
        fs::write(&old_path, b"renamed-content").unwrap();
        let locked_old_path = series_directory.join("Locked Old.cbz");
        fs::write(&locked_old_path, b"locked-renamed-content").unwrap();
        let library = Library::new("Restore book".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();

        let mut initial = scan_library_files(&library, || false).unwrap();
        set_scanned_book_hash(&mut initial, "Old", "same-book-hash");
        set_scanned_book_hash(&mut initial, "Locked Old", "same-locked-book-hash");
        storage.apply_library_scan(&library.id, &initial, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let books = storage.list_books_by_series_id(&series.id).await.unwrap();
        let keep = books.iter().find(|book| book.name == "Keep").unwrap().clone();
        let old = books.iter().find(|book| book.name == "Old").unwrap().clone();
        let locked_old = books.iter().find(|book| book.name == "Locked Old").unwrap().clone();

        let expected_pages = vec![
            BookPage {
                file_name: "001.jpg".into(),
                media_type: "image/jpeg".into(),
                dimension: Some(Dimension {
                    width: 1200,
                    height: 1800,
                }),
                file_hash: "page-hash-1".into(),
                file_size: Some(101),
            },
            BookPage {
                file_name: "002.png".into(),
                media_type: "image/png".into(),
                dimension: None,
                file_hash: "page-hash-2".into(),
                file_size: None,
            },
        ];
        let expected_files = vec![
            MediaFile {
                file_name: "ComicInfo.xml".into(),
                media_type: Some("application/xml".into()),
                sub_type: Some("METADATA".into()),
                file_size: Some(42),
            },
            MediaFile {
                file_name: "notes.txt".into(),
                media_type: None,
                sub_type: None,
                file_size: None,
            },
        ];
        storage
            .replace_media_analysis(&Media {
                status: MediaStatus::Ready,
                media_type: Some("application/zip".into()),
                pages: expected_pages.clone(),
                page_count: 2,
                files: expected_files.clone(),
                comment: Some("analysis warning".into()),
                extension: None,
                book_id: old.id.clone(),
                epub_divina_compatible: true,
                epub_is_kepub: false,
                created_date: None,
                last_modified_date: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "UPDATE MEDIA_PAGE SET FILE_HASH_ALGORITHM = CASE NUMBER \
             WHEN 0 THEN 'RAW_XXH3_128_V1' ELSE 'PDF_RGB24_XXH3_128_V1' END \
             WHERE BOOK_ID = ?",
        )
        .bind(&old.id)
        .execute(storage.pool())
        .await
        .unwrap();
        let mut metadata = storage.find_book_metadata(&old.id).await.unwrap().unwrap();
        metadata.title = "Stale unlocked title".into();
        metadata.title_lock = false;
        metadata.number = "99".into();
        metadata.number_lock = false;
        metadata.number_sort = 99.0;
        metadata.number_sort_lock = false;
        metadata.summary = "Restored summary".into();
        metadata.summary_lock = true;
        metadata.tags.insert("restored-tag".into());
        metadata.tags_lock = true;
        storage.save_book_metadata(&metadata).await.unwrap();
        let mut locked_metadata = storage.find_book_metadata(&locked_old.id).await.unwrap().unwrap();
        locked_metadata.title = "Locked book title".into();
        locked_metadata.title_lock = true;
        storage.save_book_metadata(&locked_metadata).await.unwrap();

        for (thumbnail_type, value) in [("GENERATED", 1_u8), ("USER_UPLOADED", 2)] {
            storage
                .insert_book_thumbnail(
                    &ThumbnailBook {
                        id: new_id(),
                        thumbnail: Some(vec![value]),
                        url: None,
                        selected: false,
                        thumbnail_type: thumbnail_type.into(),
                        media_type: "image/jpeg".into(),
                        file_size: 1,
                        dimension: Dimension { width: 1, height: 1 },
                        book_id: old.id.clone(),
                    },
                    thumbnail_type == "USER_UPLOADED",
                )
                .await
                .unwrap();
        }
        storage
            .upsert_book_sidecar_thumbnail(
                &ThumbnailBook {
                    id: new_id(),
                    thumbnail: None,
                    url: Some("file:///old-sidecar.jpg".into()),
                    selected: false,
                    thumbnail_type: "SIDECAR".into(),
                    media_type: "image/jpeg".into(),
                    file_size: 1,
                    dimension: Dimension { width: 1, height: 1 },
                    book_id: old.id.clone(),
                },
                false,
            )
            .await
            .unwrap();

        let reader = User::new("restore-book@example.org".into(), "unused".into());
        storage.create_user(&reader).await.unwrap();
        storage
            .save_read_progress(&ReadProgress {
                book_id: old.id.clone(),
                user_id: reader.id.clone(),
                page: 4,
                completed: false,
                read_date: Local::now().naive_local(),
                device_id: "device".into(),
                device_name: "Reader".into(),
                locator: None,
            })
            .await
            .unwrap();
        storage
            .create_read_list(
                "restore-read-list",
                &ReadListCreation {
                    name: "Restore read list".into(),
                    summary: String::new(),
                    ordered: true,
                    book_ids: vec![keep.id.clone(), old.id.clone()],
                },
            )
            .await
            .unwrap();

        fs::remove_file(&old_path).unwrap();
        fs::remove_file(&locked_old_path).unwrap();
        let missing = scan_library_files(&library, || false).unwrap();
        let missing_stats = storage.apply_library_scan(&library.id, &missing, false).await.unwrap();
        assert_eq!(missing_stats.books_deleted, 2);
        assert_eq!(
            missing_stats.book_ids,
            BTreeSet::from([old.id.clone(), locked_old.id.clone()])
        );
        assert_eq!(
            missing_stats
                .book_updated_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                (old.id.clone(), series.id.clone(), library.id.clone()),
                (locked_old.id.clone(), series.id.clone(), library.id.clone()),
            ])
        );
        for deleted in [&old, &locked_old] {
            assert!(
                storage
                    .find_book_by_id(&deleted.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .deleted_date
                    .is_some()
            );
        }

        fs::write(series_directory.join("Renamed.cbz"), b"renamed-content").unwrap();
        fs::write(series_directory.join("Locked Renamed.cbz"), b"locked-renamed-content").unwrap();
        let mut restored_scan = scan_library_files(&library, || false).unwrap();
        set_scanned_book_hash(&mut restored_scan, "Renamed", "same-book-hash");
        set_scanned_book_hash(&mut restored_scan, "Locked Renamed", "same-locked-book-hash");
        let restored_stats = storage
            .apply_library_scan(&library.id, &restored_scan, false)
            .await
            .unwrap();

        let active = storage.list_active_books_by_library_id(&library.id).await.unwrap();
        let restored = active.iter().find(|book| book.name == "Renamed").unwrap();
        let locked_restored = active.iter().find(|book| book.name == "Locked Renamed").unwrap();
        assert_ne!(restored.id, old.id);
        assert_ne!(locked_restored.id, locked_old.id);
        assert_eq!(restored.series_id, series.id);
        assert_eq!(locked_restored.series_id, series.id);
        assert_eq!(restored_stats.books_added, 2);
        assert_eq!(restored_stats.books_restored, 2);
        assert_eq!(restored_stats.series_restored, 0);
        assert_eq!(
            restored_stats.book_ids,
            BTreeSet::from([
                old.id.clone(),
                locked_old.id.clone(),
                restored.id.clone(),
                locked_restored.id.clone(),
            ])
        );
        assert_eq!(restored_stats.series_ids, BTreeSet::from([series.id.clone()]));
        assert_eq!(
            restored_stats
                .book_added_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                (restored.id.clone(), series.id.clone(), library.id.clone()),
                (locked_restored.id.clone(), series.id.clone(), library.id.clone()),
            ])
        );
        assert_eq!(
            restored_stats
                .book_deleted_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                (old.id.clone(), series.id.clone(), library.id.clone()),
                (locked_old.id.clone(), series.id.clone(), library.id.clone()),
            ])
        );
        assert!(restored_stats.book_updated_events.is_empty());
        assert!(restored_stats.series_added_events.is_empty());
        assert!(restored_stats.series_deleted_events.is_empty());
        assert_eq!(
            restored_stats.read_list_ids,
            BTreeSet::from(["restore-read-list".into()])
        );
        assert!(restored_stats.collection_ids.is_empty());
        assert_eq!(restored_stats.book_title_refresh_events.len(), 1);
        assert_eq!(restored_stats.book_title_refresh_events[0].book_id, restored.id);
        assert_eq!(restored_stats.book_title_refresh_events[0].series_id, series.id);
        assert_eq!(restored_stats.book_title_refresh_events[0].library_id, library.id);
        assert_eq!(restored_stats.book_numbering_refresh_events.len(), 1);
        assert_eq!(restored_stats.book_numbering_refresh_events[0].book_id, restored.id);
        assert_eq!(
            restored_stats.series_metadata_refresh_ids,
            BTreeSet::from([series.id.clone()])
        );
        assert!(storage.find_book_by_id(&old.id).await.unwrap().is_none());
        assert!(storage.find_book_by_id(&locked_old.id).await.unwrap().is_none());
        assert_eq!(storage.list_books_by_library_id(&library.id).await.unwrap().len(), 3);
        let restored_media = storage.find_media_by_book_id(&restored.id).await.unwrap().unwrap();
        assert_eq!(restored_media.status, MediaStatus::Ready);
        assert_eq!(restored_media.media_type.as_deref(), Some("application/zip"));
        assert_eq!(restored_media.pages, expected_pages);
        assert_eq!(restored_media.page_count, 2);
        assert_eq!(restored_media.files, expected_files);
        assert_eq!(restored_media.comment.as_deref(), Some("analysis warning"));
        assert!(restored_media.extension.is_none());
        assert!(restored_media.epub_divina_compatible);
        assert!(!restored_media.epub_is_kepub);
        assert_eq!(restored_media.book_id, restored.id);
        assert_eq!(
            sqlx::query_as::<
                _,
                (
                    String,
                    String,
                    i32,
                    Option<i32>,
                    Option<i32>,
                    String,
                    Option<i64>,
                    String,
                ),
            >(
                r#"SELECT FILE_NAME, MEDIA_TYPE, NUMBER, WIDTH, HEIGHT,
                          FILE_HASH, FILE_SIZE, FILE_HASH_ALGORITHM
                     FROM MEDIA_PAGE WHERE BOOK_ID = ? ORDER BY NUMBER"#,
            )
            .bind(&restored.id)
            .fetch_all(storage.pool())
            .await
            .unwrap(),
            vec![
                (
                    "001.jpg".into(),
                    "image/jpeg".into(),
                    0,
                    Some(1200),
                    Some(1800),
                    "page-hash-1".into(),
                    Some(101),
                    "RAW_XXH3_128_V1".into(),
                ),
                (
                    "002.png".into(),
                    "image/png".into(),
                    1,
                    None,
                    None,
                    "page-hash-2".into(),
                    None,
                    "PDF_RGB24_XXH3_128_V1".into(),
                ),
            ]
        );
        let restored_metadata = storage.find_book_metadata(&restored.id).await.unwrap().unwrap();
        assert_eq!(restored_metadata.title, "Renamed");
        assert!(!restored_metadata.title_lock);
        assert_eq!(restored_metadata.number, restored.number.to_string());
        assert_eq!(restored_metadata.number_sort, restored.number as f32);
        assert_eq!(restored_metadata.summary, "Restored summary");
        assert!(restored_metadata.tags.contains("restored-tag"));
        let locked_restored_metadata = storage.find_book_metadata(&locked_restored.id).await.unwrap().unwrap();
        assert_eq!(locked_restored_metadata.title, "Locked book title");
        assert!(locked_restored_metadata.title_lock);
        let thumbnail_types = storage
            .list_book_thumbnails(&restored.id)
            .await
            .unwrap()
            .into_iter()
            .map(|thumbnail| thumbnail.thumbnail_type)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            thumbnail_types,
            BTreeSet::from(["GENERATED".into(), "USER_UPLOADED".into()])
        );
        assert_eq!(
            storage
                .find_read_progress(&restored.id, &reader.id)
                .await
                .unwrap()
                .unwrap()
                .page,
            4
        );
        assert!(storage.find_read_progress(&old.id, &reader.id).await.unwrap().is_none());
        let read_list = storage
            .find_read_list("restore-read-list", &reader)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read_list.book_ids, [keep.id.clone(), restored.id.clone()]);
        assert_eq!(restored_stats.read_list_updated_events.len(), 1);
        assert_eq!(
            restored_stats.read_list_updated_events[0].container_id,
            "restore-read-list"
        );
        assert_eq!(
            restored_stats.read_list_updated_events[0].item_ids,
            [keep.id.clone(), restored.id.clone()]
        );
        assert!(restored_stats.collection_updated_events.is_empty());
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(storage.pool())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scan_restores_series_relations_after_directory_move() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let before_directory = root.join("Before Series");
        let old_directory = root.join("Old Series");
        let after_directory = root.join("After Series");
        fs::create_dir_all(&before_directory).unwrap();
        fs::create_dir_all(&old_directory).unwrap();
        fs::create_dir_all(&after_directory).unwrap();
        fs::write(before_directory.join("Before.cbz"), b"before").unwrap();
        fs::write(old_directory.join("Book 1.cbz"), b"one").unwrap();
        fs::write(old_directory.join("Book 2.cbz"), b"two-two").unwrap();
        fs::write(after_directory.join("After.cbz"), b"after").unwrap();
        let library = Library::new("Restore series".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();

        let mut initial = scan_library_files(&library, || false).unwrap();
        set_scanned_book_hash(&mut initial, "Book 1", "book-one-hash");
        set_scanned_book_hash(&mut initial, "Book 2", "book-two-hash");
        storage.apply_library_scan(&library.id, &initial, false).await.unwrap();
        let initial_series = storage.list_series_by_library_id(&library.id).await.unwrap();
        let before_series = initial_series
            .iter()
            .find(|series| series.name == "Before Series")
            .unwrap()
            .clone();
        let old_series = initial_series
            .iter()
            .find(|series| series.name == "Old Series")
            .unwrap()
            .clone();
        let after_series = initial_series
            .iter()
            .find(|series| series.name == "After Series")
            .unwrap()
            .clone();
        let old_books = storage.list_books_by_series_id(&old_series.id).await.unwrap();
        for book in &old_books {
            storage
                .replace_media_analysis(&Media::result(
                    &book.id,
                    MediaStatus::Ready,
                    Some("application/zip".into()),
                    None,
                ))
                .await
                .unwrap();
        }
        let mut metadata = storage.find_series_metadata(&old_series.id).await.unwrap().unwrap();
        metadata.title = "Locked series title".into();
        metadata.title_lock = true;
        metadata.title_sort = "Stale unlocked series sort".into();
        metadata.title_sort_lock = false;
        metadata.summary = "Series summary".into();
        metadata.summary_lock = true;
        metadata.language = "en".into();
        metadata.language_lock = true;
        storage.save_series_metadata(&metadata).await.unwrap();
        storage
            .insert_series_thumbnail(
                &ThumbnailSeries {
                    id: new_id(),
                    thumbnail: Some(vec![3]),
                    url: None,
                    selected: false,
                    thumbnail_type: "USER_UPLOADED".into(),
                    media_type: "image/jpeg".into(),
                    file_size: 1,
                    dimension: Dimension { width: 1, height: 1 },
                    series_id: old_series.id.clone(),
                },
                true,
            )
            .await
            .unwrap();
        storage
            .upsert_series_sidecar_thumbnail(
                &ThumbnailSeries {
                    id: new_id(),
                    thumbnail: None,
                    url: Some("file:///old-series-sidecar.jpg".into()),
                    selected: false,
                    thumbnail_type: "SIDECAR".into(),
                    media_type: "image/jpeg".into(),
                    file_size: 1,
                    dimension: Dimension { width: 1, height: 1 },
                    series_id: old_series.id.clone(),
                },
                false,
            )
            .await
            .unwrap();

        let reader = User::new("restore-series@example.org".into(), "unused".into());
        storage.create_user(&reader).await.unwrap();
        let progressed_book = old_books.iter().find(|book| book.name == "Book 1").unwrap();
        storage
            .save_read_progress(&ReadProgress {
                book_id: progressed_book.id.clone(),
                user_id: reader.id.clone(),
                page: 1,
                completed: true,
                read_date: Local::now().naive_local(),
                device_id: "series-device".into(),
                device_name: "Series reader".into(),
                locator: None,
            })
            .await
            .unwrap();
        let expected_progress = storage
            .find_series_read_progress(&old_series.id, &reader.id)
            .await
            .unwrap();
        assert_eq!(expected_progress.books_count, 2);
        assert_eq!(expected_progress.books_read_count, 1);
        assert_eq!(expected_progress.books_unread_count, 1);
        assert_eq!(expected_progress.books_in_progress_count, 0);
        assert_eq!(expected_progress.last_read_continuous_number_sort, 1.0);
        assert_eq!(expected_progress.max_number_sort, 2.0);
        assert_eq!(
            sqlx::query_as::<_, (i32, i32)>(
                "SELECT READ_COUNT, IN_PROGRESS_COUNT FROM READ_PROGRESS_SERIES \
                 WHERE SERIES_ID = ? AND USER_ID = ?",
            )
            .bind(&old_series.id)
            .bind(&reader.id)
            .fetch_one(storage.pool())
            .await
            .unwrap(),
            (1, 0)
        );
        storage
            .create_collection(
                "restore-collection",
                &CollectionCreation {
                    name: "Restore collection".into(),
                    ordered: true,
                    series_ids: vec![before_series.id.clone(), old_series.id.clone(), after_series.id.clone()],
                },
            )
            .await
            .unwrap();

        let staged_directory = temporary.path().join("staged-series");
        fs::rename(&old_directory, &staged_directory).unwrap();
        let missing_scan = scan_library_files(&library, || false).unwrap();
        let missing_stats = storage
            .apply_library_scan(&library.id, &missing_scan, false)
            .await
            .unwrap();
        assert_eq!(missing_stats.series_deleted, 1);
        assert_eq!(missing_stats.books_deleted, 2);
        assert_eq!(missing_stats.series_ids, BTreeSet::from([old_series.id.clone()]));
        assert_eq!(
            missing_stats.book_ids,
            old_books.iter().map(|book| book.id.clone()).collect()
        );
        assert_eq!(
            missing_stats
                .series_updated_events
                .iter()
                .map(|event| (event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([(old_series.id.clone(), library.id.clone())])
        );
        assert_eq!(
            missing_stats
                .book_updated_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            old_books
                .iter()
                .map(|book| (book.id.clone(), old_series.id.clone(), library.id.clone()))
                .collect()
        );
        assert!(
            storage
                .find_series_by_id(&old_series.id)
                .await
                .unwrap()
                .unwrap()
                .deleted_date
                .is_some()
        );
        for book in &old_books {
            assert!(
                storage
                    .find_book_by_id(&book.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .deleted_date
                    .is_some()
            );
        }
        assert_eq!(
            sqlx::query_as::<_, (i32, i32)>(
                "SELECT READ_COUNT, IN_PROGRESS_COUNT FROM READ_PROGRESS_SERIES \
                 WHERE SERIES_ID = ? AND USER_ID = ?",
            )
            .bind(&old_series.id)
            .bind(&reader.id)
            .fetch_one(storage.pool())
            .await
            .unwrap(),
            (1, 0)
        );

        let new_directory = root.join("New Series");
        fs::rename(&staged_directory, &new_directory).unwrap();
        let mut moved_scan = scan_library_files(&library, || false).unwrap();
        set_scanned_book_hash(&mut moved_scan, "Book 1", "book-one-hash");
        set_scanned_book_hash(&mut moved_scan, "Book 2", "book-two-hash");
        let restored_stats = storage
            .apply_library_scan(&library.id, &moved_scan, false)
            .await
            .unwrap();

        let series = storage.list_series_by_library_id(&library.id).await.unwrap();
        assert_eq!(series.len(), 3);
        let restored_series = series.iter().find(|series| series.name == "New Series").unwrap();
        assert_eq!(restored_series.name, "New Series");
        assert_ne!(restored_series.id, old_series.id);
        assert_eq!(restored_stats.series_added, 1);
        assert_eq!(restored_stats.series_restored, 1);
        assert_eq!(restored_stats.books_added, 2);
        assert_eq!(restored_stats.books_restored, 2);
        assert_eq!(
            restored_stats.series_ids,
            BTreeSet::from([old_series.id.clone(), restored_series.id.clone()])
        );
        assert_eq!(
            restored_stats.collection_ids,
            BTreeSet::from(["restore-collection".into()])
        );
        assert_eq!(
            restored_stats.series_metadata_refresh_ids,
            BTreeSet::from([restored_series.id.clone()])
        );
        assert!(restored_stats.read_list_ids.is_empty());
        assert!(storage.find_series_by_id(&old_series.id).await.unwrap().is_none());
        for old_book in &old_books {
            assert!(storage.find_book_by_id(&old_book.id).await.unwrap().is_none());
        }
        let restored_books = storage.list_books_by_series_id(&restored_series.id).await.unwrap();
        assert_eq!(restored_books.len(), 2);
        assert!(restored_books.iter().all(|book| book.deleted_date.is_none()));
        assert_eq!(
            restored_stats.book_ids,
            old_books
                .iter()
                .map(|book| book.id.clone())
                .chain(restored_books.iter().map(|book| book.id.clone()))
                .collect()
        );
        assert_eq!(
            restored_stats
                .series_added_events
                .iter()
                .map(|event| (event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([(restored_series.id.clone(), library.id.clone())])
        );
        assert_eq!(
            restored_stats
                .series_deleted_events
                .iter()
                .map(|event| (event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([(old_series.id.clone(), library.id.clone())])
        );
        assert_eq!(
            restored_stats
                .book_added_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            restored_books
                .iter()
                .map(|book| { (book.id.clone(), restored_series.id.clone(), library.id.clone(),) })
                .collect()
        );
        assert_eq!(
            restored_stats
                .book_deleted_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            old_books
                .iter()
                .map(|book| (book.id.clone(), old_series.id.clone(), library.id.clone()))
                .collect()
        );
        assert!(restored_stats.book_updated_events.is_empty());
        assert!(restored_stats.series_updated_events.is_empty());
        assert_eq!(
            restored_stats
                .book_title_refresh_events
                .iter()
                .map(|event| (event.book_id.clone(), event.series_id.clone(), event.library_id.clone()))
                .collect::<BTreeSet<_>>(),
            restored_books
                .iter()
                .map(|book| { (book.id.clone(), restored_series.id.clone(), library.id.clone(),) })
                .collect()
        );
        for book in &restored_books {
            assert_eq!(
                storage.find_media_by_book_id(&book.id).await.unwrap().unwrap().status,
                MediaStatus::Ready
            );
        }
        let restored_metadata = storage
            .find_series_metadata(&restored_series.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored_metadata.title, "Locked series title");
        assert!(restored_metadata.title_lock);
        assert_eq!(restored_metadata.title_sort, "New Series");
        assert!(!restored_metadata.title_sort_lock);
        assert_eq!(restored_metadata.summary, "Series summary");
        assert_eq!(restored_metadata.language, "en");
        let thumbnail_types = storage
            .list_series_thumbnails(&restored_series.id)
            .await
            .unwrap()
            .into_iter()
            .map(|thumbnail| thumbnail.thumbnail_type)
            .collect::<BTreeSet<_>>();
        assert_eq!(thumbnail_types, BTreeSet::from(["USER_UPLOADED".into()]));
        let collection = storage
            .find_collection("restore-collection", &reader)
            .await
            .unwrap()
            .unwrap();
        let expected_collection_ids = vec![
            before_series.id.clone(),
            restored_series.id.clone(),
            after_series.id.clone(),
        ];
        assert_eq!(collection.series_ids, expected_collection_ids);
        assert_eq!(restored_stats.collection_updated_events.len(), 1);
        assert_eq!(
            restored_stats.collection_updated_events[0].container_id,
            "restore-collection"
        );
        assert_eq!(
            restored_stats.collection_updated_events[0].item_ids,
            expected_collection_ids
        );
        assert!(restored_stats.read_list_updated_events.is_empty());

        let restored_progress = storage
            .find_series_read_progress(&restored_series.id, &reader.id)
            .await
            .unwrap();
        assert_eq!(restored_progress, expected_progress);
        assert_eq!(
            sqlx::query_as::<_, (i32, i32)>(
                "SELECT READ_COUNT, IN_PROGRESS_COUNT FROM READ_PROGRESS_SERIES \
                 WHERE SERIES_ID = ? AND USER_ID = ?",
            )
            .bind(&restored_series.id)
            .bind(&reader.id)
            .fetch_one(storage.pool())
            .await
            .unwrap(),
            (1, 0)
        );
        assert!(
            sqlx::query_scalar::<_, String>(
                "SELECT SERIES_ID FROM READ_PROGRESS_SERIES WHERE SERIES_ID = ? AND USER_ID = ?",
            )
            .bind(&old_series.id)
            .bind(&reader.id)
            .fetch_optional(storage.pool())
            .await
            .unwrap()
            .is_none()
        );
        let restored_progress_book = restored_books.iter().find(|book| book.name == "Book 1").unwrap();
        let progress = storage
            .find_read_progress(&restored_progress_book.id, &reader.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progress.page, 1);
        assert!(progress.completed);
        assert!(
            storage
                .find_read_progress(&progressed_book.id, &reader.id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(storage.pool())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn scan_snapshots_track_each_sidecar_consumer_only_for_additions_and_changes() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_directory = root.join("Series");
        fs::create_dir_all(&series_directory).unwrap();
        fs::write(series_directory.join("Book.cbz"), b"book").unwrap();
        fs::write(series_directory.join("series.json"), b"{}").unwrap();
        fs::write(series_directory.join("Book.jpg"), b"book artwork").unwrap();
        fs::write(series_directory.join("cover.jpg"), b"series artwork").unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();

        let first_scan = scan_library_files(&library, || false).unwrap();
        let first = storage
            .apply_library_scan(&library.id, &first_scan, false)
            .await
            .unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let book = storage.list_books_by_series_id(&series.id).await.unwrap().remove(0);
        assert_eq!(first.sidecars_changed.as_slice(), std::slice::from_ref(&series.id));
        assert_eq!(first.book_artwork_changed.as_slice(), std::slice::from_ref(&book.id));
        assert_eq!(
            first.series_artwork_changed.as_slice(),
            std::slice::from_ref(&series.id)
        );

        let unchanged = scan_library_files(&library, || false).unwrap();
        assert_eq!(
            storage
                .apply_library_scan(&library.id, &unchanged, false)
                .await
                .unwrap(),
            ScanApplyStats::default()
        );

        let mut changed_scan = unchanged.clone();
        for sidecar in &mut changed_scan.sidecars {
            sidecar.last_modified_time += chrono::Duration::seconds(1);
        }
        let changed = storage
            .apply_library_scan(&library.id, &changed_scan, false)
            .await
            .unwrap();
        assert_eq!(changed.sidecars_changed.as_slice(), std::slice::from_ref(&series.id));
        assert_eq!(changed.book_artwork_changed.as_slice(), std::slice::from_ref(&book.id));
        assert_eq!(
            changed.series_artwork_changed.as_slice(),
            std::slice::from_ref(&series.id)
        );

        let mut removed = changed_scan;
        removed.sidecars.retain(|sidecar| {
            !(sidecar.source == komga_core::SidecarSource::Series
                && sidecar.sidecar_type == komga_core::SidecarType::Artwork)
        });
        assert!(
            storage
                .apply_library_scan(&library.id, &removed, false)
                .await
                .unwrap()
                .series_artwork_changed
                .is_empty()
        );
    }

    #[tokio::test]
    async fn legacy_series_regex_filters_ranked_queries_groups_and_permissions() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        for name in ["Alpha", "Beta"] {
            let directory = root.join(name);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("Volume.cbz"), [1_u8]).unwrap();
        }
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();

        let mut series = storage.list_series_by_library_id(&library.id).await.unwrap();
        series.sort_by(|left, right| left.name.cmp(&right.name));
        let alpha = &series[0];
        let beta = &series[1];
        sqlx::query("UPDATE SERIES_METADATA SET TITLE = 'Café Stories', TITLE_SORT = 'Zulu Alpha' WHERE SERIES_ID = ?")
            .bind(&alpha.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE SERIES_METADATA SET TITLE = 'Other', TITLE_SORT = 'beta' WHERE SERIES_ID = ?")
            .bind(&beta.id)
            .execute(storage.pool())
            .await
            .unwrap();

        let user = User::new("reader@example.org".into(), "hash".into());
        let title = LegacySeriesRegex {
            pattern: "^CAFÉ".into(),
            field: LegacySeriesRegexField::Title,
        };
        let title_sort = LegacySeriesRegex {
            pattern: "^zulu".into(),
            field: LegacySeriesRegexField::TitleSort,
        };
        let unpaged = PageRequest {
            unpaged: true,
            ..PageRequest::default()
        };

        let title_page = storage
            .query_series_legacy(&SeriesSearch::default(), &unpaged, &user, None, Some(&title))
            .await
            .unwrap();
        assert_eq!(
            title_page.content.iter().map(|series| &series.id).collect::<Vec<_>>(),
            [&alpha.id]
        );
        let sort_page = storage
            .query_series_legacy(&SeriesSearch::default(), &unpaged, &user, None, Some(&title_sort))
            .await
            .unwrap();
        assert_eq!(sort_page.content[0].id, alpha.id);

        assert_eq!(
            storage
                .query_series_legacy(
                    &SeriesSearch::default(),
                    &unpaged,
                    &user,
                    Some(&[beta.id.clone(), alpha.id.clone()]),
                    Some(&title),
                )
                .await
                .unwrap()
                .total_elements,
            1
        );
        assert_eq!(
            storage
                .query_series_legacy(&SeriesSearch::default(), &unpaged, &user, Some(&[]), Some(&title))
                .await
                .unwrap()
                .total_elements,
            0
        );

        let groups = storage
            .series_alphabetical_groups_legacy(&SeriesSearch::default(), &user, None, Some(&title))
            .await
            .unwrap();
        assert_eq!(
            groups,
            [komga_core::GroupCountDto {
                group: "z".into(),
                count: 1
            }]
        );

        let mut restricted = user.clone();
        restricted.shared_all_libraries = false;
        assert_eq!(
            storage
                .query_series_legacy(&SeriesSearch::default(), &unpaged, &restricted, None, Some(&title))
                .await
                .unwrap()
                .total_elements,
            0
        );
        assert!(
            storage
                .series_alphabetical_groups_legacy(&SeriesSearch::default(), &restricted, None, Some(&title))
                .await
                .unwrap()
                .is_empty()
        );

        let invalid = LegacySeriesRegex {
            pattern: "(?=Stories)".into(),
            field: LegacySeriesRegexField::Title,
        };
        assert!(matches!(
            storage
                .query_series_legacy(&SeriesSearch::default(), &unpaged, &user, None, Some(&invalid))
                .await,
            Err(StorageError::Database(_))
        ));
    }

    #[tokio::test]
    async fn content_queries_hydrate_filter_page_and_apply_restrictions() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let series_directory = root.join("Series");
        fs::create_dir_all(&series_directory).unwrap();
        fs::write(series_directory.join("Volume 1.cbz"), vec![0_u8; 1536]).unwrap();
        fs::write(series_directory.join("Volume 2.cbz"), vec![0_u8; 2048]).unwrap();
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();

        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let books = storage.list_books_by_series_id(&series.id).await.unwrap();
        let mut user = User::new("reader@example.org".into(), "$2b$10$hash".into());
        storage.create_user(&user).await.unwrap();

        sqlx::query("UPDATE SERIES_METADATA SET AGE_RATING = 16 WHERE SERIES_ID = ?")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO SERIES_METADATA_GENRE (SERIES_ID, GENRE) VALUES (?, 'Fantasy')")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO SERIES_METADATA_TAG (SERIES_ID, TAG) VALUES (?, 'Favorite')")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO SERIES_METADATA_SHARING (SERIES_ID, LABEL) VALUES (?, 'teen')")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO SERIES_METADATA_LINK (SERIES_ID, LABEL, URL) VALUES (?, 'Source', 'https://example.org')",
        )
        .bind(&series.id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO SERIES_METADATA_ALTERNATE_TITLE (SERIES_ID, LABEL, TITLE) VALUES (?, 'ja', 'Alternative')",
        )
        .bind(&series.id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO BOOK_METADATA_AGGREGATION_AUTHOR (SERIES_ID, NAME, ROLE) VALUES (?, 'Author', 'writer')",
        )
        .bind(&series.id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query("INSERT INTO BOOK_METADATA_AGGREGATION_TAG (SERIES_ID, TAG) VALUES (?, 'Collected')")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();

        sqlx::query(
            "UPDATE SERIES_METADATA SET TOTAL_BOOK_COUNT = 2, PUBLISHER = 'ＭＡＲＶＥＬ', LANGUAGE = 'EN' WHERE SERIES_ID = ?",
        )
        .bind(&series.id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query("UPDATE BOOK_METADATA_AGGREGATION SET RELEASE_DATE = '2024-01-15' WHERE SERIES_ID = ?")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();

        for book in &books {
            sqlx::query(
                "UPDATE MEDIA SET STATUS = 'READY', MEDIA_TYPE = 'application/pdf', PAGE_COUNT = 4 WHERE BOOK_ID = ?",
            )
            .bind(&book.id)
            .execute(storage.pool())
            .await
            .unwrap();
        }
        sqlx::query("INSERT INTO BOOK_METADATA_AUTHOR (BOOK_ID, NAME, ROLE) VALUES (?, 'Author', 'writer')")
            .bind(&books[0].id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO BOOK_METADATA_TAG (BOOK_ID, TAG) VALUES (?, 'BookTag')")
            .bind(&books[0].id)
            .execute(storage.pool())
            .await
            .unwrap();
        let kotlin_number_sort = f64::from(0.1_f32);
        sqlx::query("UPDATE BOOK_METADATA SET RELEASE_DATE = '2024-01-10', NUMBER_SORT = ? WHERE BOOK_ID = ?")
            .bind(kotlin_number_sort)
            .bind(&books[0].id)
            .execute(storage.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO THUMBNAIL_BOOK (ID, SELECTED, TYPE, BOOK_ID) VALUES ('query-poster', 1, 'SIDECAR', ?)",
        )
        .bind(&books[0].id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO BOOK_METADATA_LINK (BOOK_ID, LABEL, URL) VALUES (?, 'Info', 'https://example.org/book')",
        )
        .bind(&books[0].id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO READ_PROGRESS (BOOK_ID, USER_ID, PAGE, COMPLETED, DEVICE_ID, DEVICE_NAME) VALUES (?, ?, 3, 1, 'device', 'Reader')",
        )
        .bind(&books[0].id)
        .bind(&user.id)
        .execute(storage.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO READ_PROGRESS_SERIES (SERIES_ID, USER_ID, READ_COUNT, IN_PROGRESS_COUNT) VALUES (?, ?, 1, 0)",
        )
        .bind(&series.id)
        .bind(&user.id)
        .execute(storage.pool())
        .await
        .unwrap();

        let series_page = storage
            .query_series(&SeriesSearch::default(), &PageRequest::default(), &user, None)
            .await
            .unwrap();
        assert_eq!(series_page.total_elements, 1);
        let dto = &series_page.content[0];
        assert_eq!(dto.books_read_count, 1);
        assert_eq!(dto.books_unread_count, 1);
        assert!(dto.metadata.genres.contains("Fantasy"));
        assert_eq!(dto.metadata.links[0].label, "Source");
        assert_eq!(dto.books_metadata.authors[0].name, "Author");

        let structured_series: SeriesSearch = serde_json::from_value(serde_json::json!({
            "condition": {
                "allOf": [
                    {"complete": {"operator": "isTrue"}},
                    {"releaseDate": {"operator": "after", "dateTime": "2024-01-01T23:00:00-05:00"}},
                    {"tag": {"operator": "is", "value": "collected"}},
                    {"sharingLabel": {"operator": "isNotNull"}},
                    {"publisher": {"operator": "is", "value": "marvel"}},
                    {"language": {"operator": "is", "value": "en"}},
                    {"genre": {"operator": "is", "value": "fantasy"}},
                    {"ageRating": {"operator": "greaterThan", "value": 12}},
                    {"author": {"operator": "is", "value": {"name": "author", "role": "WRITER"}}}
                ]
            }
        }))
        .unwrap();
        assert_eq!(
            storage
                .query_series(
                    &structured_series,
                    &PageRequest::default(),
                    &user,
                    Some(std::slice::from_ref(&series.id))
                )
                .await
                .unwrap()
                .total_elements,
            1
        );
        let empty_author: SeriesSearch = serde_json::from_value(serde_json::json!({
            "condition": {"author": {"operator": "isNot", "value": {}}}
        }))
        .unwrap();
        assert_eq!(
            storage
                .query_series(&empty_author, &PageRequest::default(), &user, None)
                .await
                .unwrap()
                .total_elements,
            1
        );

        let structured_book: BookSearch = serde_json::from_value(serde_json::json!({
            "condition": {
                "allOf": [
                    {"releaseDate": {"operator": "before", "dateTime": "2024-02-01T00:00:00Z"}},
                    {"tag": {"operator": "is", "value": "booktag"}},
                    {"numberSort": {"operator": "is", "value": 0.1}},
                    {"mediaProfile": {"operator": "is", "value": "PDF"}},
                    {"author": {"operator": "is", "value": {"name": "AUTHOR"}}},
                    {"poster": {"operator": "is", "value": {"type": "SIDECAR", "selected": true}}}
                ]
            }
        }))
        .unwrap();
        assert_eq!(
            storage
                .query_books(
                    &structured_book,
                    &PageRequest::default(),
                    &user,
                    Some(&[books[0].id.clone(), books[1].id.clone()])
                )
                .await
                .unwrap()
                .content[0]
                .id,
            books[0].id
        );
        let absent_tag: BookSearch = serde_json::from_value(serde_json::json!({
            "condition": {"tag": {"operator": "isNull"}}
        }))
        .unwrap();
        assert_eq!(
            storage
                .query_books(&absent_tag, &PageRequest::default(), &user, None)
                .await
                .unwrap()
                .content[0]
                .id,
            books[1].id
        );
        let empty_poster: BookSearch = serde_json::from_value(serde_json::json!({
            "condition": {"poster": {"operator": "is", "value": {}}}
        }))
        .unwrap();
        assert_eq!(
            storage
                .query_books(&empty_poster, &PageRequest::default(), &user, None)
                .await
                .unwrap()
                .total_elements,
            2
        );

        let groups = storage
            .series_alphabetical_groups(&SeriesSearch::default(), &user, None)
            .await
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group, "s");
        assert_eq!(groups[0].count, 1);
        assert!(
            storage
                .series_alphabetical_groups(&SeriesSearch::default(), &user, Some(&[]))
                .await
                .unwrap()
                .is_empty()
        );

        let updated_page = PageRequest {
            sort: vec![SortOrder {
                property: "lastModified".into(),
                direction: SortDirection::Descending,
            }],
            ..PageRequest::default()
        };
        assert_eq!(
            storage
                .query_recently_updated_series(&SeriesSearch::default(), &updated_page, &user)
                .await
                .unwrap()
                .total_elements,
            0
        );
        sqlx::query("UPDATE SERIES SET LAST_MODIFIED_DATE = datetime(CREATED_DATE, '+1 day') WHERE ID = ?")
            .bind(&series.id)
            .execute(storage.pool())
            .await
            .unwrap();
        assert_eq!(
            storage
                .query_recently_updated_series(&SeriesSearch::default(), &updated_page, &user)
                .await
                .unwrap()
                .total_elements,
            1
        );

        let on_deck = storage
            .query_books_on_deck(&[], &PageRequest::default(), &user)
            .await
            .unwrap();
        assert_eq!(on_deck.total_elements, 1);
        assert_eq!(on_deck.content[0].id, books[1].id);

        let read_search = BookSearch {
            condition: Some(BookCondition::ReadStatus {
                operator: EqualityOperator::Is {
                    value: ReadStatus::Read,
                },
            }),
            full_text_search: None,
        };
        let page_request = PageRequest {
            page: 0,
            size: 1,
            unpaged: false,
            sort: vec![SortOrder {
                property: "metadata.numberSort".into(),
                direction: SortDirection::Ascending,
            }],
        };
        let book_page = storage
            .query_books(&read_search, &page_request, &user, None)
            .await
            .unwrap();
        assert_eq!(book_page.total_elements, 1);
        assert_eq!(book_page.content[0].media.media_profile, "PDF");
        assert_eq!(book_page.content[0].metadata.tags, BTreeSet::from(["BookTag".into()]));
        assert_eq!(book_page.content[0].read_progress.as_ref().unwrap().device_id, "device");
        assert_eq!(book_page.content[0].size, "1.5 KiB");
        assert_eq!(book_page.content[0].metadata.number_sort, 0.1_f32);
        assert!(
            serde_json::to_string(&book_page.content[0])
                .unwrap()
                .contains(r#""numberSort":0.1"#)
        );
        let metadata = storage.find_book_metadata(&books[0].id).await.unwrap().unwrap();
        storage.save_book_metadata(&metadata).await.unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, f64>("SELECT NUMBER_SORT FROM BOOK_METADATA WHERE BOOK_ID = ?")
                .bind(&books[0].id)
                .fetch_one(storage.pool())
                .await
                .unwrap(),
            kotlin_number_sort
        );
        let ranked = vec![books[1].id.clone(), books[0].id.clone()];
        let ranked_page = storage
            .query_books(
                &BookSearch::default(),
                &PageRequest {
                    page: 0,
                    size: 1,
                    ..PageRequest::default()
                },
                &user,
                Some(&ranked),
            )
            .await
            .unwrap();
        assert_eq!(ranked_page.total_elements, 2);
        assert_eq!(ranked_page.content[0].id, books[1].id);
        let explicitly_sorted = storage
            .query_books(
                &BookSearch::default(),
                &PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "metadata.numberSort".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..PageRequest::default()
                },
                &user,
                Some(&ranked),
            )
            .await
            .unwrap();
        assert_eq!(explicitly_sorted.content[0].id, books[0].id);
        assert_eq!(
            storage
                .query_books(&BookSearch::default(), &PageRequest::default(), &user, Some(&[]))
                .await
                .unwrap()
                .total_elements,
            0
        );
        assert!(
            storage
                .query_books(
                    &BookSearch::default(),
                    &PageRequest {
                        sort: vec![SortOrder {
                            property: "relevance".into(),
                            direction: SortDirection::Ascending,
                        }],
                        ..PageRequest::default()
                    },
                    &user,
                    None,
                )
                .await
                .is_err()
        );

        for (id, name) in [("collection-a", "Alpha"), ("collection-z", "Zulu")] {
            storage
                .create_collection(
                    id,
                    &CollectionCreation {
                        name: name.into(),
                        ordered: false,
                        series_ids: vec![series.id.clone()],
                    },
                )
                .await
                .unwrap();
        }
        let container_rank = ["collection-z".into(), "collection-a".into()];
        let collection_page = storage
            .list_collections(
                None,
                Some(&library.id),
                Some(&container_rank),
                &PageRequest {
                    page: 0,
                    size: 1,
                    ..PageRequest::default()
                },
                &user,
            )
            .await
            .unwrap();
        assert_eq!(collection_page.total_elements, 2);
        assert_eq!(collection_page.content[0].id, "collection-z");
        let explicitly_sorted = storage
            .list_collections(
                None,
                None,
                Some(&container_rank),
                &PageRequest {
                    unpaged: true,
                    sort: vec![SortOrder {
                        property: "name".into(),
                        direction: SortDirection::Ascending,
                    }],
                    ..PageRequest::default()
                },
                &user,
            )
            .await
            .unwrap();
        assert_eq!(explicitly_sorted.content[0].id, "collection-a");
        assert_eq!(
            storage
                .list_collections(
                    None,
                    Some("missing"),
                    Some(&container_rank),
                    &PageRequest::default(),
                    &user
                )
                .await
                .unwrap()
                .total_elements,
            0
        );
        assert_eq!(
            storage
                .list_collections(None, None, Some(&[]), &PageRequest::default(), &user)
                .await
                .unwrap()
                .total_elements,
            0
        );

        for (id, name) in [("readlist-a", "Alpha"), ("readlist-z", "Zulu")] {
            storage
                .create_read_list(
                    id,
                    &ReadListCreation {
                        name: name.into(),
                        summary: String::new(),
                        ordered: true,
                        book_ids: vec![books[0].id.clone()],
                    },
                )
                .await
                .unwrap();
        }
        let read_list_rank = ["readlist-z".into(), "readlist-a".into()];
        let read_list_page = storage
            .list_read_lists(
                None,
                None,
                Some(&read_list_rank),
                &PageRequest {
                    page: 0,
                    size: 1,
                    ..PageRequest::default()
                },
                &user,
            )
            .await
            .unwrap();
        assert_eq!(read_list_page.total_elements, 2);
        assert_eq!(read_list_page.content[0].id, "readlist-z");
        assert_eq!(
            storage
                .list_read_lists(
                    None,
                    Some("missing"),
                    Some(&read_list_rank),
                    &PageRequest::default(),
                    &user
                )
                .await
                .unwrap()
                .total_elements,
            0
        );
        assert_eq!(
            storage
                .list_read_lists(None, None, Some(&[]), &PageRequest::default(), &user)
                .await
                .unwrap()
                .total_elements,
            0
        );
        assert_eq!(
            storage
                .find_book_sibling_dto(&books[0].id, &user.id, true)
                .await
                .unwrap()
                .unwrap()
                .id,
            books[1].id
        );

        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 12,
                restriction: AllowExclude::AllowOnly,
            }),
            BTreeSet::new(),
            BTreeSet::new(),
        );
        assert_eq!(
            storage
                .query_series(&SeriesSearch::default(), &PageRequest::default(), &user, None)
                .await
                .unwrap()
                .total_elements,
            0
        );
        user.restrictions = ContentRestrictions::default();
        user.shared_all_libraries = false;
        assert_eq!(
            storage
                .query_books(&BookSearch::default(), &PageRequest::default(), &user, None)
                .await
                .unwrap()
                .total_elements,
            0
        );
        assert_eq!(
            storage
                .query_books_on_deck(&[], &PageRequest::default(), &user)
                .await
                .unwrap()
                .total_elements,
            0
        );

        let media_search = BookSearch {
            condition: Some(BookCondition::MediaStatus {
                operator: EqualityOperator::Is {
                    value: MediaStatus::Ready,
                },
            }),
            full_text_search: None,
        };
        assert_eq!(
            storage
                .query_books(
                    &media_search,
                    &PageRequest::default(),
                    &User::new("all@example.org".into(), "hash".into()),
                    None,
                )
                .await
                .unwrap()
                .total_elements,
            2
        );
    }

    #[tokio::test]
    async fn on_deck_requires_read_progress_and_uses_stable_unread_order() {
        let storage = Storage::connect_memory().await.unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("library");
        let directory = root.join("Series");
        fs::create_dir_all(&directory).unwrap();
        for number in 1..=3 {
            fs::write(directory.join(format!("Volume {number}.cbz")), [number]).unwrap();
        }
        let library = Library::new("Library".into(), Url::from_directory_path(&root).unwrap());
        storage.create_library(&library).await.unwrap();
        let scan = scan_library_files(&library, || false).unwrap();
        storage.apply_library_scan(&library.id, &scan, false).await.unwrap();
        let series = storage.list_series_by_library_id(&library.id).await.unwrap().remove(0);
        let mut books = storage.list_books_by_series_id(&series.id).await.unwrap();
        books.sort_by(|left, right| left.name.cmp(&right.name));
        for book in &books[1..] {
            sqlx::query("UPDATE BOOK_METADATA SET NUMBER_SORT = 2 WHERE BOOK_ID = ?")
                .bind(&book.id)
                .execute(storage.pool())
                .await
                .unwrap();
        }

        let user = User::new("reader@example.org".into(), "hash".into());
        storage.create_user(&user).await.unwrap();
        assert_eq!(
            storage
                .query_books_on_deck(&[], &PageRequest::default(), &user)
                .await
                .unwrap()
                .total_elements,
            0
        );

        let progress = |book_id: &str, completed| komga_core::ReadProgress {
            book_id: book_id.to_owned(),
            user_id: user.id.clone(),
            page: 1,
            completed,
            read_date: chrono::Utc::now().naive_utc(),
            device_id: String::new(),
            device_name: String::new(),
            locator: None,
        };
        storage.save_read_progress(&progress(&books[0].id, true)).await.unwrap();
        let expected = [&books[1].id, &books[2].id].into_iter().min().unwrap();
        let on_deck = storage
            .query_books_on_deck(&[], &PageRequest::default(), &user)
            .await
            .unwrap();
        assert_eq!(on_deck.total_elements, 1);
        assert_eq!(&on_deck.content[0].id, expected);
        assert_eq!(
            storage
                .query_books_on_deck(
                    std::slice::from_ref(&"missing".to_owned()),
                    &PageRequest::default(),
                    &user
                )
                .await
                .unwrap()
                .total_elements,
            0
        );

        storage.save_read_progress(&progress(expected, false)).await.unwrap();
        assert_eq!(
            storage
                .query_books_on_deck(&[], &PageRequest::default(), &user)
                .await
                .unwrap()
                .total_elements,
            0
        );
        storage.delete_read_progress(expected, &user.id).await.unwrap();

        for book in &books[1..] {
            storage.save_read_progress(&progress(&book.id, true)).await.unwrap();
        }
        assert_eq!(
            storage
                .query_books_on_deck(&[], &PageRequest::default(), &user)
                .await
                .unwrap()
                .total_elements,
            0
        );
    }

    #[tokio::test]
    async fn non_empty_library_deletion_cascades() {
        let storage = Storage::connect_memory().await.unwrap();
        let root = tempfile::tempdir().unwrap();
        let library = Library::new("Library".into(), Url::from_file_path(root.path()).unwrap());
        storage.create_library(&library).await.unwrap();
        sqlx::query(
            "INSERT INTO SERIES (ID, FILE_LAST_MODIFIED, NAME, URL, LIBRARY_ID) VALUES ('series', CURRENT_TIMESTAMP, 'Series', 'file:/tmp/series', ?)",
        )
            .bind(&library.id)
            .execute(storage.pool())
            .await
            .unwrap();

        assert!(matches!(
            storage.delete_library(&library.id).await.unwrap(),
            DeleteLibraryResult::Deleted { .. }
        ));
        assert!(storage.find_library_by_id(&library.id).await.unwrap().is_none());
    }
}
