//! Request payloads at the Komga REST compatibility boundary.
//!
//! These types intentionally mirror existing camel-case JSON names. Update
//! payloads use [`Patch`] to distinguish an omitted property from an explicit
//! JSON `null`, because those operations have different meanings in Komga.

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserCreationDto {
    pub(crate) email: String,
    pub(crate) password: String,
    #[serde(default)]
    pub(crate) roles: BTreeSet<String>,
    pub(crate) age_restriction: Option<AgeRestrictionUpdateDto>,
    pub(crate) labels_allow: Option<BTreeSet<String>>,
    pub(crate) labels_exclude: Option<BTreeSet<String>>,
    pub(crate) shared_libraries: Option<SharedLibrariesUpdateDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasswordUpdateDto {
    pub(crate) password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApiKeyRequestDto {
    pub(crate) comment: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AgeRestrictionUpdateDto {
    pub(crate) age: i64,
    pub(crate) restriction: RestrictionUpdate,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum RestrictionUpdate {
    AllowOnly,
    Exclude,
    None,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SharedLibrariesUpdateDto {
    #[serde(deserialize_with = "deserialize_boolish")]
    pub(crate) all: bool,
    #[serde(default)]
    pub(crate) library_ids: BTreeSet<String>,
}

fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Boolish {
        Bool(bool),
        String(String),
    }

    match Boolish::deserialize(deserializer)? {
        Boolish::Bool(value) => Ok(value),
        Boolish::String(value) if value.eq_ignore_ascii_case("true") => Ok(true),
        Boolish::String(value) if value.eq_ignore_ascii_case("false") => Ok(false),
        Boolish::String(value) => Err(serde::de::Error::custom(format!("invalid boolean: {value}"))),
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserUpdateDto {
    #[serde(default)]
    pub(crate) age_restriction: Patch<AgeRestrictionUpdateDto>,
    #[serde(default)]
    pub(crate) labels_allow: Patch<BTreeSet<String>>,
    #[serde(default)]
    pub(crate) labels_exclude: Patch<BTreeSet<String>>,
    #[serde(default)]
    pub(crate) roles: Patch<BTreeSet<String>>,
    #[serde(default)]
    pub(crate) shared_libraries: Patch<SharedLibrariesUpdateDto>,
}

#[derive(Debug, Default)]
pub(crate) enum Patch<T> {
    /// The property was absent and existing state must remain unchanged.
    #[default]
    Missing,
    /// The property was present as JSON `null` and should be cleared or rejected.
    Null,
    /// The property was present with a new value.
    Value(T),
}

impl<'de, T> Deserialize<'de> for Patch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}
