use komga_core::ApiKey;
use sqlx::FromRow;

use crate::{Result, Storage, StorageError};

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

pub(crate) fn sqlite_datetime_to_utc(value: String) -> String {
    if value.ends_with('Z') || value.contains('+') {
        value
    } else {
        format!("{}Z", value.replace(' ', "T"))
    }
}
