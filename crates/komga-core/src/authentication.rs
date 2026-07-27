//! Authentication audit domain contracts.

use chrono::NaiveDateTime;

/// One authentication attempt retained for account security review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationActivity {
    /// Resolved user identifier, when the principal was known.
    pub user_id: Option<String>,
    /// Login email, when the authentication mechanism uses one.
    pub email: Option<String>,
    /// API key record identifier, never the key or its digest.
    pub api_key_id: Option<String>,
    /// User-facing API key comment.
    pub api_key_comment: Option<String>,
    /// Best available client address.
    pub ip: Option<String>,
    /// Truncated HTTP User-Agent value.
    pub user_agent: Option<String>,
    /// Whether authentication succeeded.
    pub success: bool,
    /// Truncated client-safe failure description.
    pub error: Option<String>,
    /// UTC timestamp stored without an offset, matching Kotlin's SQLite schema.
    pub date_time: NaiveDateTime,
    /// Authentication mechanism, such as `Password` or `ApiKey`.
    pub source: Option<String>,
}
