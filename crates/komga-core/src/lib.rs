//! Framework-independent domain types and authorization policy for Komga.
//!
//! This crate contains the behavior shared by the HTTP and persistence layers.
//! It intentionally has no dependency on Axum or SQLx. Types that cross the
//! existing Komga REST boundary preserve the Kotlin service's serialized names.

#![warn(missing_docs)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Every application role recognized by the current Komga compatibility layer.
///
/// `USER` is not stored as a role. It is added only when a [`UserDto`] is
/// produced, matching Spring Security's implicit authenticated-user authority.
pub const ALL_ROLES: [UserRole; 5] = [
    UserRole::Admin,
    UserRole::FileDownload,
    UserRole::PageStreaming,
    UserRole::KoboSync,
    UserRole::KoreaderSync,
];

/// A capability that can be granted to a Komga user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum UserRole {
    /// Manage the server and bypass library visibility restrictions.
    Admin,
    /// Download original book files and generated archives.
    FileDownload,
    /// Stream individual book pages.
    PageStreaming,
    /// Synchronize books with Kobo devices.
    KoboSync,
    /// Synchronize reading progress with KOReader.
    KoreaderSync,
}

impl UserRole {
    /// Returns the uppercase identifier stored by the Kotlin database schema.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "ADMIN",
            Self::FileDownload => "FILE_DOWNLOAD",
            Self::PageStreaming => "PAGE_STREAMING",
            Self::KoboSync => "KOBO_SYNC",
            Self::KoreaderSync => "KOREADER_SYNC",
        }
    }
}

impl TryFrom<&str> for UserRole {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "ADMIN" => Ok(Self::Admin),
            "FILE_DOWNLOAD" => Ok(Self::FileDownload),
            "PAGE_STREAMING" => Ok(Self::PageStreaming),
            "KOBO_SYNC" => Ok(Self::KoboSync),
            "KOREADER_SYNC" => Ok(Self::KoreaderSync),
            _ => Err(()),
        }
    }
}

/// Determines whether an age boundary admits or rejects matching content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AllowExclude {
    /// Admit content at or below the configured age.
    AllowOnly,
    /// Reject content at or above the configured age.
    Exclude,
}

/// An age threshold and the policy applied at that threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeRestriction {
    /// Inclusive age threshold used by [`User::is_content_allowed`].
    pub age: u32,
    /// Whether the threshold acts as an allow-list or exclusion rule.
    pub restriction: AllowExclude,
}

/// Age and sharing-label restrictions attached to a user.
///
/// Instances created with [`ContentRestrictions::new`] are normalized: labels
/// are trimmed and lowercased, blank labels are removed, and an excluded label
/// is removed from the allow set when it occurs in both sets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentRestrictions {
    /// Optional age-based rule.
    pub age_restriction: Option<AgeRestriction>,
    /// Normalized labels that may admit otherwise restricted content.
    pub labels_allow: BTreeSet<String>,
    /// Normalized labels that always reject matching content.
    pub labels_exclude: BTreeSet<String>,
}

impl ContentRestrictions {
    /// Creates a normalized set of content restrictions.
    ///
    /// Exclusion wins when the same label occurs in both input collections.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::BTreeSet;
    /// use komga_core::ContentRestrictions;
    ///
    /// let restrictions = ContentRestrictions::new(
    ///     None,
    ///     [" Kids ".to_owned(), "blocked".to_owned()],
    ///     ["BLOCKED".to_owned()],
    /// );
    ///
    /// assert_eq!(restrictions.labels_allow, BTreeSet::from(["kids".to_owned()]));
    /// assert_eq!(restrictions.labels_exclude, BTreeSet::from(["blocked".to_owned()]));
    /// ```
    pub fn new(
        age_restriction: Option<AgeRestriction>,
        labels_allow: impl IntoIterator<Item = String>,
        labels_exclude: impl IntoIterator<Item = String>,
    ) -> Self {
        let labels_exclude = normalized_labels(labels_exclude);
        let labels_allow = normalized_labels(labels_allow)
            .difference(&labels_exclude)
            .cloned()
            .collect();

        Self {
            age_restriction,
            labels_allow,
            labels_exclude,
        }
    }
}

fn normalized_labels(labels: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    labels
        .into_iter()
        .map(|label| label.trim().to_lowercase())
        .filter(|label| !label.is_empty())
        .collect()
}

/// A persisted Komga user and the policy needed to authorize that user.
///
/// [`User::password_hash`] contains a BCrypt hash and must never be serialized
/// in an API response. Use [`User::to_dto`] for the public representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// Stable identifier used by Komga database relations.
    pub id: String,
    /// Login email. Persistence lookups treat this value case-insensitively.
    pub email: String,
    /// BCrypt password hash, including its version and cost prefix.
    pub password_hash: String,
    /// Explicit capabilities; the implicit `USER` role is not stored here.
    pub roles: BTreeSet<UserRole>,
    /// Whether all libraries are visible without per-library entries.
    pub shared_all_libraries: bool,
    /// Libraries visible when [`User::shared_all_libraries`] is false.
    pub shared_library_ids: BTreeSet<String>,
    /// Age and sharing-label restrictions applied to visible content.
    pub restrictions: ContentRestrictions,
}

impl User {
    /// Creates a user with a generated ID and unrestricted library visibility.
    ///
    /// The role set is empty. Claim and user-creation workflows are responsible
    /// for assigning the roles appropriate to their API contract.
    pub fn new(email: String, password_hash: String) -> Self {
        Self {
            id: new_id(),
            email,
            password_hash,
            roles: BTreeSet::new(),
            shared_all_libraries: true,
            shared_library_ids: BTreeSet::new(),
            restrictions: ContentRestrictions::default(),
        }
    }

