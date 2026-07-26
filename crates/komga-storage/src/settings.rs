use std::collections::BTreeMap;

use komga_core::ThumbnailSize;
use rand::distr::{Alphanumeric, SampleString};
use serde::Serialize;
use sqlx::{Sqlite, Transaction};

use crate::{Result, Storage, StorageError};

/// One global or user-specific client setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSetting {
    /// Opaque value interpreted by the web client.
    pub value: String,
    /// Global anonymous-access policy; omitted for user settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_unauthorized: Option<bool>,
}

/// Parsed values stored in Kotlin's global `SERVER_SETTINGS` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    /// Whether empty collections are removed during content cleanup.
    pub delete_empty_collections: bool,
    /// Whether empty read lists are removed during content cleanup.
    pub delete_empty_read_lists: bool,
    /// Secret generation used by remember-me authentication.
    pub remember_me_key: String,
    /// Lifetime of newly issued remember-me sessions.
    pub remember_me_duration_days: u64,
    /// Generated-thumbnail size preset.
    pub thumbnail_size: ThumbnailSize,
    /// Desired number of concurrent task workers.
    pub task_pool_size: usize,
    /// Optional database override for the primary HTTP port.
    pub server_port: Option<u16>,
    /// Optional database override for the servlet-style context path.
    pub server_context_path: Option<String>,
    /// Optional database override for Kobo Store proxying.
    pub kobo_proxy: Option<bool>,
    /// Optional externally visible port used in direct Kobo URLs.
    pub kobo_port: Option<u16>,
    /// Optional database override for the external `kepubify` executable.
    pub kepubify_path: Option<String>,
}

/// Tri-state update for nullable server settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SettingUpdate<T> {
    /// Leave the existing row unchanged.
    #[default]
    Keep,
    /// Store the supplied value.
    Set(T),
    /// Delete the setting row so configuration fallback applies.
    Clear,
}

/// Validated changes applied atomically to `SERVER_SETTINGS`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSettingsPatch {
    /// New empty-collection cleanup policy, when supplied.
    pub delete_empty_collections: Option<bool>,
    /// New empty-read-list cleanup policy, when supplied.
    pub delete_empty_read_lists: Option<bool>,
    /// New remember-me duration, when supplied.
    pub remember_me_duration_days: Option<u64>,
    /// Whether to replace the remember-me key.
    pub renew_remember_me_key: bool,
    /// New thumbnail size, when supplied.
    pub thumbnail_size: Option<ThumbnailSize>,
    /// New task worker count, when supplied.
    pub task_pool_size: Option<usize>,
    /// Update for the nullable primary server port override.
    pub server_port: SettingUpdate<u16>,
    /// Update for the nullable server context path override.
    pub server_context_path: SettingUpdate<String>,
    /// New Kobo proxy policy, when supplied.
    pub kobo_proxy: Option<bool>,
    /// Update for the nullable Kobo public port.
    pub kobo_port: SettingUpdate<u16>,
    /// Update for the nullable `kepubify` path.
    pub kepubify_path: SettingUpdate<String>,
}

impl Storage {
    /// Lists global client settings, optionally filtering for anonymous access.
    ///
    /// # Errors
    ///
    /// Returns a database error when settings cannot be queried.
    pub async fn list_global_client_settings(
        &self,
        only_unauthorized: bool,
    ) -> Result<BTreeMap<String, ClientSetting>> {
        let rows = if only_unauthorized {
            sqlx::query_as::<_, (String, String, bool)>(
                "SELECT KEY, VALUE, ALLOW_UNAUTHORIZED FROM CLIENT_SETTINGS_GLOBAL WHERE ALLOW_UNAUTHORIZED = 1",
            )
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query_as::<_, (String, String, bool)>(
                "SELECT KEY, VALUE, ALLOW_UNAUTHORIZED FROM CLIENT_SETTINGS_GLOBAL",
            )
            .fetch_all(&self.pool)
            .await?
        };
        Ok(rows
            .into_iter()
            .map(|(key, value, allow_unauthorized)| {
                (
                    key,
                    ClientSetting {
                        value,
                        allow_unauthorized: Some(allow_unauthorized),
                    },
                )
            })
            .collect())
    }

