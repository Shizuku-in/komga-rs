use serde::{Deserialize, Serialize};

/// Longest-edge presets used when Komga generates thumbnails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ThumbnailSize {
    /// Generate thumbnails with a 300-pixel longest edge.
    #[default]
    Default,
    /// Generate thumbnails with a 600-pixel longest edge.
    Medium,
    /// Generate thumbnails with a 900-pixel longest edge.
    Large,
    /// Generate thumbnails with a 1200-pixel longest edge.
    Xlarge,
}

impl ThumbnailSize {
    /// Returns the identifier stored by Kotlin in `SERVER_SETTINGS`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "DEFAULT",
            Self::Medium => "MEDIUM",
            Self::Large => "LARGE",
            Self::Xlarge => "XLARGE",
        }
    }

    /// Returns the longest edge represented by this preset.
    pub const fn max_edge(self) -> u32 {
        match self {
            Self::Default => 300,
            Self::Medium => 600,
            Self::Large => 900,
            Self::Xlarge => 1200,
        }
    }
}

impl TryFrom<&str> for ThumbnailSize {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "DEFAULT" => Ok(Self::Default),
            "MEDIUM" => Ok(Self::Medium),
            "LARGE" => Ok(Self::Large),
            "XLARGE" => Ok(Self::Xlarge),
            _ => Err(()),
        }
    }
}

/// Checks the servlet-style context-path syntax accepted by Kotlin's settings API.
///
/// The root path is represented by an absent setting, so `/` is intentionally
/// rejected. Non-ASCII word characters are rejected even though Rust regexes
/// normally treat them as `\w`.
pub fn valid_server_context_path(value: &str) -> bool {
    let Some(rest) = value.strip_prefix('/') else {
        return false;
    };
    !rest.is_empty()
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'/'))
        && rest.bytes().last().is_some_and(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_path_matches_kotlin_validation_matrix() {
        for valid in ["/komga", "/komga-hyphen/subpath123", "/under_score/ok2"] {
            assert!(valid_server_context_path(valid), "{valid}");
        }
        for invalid in [
            "",
            "/",
            "noSlashBegin",
            "/slashEnd/",
            "/invalid=character",
            "/invalid/end-",
            "/invalid/end_",
            "/日本語",
        ] {
            assert!(!valid_server_context_path(invalid), "{invalid}");
        }
    }
}