    /// Returns whether the user has the administrator capability.
    pub fn is_admin(&self) -> bool {
        self.roles.contains(&UserRole::Admin)
    }

    /// Returns whether the user may access a library.
    ///
    /// Administrators always have access, matching the Kotlin implementation.
    pub fn can_access_library(&self, library_id: &str) -> bool {
        self.is_admin() || self.shared_all_libraries || self.shared_library_ids.contains(library_id)
    }

    /// Evaluates Komga's combined age and sharing-label policy.
    ///
    /// Allow rules are combined with logical OR. After admission, any matching
    /// exclusion rule rejects the content. Input labels are normalized before
    /// comparison.
    pub fn is_content_allowed(&self, age_rating: Option<u32>, sharing_labels: &BTreeSet<String>) -> bool {
        let labels = normalized_labels(sharing_labels.iter().cloned());
        let age_allowed = self.restrictions.age_restriction.as_ref().and_then(|restriction| {
            (restriction.restriction == AllowExclude::AllowOnly)
                .then(|| age_rating.is_some_and(|age| age <= restriction.age))
        });
        let label_allowed =
            (!self.restrictions.labels_allow.is_empty()).then(|| !self.restrictions.labels_allow.is_disjoint(&labels));

        let allowed = match (age_allowed, label_allowed) {
            (None, Some(label)) => label,
            (Some(age), None) => age,
            (Some(age), Some(label)) => age || label,
            (None, None) => true,
        };
        if !allowed {
            return false;
        }

        let age_denied = self.restrictions.age_restriction.as_ref().is_some_and(|restriction| {
            restriction.restriction == AllowExclude::Exclude && age_rating.is_some_and(|age| age >= restriction.age)
        });
        let label_denied = !self.restrictions.labels_exclude.is_disjoint(&labels);
        !age_denied && !label_denied
    }

    /// Produces the REST-safe representation of this user.
    ///
    /// This omits the password hash and adds the implicit `USER` role expected
    /// by existing Komga clients.
    pub fn to_dto(&self) -> UserDto {
        let roles = self
            .roles
            .iter()
            .map(|role| role.as_str().to_owned())
            .chain(std::iter::once("USER".to_owned()))
            .collect();

        UserDto {
            id: self.id.clone(),
            email: self.email.clone(),
            roles,
            shared_all_libraries: self.shared_all_libraries,
            shared_libraries_ids: self.shared_library_ids.clone(),
            labels_allow: self.restrictions.labels_allow.clone(),
            labels_exclude: self.restrictions.labels_exclude.clone(),
            age_restriction: self.restrictions.age_restriction.clone(),
        }
    }
}

/// Public user representation used by the Komga REST API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDto {
    /// Stable user identifier.
    pub id: String,
    /// Login email.
    pub email: String,
    /// Explicit role names plus the implicit `USER` role.
    pub roles: BTreeSet<String>,
    /// Whether the user can access every library.
    pub shared_all_libraries: bool,
    /// Explicitly shared library identifiers.
    pub shared_libraries_ids: BTreeSet<String>,
    /// Normalized labels that may admit content.
    pub labels_allow: BTreeSet<String>,
    /// Normalized labels that reject content.
    pub labels_exclude: BTreeSet<String>,
    /// Optional age policy; omitted from JSON when no policy is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_restriction: Option<AgeRestriction>,
}

/// REST representation of an API key record.
///
/// Creation responses contain the one-time plaintext key. List responses
/// replace [`ApiKey::key`] with `******`; persistence stores only its SHA-512
/// digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKey {
    /// Stable API key record identifier.
    pub id: String,
    /// Identifier of the owning user.
    pub user_id: String,
    /// Plaintext key, redaction marker, or persisted digest depending on layer.
    pub key: String,
    /// User-supplied description, unique per user.
    pub comment: String,
    /// UTC creation time serialized in Komga-compatible form.
    pub created_date: String,
    /// UTC modification time serialized in Komga-compatible form.
    pub last_modified_date: String,
}

/// Generates a lowercase, hyphen-free UUID identifier.
///
/// The value is opaque to clients and fits Komga's string ID columns.
pub fn new_id() -> String {
    Uuid::new_v4().simple().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrictions_are_normalized_and_exclude_wins() {
        let restrictions =
            ContentRestrictions::new(None, [" Kids ".into(), "ADULT".into(), "".into()], ["adult".into()]);

        assert_eq!(restrictions.labels_allow, BTreeSet::from(["kids".into()]));
        assert_eq!(restrictions.labels_exclude, BTreeSet::from(["adult".into()]));
    }

    #[test]
    fn dto_adds_implicit_user_role_and_uses_komga_field_names() {
        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.roles.insert(UserRole::PageStreaming);

        let json = serde_json::to_value(user.to_dto()).unwrap();
        assert_eq!(json["sharedAllLibraries"], true);
        assert_eq!(json["roles"], serde_json::json!(["PAGE_STREAMING", "USER"]));
        assert!(json.get("ageRestriction").is_none());
    }

    #[test]
    fn content_rules_match_komga_allow_and_exclude_semantics() {
        let mut user = User::new("reader@example.org".into(), "hash".into());
        user.restrictions = ContentRestrictions::new(
            Some(AgeRestriction {
                age: 12,
                restriction: AllowExclude::AllowOnly,
            }),
            ["family".into()],
            ["blocked".into()],
        );

        assert!(user.is_content_allowed(Some(10), &BTreeSet::new()));
        assert!(user.is_content_allowed(Some(16), &BTreeSet::from(["family".into()])));
        assert!(!user.is_content_allowed(Some(16), &BTreeSet::new()));
        assert!(!user.is_content_allowed(Some(10), &BTreeSet::from(["blocked".into()])));
    }
}
