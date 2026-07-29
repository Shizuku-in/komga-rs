use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, SqlitePool, migrate::Migrator, sqlite::SqlitePoolOptions};

use crate::{Result, StorageError};

pub(crate) static MAIN_MIGRATOR: Migrator = sqlx::migrate!("./migrations");
pub(crate) static TASK_MIGRATOR: Migrator = sqlx::migrate!("./tasks_migrations");

const MAIN_FLYWAY_FINGERPRINT: [u8; 32] = [
    0x5f, 0x1f, 0x5e, 0xef, 0xfc, 0x26, 0x31, 0x35, 0x96, 0x33, 0x1f, 0x40, 0xec, 0x1d, 0x3c, 0x94, 0x13, 0xd2, 0x39,
    0x67, 0x29, 0x0b, 0x07, 0x51, 0xe1, 0xf5, 0xee, 0xbf, 0x02, 0xca, 0xa1, 0xa2,
];
const TASK_FLYWAY_FINGERPRINT: [u8; 32] = [
    0x6d, 0xba, 0xe8, 0xc8, 0x8a, 0x4f, 0xa9, 0x0e, 0x7f, 0x49, 0x43, 0x96, 0xcf, 0xe3, 0xee, 0x66, 0x52, 0xbf, 0x05,
    0x02, 0x78, 0xc6, 0xdb, 0x64, 0x8d, 0xf8, 0x83, 0x08, 0xda, 0xeb, 0x42, 0xb9,
];

#[derive(Clone, Copy)]
pub(crate) enum DatabaseKind {
    Main,
    Tasks,
}

impl DatabaseKind {
    fn name(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Tasks => "task",
        }
    }

    fn migrator(self) -> &'static Migrator {
        match self {
            Self::Main => &MAIN_MIGRATOR,
            Self::Tasks => &TASK_MIGRATOR,
        }
    }

    fn flyway_fingerprint(self) -> &'static [u8; 32] {
        match self {
            Self::Main => &MAIN_FLYWAY_FINGERPRINT,
            Self::Tasks => &TASK_FLYWAY_FINGERPRINT,
        }
    }

    fn flyway_description(self) -> &'static str {
        match self {
            Self::Main => "90 migrations ending at 20250730173126",
            Self::Tasks => "1 migration ending at 20231013114850",
        }
    }
}

#[derive(FromRow, Serialize)]
struct FlywayHistoryEntry {
    installed_rank: i64,
    version: Option<String>,
    description: String,
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    migration_type: String,
    script: String,
    checksum: Option<i64>,
    success: bool,
}

#[derive(FromRow)]
struct SqlxHistoryEntry {
    version: i64,
    checksum: Vec<u8>,
    success: bool,
}

pub(crate) async fn verify_database_file(path: &Path, kind: DatabaseKind) -> Result<()> {
    if !path.exists() || path.metadata().is_ok_and(|metadata| metadata.len() == 0) {
        return Ok(());
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .read_only(true)
                .immutable(!has_journal_sidecar(path)),
        )
        .await?;
    let result = verify_schema(&pool, kind).await;
    pool.close().await;
    result
}

fn has_journal_sidecar(path: &Path) -> bool {
    ["-wal", "-shm", "-journal"]
        .iter()
        .any(|suffix| sidecar_path(path, suffix).exists())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(OsStr::new(suffix));
    value.into()
}

async fn verify_schema(pool: &SqlitePool, kind: DatabaseKind) -> Result<()> {
    let objects = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'")
        .fetch_one(pool)
        .await?;
    if objects == 0 {
        return Ok(());
    }

    let has_flyway = table_exists(pool, "flyway_schema_history").await?;
    let has_sqlx = table_exists(pool, "_sqlx_migrations").await?;
    if !has_flyway && !has_sqlx {
        return Err(incompatible(
            kind,
            "schema has no supported Flyway or SQLx migration history",
        ));
    }
    if has_flyway {
        verify_flyway_history(pool, kind).await?;
    }
    if has_sqlx {
        verify_sqlx_history(pool, kind).await?;
    }
    Ok(())
}

async fn table_exists(pool: &SqlitePool, table: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND lower(name)=lower(?)")
            .bind(table)
            .fetch_one(pool)
            .await?
            == 1,
    )
}

async fn verify_flyway_history(pool: &SqlitePool, kind: DatabaseKind) -> Result<()> {
    let entries = sqlx::query_as::<_, FlywayHistoryEntry>(
        r#"SELECT installed_rank, version, description, type, script, checksum, success
           FROM flyway_schema_history ORDER BY installed_rank"#,
    )
    .fetch_all(pool)
    .await
    .map_err(|error| incompatible(kind, format!("could not read Flyway history: {error}")))?;
    let encoded = serde_json::to_vec(&entries)
        .map_err(|error| incompatible(kind, format!("could not validate Flyway history: {error}")))?;
    let fingerprint: [u8; 32] = Sha256::digest(encoded).into();
    if &fingerprint != kind.flyway_fingerprint() {
        let latest = entries
            .last()
            .and_then(|entry| entry.version.as_deref())
            .unwrap_or("none");
        return Err(incompatible(
            kind,
            format!(
                "Flyway history does not match the fixed Kotlin baseline (expected {}; found {} entries ending at {latest})",
                kind.flyway_description(),
                entries.len()
            ),
        ));
    }
    Ok(())
}

async fn verify_sqlx_history(pool: &SqlitePool, kind: DatabaseKind) -> Result<()> {
    let applied = sqlx::query_as::<_, SqlxHistoryEntry>(
        "SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| incompatible(kind, format!("could not read SQLx migration history: {error}")))?;
    let expected = kind.migrator().iter().collect::<Vec<_>>();
    if applied.len() > expected.len()
        || applied.iter().zip(&expected).any(|(applied, expected)| {
            !applied.success || applied.version != expected.version || applied.checksum != expected.checksum.as_ref()
        })
    {
        let latest = applied.last().map_or(0, |migration| migration.version);
        return Err(incompatible(
            kind,
            format!(
                "SQLx migration history is not a supported prefix (found {} entries ending at {latest})",
                applied.len()
            ),
        ));
    }
    Ok(())
}

fn incompatible(kind: DatabaseKind, reason: impl Into<String>) -> StorageError {
    StorageError::IncompatibleDatabase {
        database: kind.name(),
        reason: reason.into(),
    }
}