    /// Lists client settings owned by one user.
    ///
    /// # Errors
    ///
    /// Returns a database error when settings cannot be queried.
    pub async fn list_user_client_settings(&self, user_id: &str) -> Result<BTreeMap<String, ClientSetting>> {
        let rows =
            sqlx::query_as::<_, (String, String)>("SELECT KEY, VALUE FROM CLIENT_SETTINGS_USER WHERE USER_ID = ?")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    ClientSetting {
                        value,
                        allow_unauthorized: None,
                    },
                )
            })
            .collect())
    }

    /// Upserts global client settings atomically.
    ///
    /// Kotlin preserves `ALLOW_UNAUTHORIZED` when a key already exists, so the
    /// conflict branch intentionally updates only `VALUE`.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction fails.
    pub async fn save_global_client_settings(&self, settings: &[(String, String, bool)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (key, value, allow_unauthorized) in settings {
            sqlx::query(
                r#"INSERT INTO CLIENT_SETTINGS_GLOBAL (KEY, VALUE, ALLOW_UNAUTHORIZED)
                   VALUES (?, ?, ?)
                   ON CONFLICT(KEY) DO UPDATE SET VALUE = excluded.VALUE"#,
            )
            .bind(key)
            .bind(value)
            .bind(allow_unauthorized)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Upserts user client settings atomically.
    ///
    /// # Errors
    ///
    /// Returns a database error when the transaction fails.
    pub async fn save_user_client_settings(&self, user_id: &str, settings: &[(String, String)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (key, value) in settings {
            sqlx::query(
                r#"INSERT INTO CLIENT_SETTINGS_USER (USER_ID, KEY, VALUE) VALUES (?, ?, ?)
                   ON CONFLICT(KEY, USER_ID) DO UPDATE SET VALUE = excluded.VALUE"#,
            )
            .bind(user_id)
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Deletes selected global client settings.
    ///
    /// # Errors
    ///
    /// Returns a database error when deletion fails.
    pub async fn delete_global_client_settings(&self, keys: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for key in keys {
            sqlx::query("DELETE FROM CLIENT_SETTINGS_GLOBAL WHERE KEY = ?")
                .bind(key)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Deletes selected client settings for one user.
    ///
    /// # Errors
    ///
    /// Returns a database error when deletion fails.
    pub async fn delete_user_client_settings(&self, user_id: &str, keys: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for key in keys {
            sqlx::query("DELETE FROM CLIENT_SETTINGS_USER WHERE USER_ID = ? AND KEY = ?")
                .bind(user_id)
                .bind(key)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Reads and parses all Kotlin-compatible global server settings.
    ///
    /// Missing or malformed scalar values use Kotlin defaults. A missing or
    /// blank remember-me key is repaired before the snapshot is returned.
    ///
    /// # Errors
    ///
    /// Returns a database error when settings cannot be queried or a missing
    /// remember-me key cannot be persisted.
    pub async fn server_settings(&self) -> Result<ServerSettings> {
        let mut values = sqlx::query_as::<_, (String, Option<String>)>("SELECT KEY, VALUE FROM SERVER_SETTINGS")
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .collect::<BTreeMap<_, _>>();

        let remember_me_key = values
            .get("REMEMBER_ME_KEY")
            .and_then(|value| value.as_deref())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(random_remember_me_key);
        if values
            .get("REMEMBER_ME_KEY")
            .and_then(|value| value.as_deref())
            .is_none_or(str::is_empty)
        {
            sqlx::query(
                "INSERT INTO SERVER_SETTINGS (KEY, VALUE) VALUES ('REMEMBER_ME_KEY', ?) ON CONFLICT(KEY) DO UPDATE SET VALUE=excluded.VALUE",
            )
            .bind(&remember_me_key)
            .execute(&self.pool)
            .await?;
            values.insert("REMEMBER_ME_KEY".to_owned(), Some(remember_me_key.clone()));
        }

        Ok(parse_server_settings(&values, remember_me_key))
    }

    /// Applies validated server-setting changes in one transaction.
    ///
    /// # Errors
    ///
    /// Returns a database error and rolls back every setting when any write fails.
    pub async fn update_server_settings(&self, patch: &ServerSettingsPatch) -> Result<ServerSettings> {
        validate_server_settings_patch(patch)?;
        let mut tx = self.pool.begin().await?;
        if let Some(value) = patch.delete_empty_collections {
            upsert_setting(&mut tx, "DELETE_EMPTY_COLLECTIONS", value.to_string()).await?;
        }
        if let Some(value) = patch.delete_empty_read_lists {
            upsert_setting(&mut tx, "DELETE_EMPTY_READLISTS", value.to_string()).await?;
        }
        if let Some(value) = patch.remember_me_duration_days {
            upsert_setting(&mut tx, "REMEMBER_ME_DURATION", value.to_string()).await?;
        }
        if patch.renew_remember_me_key {
            upsert_setting(&mut tx, "REMEMBER_ME_KEY", random_remember_me_key()).await?;
        }
        if let Some(value) = patch.thumbnail_size {
            upsert_setting(&mut tx, "THUMBNAIL_SIZE", value.as_str().to_owned()).await?;
        }
        if let Some(value) = patch.task_pool_size {
            upsert_setting(&mut tx, "TASK_POOL_SIZE", value.to_string()).await?;
        }
        apply_setting_update(&mut tx, "SERVER_PORT", &patch.server_port, ToString::to_string).await?;
        apply_setting_update(&mut tx, "SERVER_CONTEXT_PATH", &patch.server_context_path, Clone::clone).await?;
        if let Some(value) = patch.kobo_proxy {
            upsert_setting(&mut tx, "KOBO_PROXY", value.to_string()).await?;
        }
        apply_setting_update(&mut tx, "KOBO_PORT", &patch.kobo_port, ToString::to_string).await?;
        apply_setting_update(&mut tx, "KEPUBIFY_PATH", &patch.kepubify_path, Clone::clone).await?;
        let mut values = sqlx::query_as::<_, (String, Option<String>)>("SELECT KEY, VALUE FROM SERVER_SETTINGS")
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let remember_me_key = values
            .get("REMEMBER_ME_KEY")
            .and_then(|value| value.as_deref())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(random_remember_me_key);
        if values
            .get("REMEMBER_ME_KEY")
            .and_then(|value| value.as_deref())
            .is_none_or(str::is_empty)
        {
            upsert_setting(&mut tx, "REMEMBER_ME_KEY", remember_me_key.clone()).await?;
            values.insert("REMEMBER_ME_KEY".to_owned(), Some(remember_me_key.clone()));
        }
        let settings = parse_server_settings(&values, remember_me_key);
        tx.commit().await?;
        Ok(settings)
    }

    /// Reads the configured remember-me lifetime in days.
    ///
    /// Invalid or absent values use Kotlin's 365-day default.
    ///
    /// # Errors
    ///
    /// Returns a database error when the setting cannot be queried.
    pub async fn remember_me_duration_days(&self) -> Result<u64> {
        Ok(self.server_settings().await?.remember_me_duration_days)
    }

    /// Reads the configured task worker pool size.
    ///
    /// Missing, zero, negative, or malformed values use Kotlin's default of one.
    ///
    /// # Errors
    ///
    /// Returns a database error when the setting cannot be queried.
    pub async fn task_pool_size(&self) -> Result<usize> {
        Ok(self.server_settings().await?.task_pool_size)
    }

    /// Reads the longest edge used for generated book thumbnails.
    ///
    /// Missing or invalid values use Kotlin's `DEFAULT` size of 300 pixels.
    ///
    /// # Errors
    ///
    /// Returns a database error when the setting cannot be queried.
    pub async fn thumbnail_max_edge(&self) -> Result<u32> {
        Ok(self.server_settings().await?.thumbnail_size.max_edge())
    }
}

fn validate_server_settings_patch(patch: &ServerSettingsPatch) -> Result<()> {
    if patch.remember_me_duration_days == Some(0) {
        return Err(StorageError::InvalidQuery(
            "remember-me duration must be positive".to_owned(),
        ));
    }
    if patch.task_pool_size == Some(0) {
        return Err(StorageError::InvalidQuery("task pool size must be positive".to_owned()));
    }
    if matches!(patch.server_port, SettingUpdate::Set(0)) {
        return Err(StorageError::InvalidQuery("server port must be positive".to_owned()));
    }
    if matches!(patch.kobo_port, SettingUpdate::Set(0)) {
        return Err(StorageError::InvalidQuery("Kobo port must be positive".to_owned()));
    }
    if let SettingUpdate::Set(value) = &patch.server_context_path
        && !komga_core::valid_server_context_path(value)
    {
        return Err(StorageError::InvalidQuery("invalid server context path".to_owned()));
    }
    Ok(())
}

fn parse_server_settings(values: &BTreeMap<String, Option<String>>, remember_me_key: String) -> ServerSettings {
    let value = |key: &str| values.get(key).and_then(|value| value.as_deref());
    ServerSettings {
        delete_empty_collections: parse_setting(
            "DELETE_EMPTY_COLLECTIONS",
            value("DELETE_EMPTY_COLLECTIONS"),
            parse_bool,
        )
        .unwrap_or(false),
        delete_empty_read_lists: parse_setting("DELETE_EMPTY_READLISTS", value("DELETE_EMPTY_READLISTS"), parse_bool)
            .unwrap_or(false),
        remember_me_key,
        remember_me_duration_days: parse_setting("REMEMBER_ME_DURATION", value("REMEMBER_ME_DURATION"), |value| {
            value.parse().ok().filter(|value| *value > 0)
        })
        .unwrap_or(365),
        thumbnail_size: parse_setting("THUMBNAIL_SIZE", value("THUMBNAIL_SIZE"), |value| {
            ThumbnailSize::try_from(value).ok()
        })
        .unwrap_or_default(),
        task_pool_size: parse_setting("TASK_POOL_SIZE", value("TASK_POOL_SIZE"), |value| {
            value.parse().ok().filter(|value| *value > 0)
        })
        .unwrap_or(1),
        server_port: parse_setting("SERVER_PORT", value("SERVER_PORT"), parse_port),
        server_context_path: parse_setting("SERVER_CONTEXT_PATH", value("SERVER_CONTEXT_PATH"), |value| {
            komga_core::valid_server_context_path(value).then(|| value.to_owned())
        }),
        kobo_proxy: parse_setting("KOBO_PROXY", value("KOBO_PROXY"), parse_bool),
        kobo_port: parse_setting("KOBO_PORT", value("KOBO_PORT"), parse_port),
        kepubify_path: value("KEPUBIFY_PATH")
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
    }
}

fn parse_setting<T>(key: &str, value: Option<&str>, parse: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    let value = value?;
    match parse(value) {
        Some(parsed) => Some(parsed),
        None => {
            tracing::warn!(setting = key, "ignoring invalid persisted server setting");
            None
        }
    }
}

fn parse_port(value: &str) -> Option<u16> {
    value.parse().ok().filter(|value| *value > 0)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

fn random_remember_me_key() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 32)
}

async fn upsert_setting(tx: &mut Transaction<'_, Sqlite>, key: &str, value: String) -> Result<()> {
    sqlx::query(
        "INSERT INTO SERVER_SETTINGS (KEY, VALUE) VALUES (?, ?) ON CONFLICT(KEY) DO UPDATE SET VALUE=excluded.VALUE",
    )
    .bind(key)
    .bind(value)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn apply_setting_update<T>(
    tx: &mut Transaction<'_, Sqlite>,
    key: &str,
    update: &SettingUpdate<T>,
    encode: impl FnOnce(&T) -> String,
) -> Result<()> {
    match update {
        SettingUpdate::Keep => {}
        SettingUpdate::Set(value) => upsert_setting(tx, key, encode(value)).await?,
        SettingUpdate::Clear => {
            sqlx::query("DELETE FROM SERVER_SETTINGS WHERE KEY=?")
                .bind(key)
                .execute(&mut **tx)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn server_settings_round_trip_nullable_values_and_rotate_the_key() {
        let storage = Storage::connect_memory().await.unwrap();
        let original = storage.server_settings().await.unwrap();
        let patch = ServerSettingsPatch {
            delete_empty_collections: Some(false),
            delete_empty_read_lists: Some(true),
            remember_me_duration_days: Some(15),
            renew_remember_me_key: true,
            thumbnail_size: Some(ThumbnailSize::Large),
            task_pool_size: Some(4),
            server_port: SettingUpdate::Set(12_345),
            server_context_path: SettingUpdate::Set("/komga/sub-path".to_owned()),
            kobo_proxy: Some(true),
            kobo_port: SettingUpdate::Set(23_456),
            kepubify_path: SettingUpdate::Set("/usr/local/bin/kepubify".to_owned()),
        };
        storage.update_server_settings(&patch).await.unwrap();

        let updated = storage.server_settings().await.unwrap();
        assert!(!updated.delete_empty_collections);
        assert!(updated.delete_empty_read_lists);
        assert_eq!(updated.remember_me_duration_days, 15);
        assert_eq!(updated.thumbnail_size, ThumbnailSize::Large);
        assert_eq!(updated.task_pool_size, 4);
        assert_eq!(updated.server_port, Some(12_345));
        assert_eq!(updated.server_context_path.as_deref(), Some("/komga/sub-path"));
        assert_eq!(updated.kobo_proxy, Some(true));
        assert_eq!(updated.kobo_port, Some(23_456));
        assert_eq!(updated.kepubify_path.as_deref(), Some("/usr/local/bin/kepubify"));
        assert_ne!(updated.remember_me_key, original.remember_me_key);
        assert_eq!(updated.remember_me_key.len(), 32);
        assert!(updated.remember_me_key.bytes().all(|byte| byte.is_ascii_alphanumeric()));

        storage
            .update_server_settings(&ServerSettingsPatch {
                server_port: SettingUpdate::Clear,
                server_context_path: SettingUpdate::Clear,
                kobo_port: SettingUpdate::Clear,
                kepubify_path: SettingUpdate::Clear,
                ..ServerSettingsPatch::default()
            })
            .await
            .unwrap();
        let cleared = storage.server_settings().await.unwrap();
        assert_eq!(cleared.server_port, None);
        assert_eq!(cleared.server_context_path, None);
        assert_eq!(cleared.kobo_port, None);
        assert_eq!(cleared.kepubify_path, None);
    }

    #[tokio::test]
    async fn malformed_server_settings_use_safe_defaults() {
        let storage = Storage::connect_memory().await.unwrap();
        for (key, value) in [
            ("DELETE_EMPTY_COLLECTIONS", "maybe"),
            ("REMEMBER_ME_DURATION", "0"),
            ("THUMBNAIL_SIZE", "HUGE"),
            ("TASK_POOL_SIZE", "-1"),
            ("SERVER_PORT", "0"),
            ("SERVER_CONTEXT_PATH", "/bad/"),
            ("KOBO_PROXY", "maybe"),
            ("KOBO_PORT", "70000"),
            ("KEPUBIFY_PATH", "  "),
        ] {
            sqlx::query(
                "INSERT INTO SERVER_SETTINGS (KEY, VALUE) VALUES (?, ?) ON CONFLICT(KEY) DO UPDATE SET VALUE=excluded.VALUE",
            )
            .bind(key)
            .bind(value)
            .execute(&storage.pool)
            .await
            .unwrap();
        }
        let settings = storage.server_settings().await.unwrap();
        assert!(!settings.delete_empty_collections);
        assert_eq!(settings.remember_me_duration_days, 365);
        assert_eq!(settings.thumbnail_size, ThumbnailSize::Default);
        assert_eq!(settings.task_pool_size, 1);
        assert_eq!(settings.server_port, None);
        assert_eq!(settings.server_context_path, None);
        assert_eq!(settings.kobo_proxy, None);
        assert_eq!(settings.kobo_port, None);
        assert_eq!(settings.kepubify_path, None);

        assert!(matches!(
            storage
                .update_server_settings(&ServerSettingsPatch {
                    task_pool_size: Some(0),
                    ..ServerSettingsPatch::default()
                })
                .await,
            Err(StorageError::InvalidQuery(_))
        ));
    }

    #[tokio::test]
    async fn server_settings_patch_rolls_back_on_sql_failure() {
        let storage = Storage::connect_memory().await.unwrap();
        let original = storage.server_settings().await.unwrap();
        sqlx::query(
            r#"CREATE TRIGGER reject_kobo_proxy BEFORE INSERT ON SERVER_SETTINGS
               WHEN NEW.KEY='KOBO_PROXY' BEGIN SELECT RAISE(FAIL, 'forced failure'); END"#,
        )
        .execute(&storage.pool)
        .await
        .unwrap();
        let result = storage
            .update_server_settings(&ServerSettingsPatch {
                delete_empty_collections: Some(!original.delete_empty_collections),
                kobo_proxy: Some(true),
                ..ServerSettingsPatch::default()
            })
            .await;
        assert!(result.is_err());
        assert_eq!(storage.server_settings().await.unwrap(), original);
    }
}
