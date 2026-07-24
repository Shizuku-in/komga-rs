use std::collections::BTreeMap;

use serde::Serialize;

use crate::{Result, Storage};

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

    /// Reads the configured remember-me lifetime in days.
    ///
    /// Invalid or absent values use Kotlin's 365-day default.
    ///
    /// # Errors
    ///
    /// Returns a database error when the setting cannot be queried.
    pub async fn remember_me_duration_days(&self) -> Result<u64> {
        let value = sqlx::query_scalar::<_, Option<String>>(
            "SELECT VALUE FROM SERVER_SETTINGS WHERE KEY = 'REMEMBER_ME_DURATION'",
        )
        .fetch_optional(&self.pool)
        .await?
        .flatten();
        Ok(value
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(365))
    }
}
