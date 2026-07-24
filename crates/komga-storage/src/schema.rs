use std::collections::BTreeSet;

use sqlx::{Row, SqlitePool};

use crate::Result;

const LIBRARY_COLUMNS: [(&str, &str); 25] = [
    ("IMPORT_COMICINFO_BOOK", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_COMICINFO_SERIES", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_COMICINFO_COLLECTION", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_COMICINFO_READLIST", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_COMICINFO_SERIES_APPEND_VOLUME", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_EPUB_BOOK", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_EPUB_SERIES", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_MYLAR_SERIES", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_LOCAL_ARTWORK", "boolean NOT NULL DEFAULT 1"),
    ("IMPORT_BARCODE_ISBN", "boolean NOT NULL DEFAULT 1"),
    ("SCAN_FORCE_MODIFIED_TIME", "boolean NOT NULL DEFAULT 0"),
    ("SCAN_STARTUP", "boolean NOT NULL DEFAULT 0"),
    ("SCAN_CBX", "boolean NOT NULL DEFAULT 1"),
    ("SCAN_PDF", "boolean NOT NULL DEFAULT 1"),
    ("SCAN_EPUB", "boolean NOT NULL DEFAULT 1"),
    ("SCAN_INTERVAL", "varchar NOT NULL DEFAULT 'EVERY_6H'"),
    ("REPAIR_EXTENSIONS", "boolean NOT NULL DEFAULT 0"),
    ("CONVERT_TO_CBZ", "boolean NOT NULL DEFAULT 0"),
    ("EMPTY_TRASH_AFTER_SCAN", "boolean NOT NULL DEFAULT 0"),
    ("SERIES_COVER", "varchar NOT NULL DEFAULT 'FIRST'"),
    ("HASH_FILES", "boolean NOT NULL DEFAULT 1"),
    ("HASH_PAGES", "boolean NOT NULL DEFAULT 0"),
    ("HASH_KOREADER", "boolean NOT NULL DEFAULT 0"),
    ("ANALYZE_DIMENSIONS", "boolean NOT NULL DEFAULT 1"),
    ("ONESHOTS_DIRECTORY", "varchar NULL DEFAULT NULL"),
];

pub(crate) async fn reconcile_library_schema(pool: &SqlitePool) -> Result<()> {
    let existing = sqlx::query("PRAGMA table_info(LIBRARY)")
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(|row| row.get::<String, _>("name").to_ascii_uppercase())
        .collect::<BTreeSet<_>>();

    for (name, declaration) in LIBRARY_COLUMNS {
        if !existing.contains(name) {
            sqlx::query(&format!("ALTER TABLE LIBRARY ADD COLUMN {name} {declaration}"))
                .execute(pool)
                .await?;
        }
    }
    if !existing.contains("UNAVAILABLE_DATE") {
        sqlx::query("ALTER TABLE LIBRARY ADD COLUMN UNAVAILABLE_DATE datetime NULL DEFAULT NULL")
            .execute(pool)
            .await?;
    }

    let exclusions_existed = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND upper(name) = 'LIBRARY_EXCLUSIONS')",
    )
    .fetch_one(pool)
    .await?;
    sqlx::raw_sql(
        r#"
        CREATE TABLE IF NOT EXISTS LIBRARY_EXCLUSIONS
        (
            LIBRARY_ID varchar NOT NULL,
            EXCLUSION  varchar NOT NULL,
            PRIMARY KEY (LIBRARY_ID, EXCLUSION),
            FOREIGN KEY (LIBRARY_ID) REFERENCES LIBRARY (ID)
        );
        CREATE INDEX IF NOT EXISTS idx__library_exclusions__library_id
            ON LIBRARY_EXCLUSIONS (LIBRARY_ID);
        "#,
    )
    .execute(pool)
    .await?;

    if !exclusions_existed {
        sqlx::raw_sql(
            r#"
            INSERT OR IGNORE INTO LIBRARY_EXCLUSIONS (LIBRARY_ID, EXCLUSION)
                SELECT ID, '#recycle' FROM LIBRARY;
            INSERT OR IGNORE INTO LIBRARY_EXCLUSIONS (LIBRARY_ID, EXCLUSION)
                SELECT ID, '@eaDir' FROM LIBRARY;
            INSERT OR IGNORE INTO LIBRARY_EXCLUSIONS (LIBRARY_ID, EXCLUSION)
                SELECT ID, '@Recycle' FROM LIBRARY;
            "#,
        )
        .execute(pool)
        .await?;
    }
    reconcile_container_thumbnail_schema(pool).await?;
    Ok(())
}

async fn reconcile_container_thumbnail_schema(pool: &SqlitePool) -> Result<()> {
    for table in ["THUMBNAIL_COLLECTION", "THUMBNAIL_READLIST"] {
        let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("name").to_ascii_uppercase())
            .collect::<BTreeSet<_>>();
        if !columns.contains("URL") {
            sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN URL varchar NULL"))
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}
