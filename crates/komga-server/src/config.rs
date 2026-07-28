//! Resolution of process settings from CLI flags and environment variables.

use std::{net::IpAddr, num::NonZeroUsize, path::PathBuf};

use clap::Parser;
use http::HeaderValue;
use url::Url;

/// Command-line and environment configuration for the HTTP process.
///
/// Explicit database paths take precedence over the derived config-directory
/// path. CLI arguments take precedence over their corresponding environment
/// variables through Clap's normal resolution rules.
#[derive(Debug, Clone, Parser)]
#[command(name = "komga-server", version, about = "Rust backend for Komga")]
pub struct Config {
    /// Directory for the database, logs, and other persistent state.
    #[arg(long, env = "KOMGA_CONFIG_DIR")]
    pub config_dir: Option<PathBuf>,

    /// Override the SQLite database file.
    #[arg(long, env = "KOMGA_DATABASE_FILE")]
    pub database_file: Option<PathBuf>,

    /// Override the separate persistent task queue file.
    #[arg(long, env = "KOMGA_TASKS_DATABASE_FILE")]
    pub tasks_database_file: Option<PathBuf>,

    /// Override the Tantivy search index directory.
    #[arg(long, env = "KOMGA_SEARCH_DIRECTORY")]
    pub search_directory: Option<PathBuf>,

    /// Override the directory containing additional font-family directories.
    #[arg(long, env = "KOMGA_FONTS_DIRECTORY")]
    pub fonts_directory: Option<PathBuf>,

    /// Override the OAuth2/OIDC JSON configuration file.
    #[arg(long, env = "KOMGA_OAUTH2_CONFIG_FILE")]
    pub oauth2_config_file: Option<PathBuf>,

    /// Number of pages hashed from both the start and end of ZIP books.
    #[arg(long, env = "KOMGA_PAGE_HASHING", default_value = "3")]
    pub page_hashing: NonZeroUsize,

    /// Maximum source books or tags returned by one Kobo sync page.
    #[arg(long, env = "KOMGA_KOBO_SYNC_ITEM_LIMIT", default_value = "100")]
    pub kobo_sync_item_limit: NonZeroUsize,

    /// Path or command name for the optional external kepubify fallback.
    #[arg(long, env = "KOMGA_KOBO_KEPUBIFY_PATH")]
    pub kobo_kepubify_path: Option<PathBuf>,

    /// Forward unsupported Kobo device API requests to the official Kobo Store.
    #[arg(long, env = "KOMGA_KOBO_PROXY", default_value_t = false)]
    pub kobo_proxy: bool,

    /// Address on which the HTTP server listens.
    #[arg(long, env = "KOMGA_HOST", default_value = "0.0.0.0")]
    pub host: IpAddr,

    /// TCP port on which the HTTP server listens.
    #[arg(long, env = "KOMGA_PORT", default_value_t = 25600)]
    pub port: u16,

    /// Optional servlet-style prefix under which every HTTP route is mounted.
    #[arg(long, env = "KOMGA_CONTEXT_PATH", value_parser = parse_context_path)]
    pub context_path: Option<String>,

    /// Exact browser origins allowed to make credentialed cross-origin requests.
    #[arg(
        long,
        env = "KOMGA_CORS_ALLOWED_ORIGINS",
        value_delimiter = ',',
        value_parser = parse_cors_origin
    )]
    pub cors_allowed_origins: Vec<HeaderValue>,
}

fn parse_context_path(value: &str) -> Result<String, String> {
    komga_core::valid_server_context_path(value)
        .then(|| value.to_owned())
        .ok_or_else(|| "context path must start with '/' and end with an ASCII letter or digit".to_owned())
}

fn parse_cors_origin(value: &str) -> Result<HeaderValue, String> {
    let url = Url::parse(value.trim()).map_err(|_| "CORS origin must be an absolute HTTP(S) URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("CORS origin must contain only an HTTP(S) scheme, host, and optional port".to_owned());
    }
    url.origin()
        .ascii_serialization()
        .parse()
        .map_err(|_| "CORS origin is not a valid HTTP header value".to_owned())
}

impl Config {
    /// Returns the explicit config directory or Komga's `$HOME/.komga` default.
    ///
    /// When `HOME` is unavailable, the fallback is `.komga` below the current
    /// working directory.
    pub fn config_dir(&self) -> PathBuf {
        self.config_dir.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".komga")
        })
    }

    /// Returns the explicit database file or `database.sqlite` in the config directory.
    pub fn database_file(&self) -> PathBuf {
        self.database_file
            .clone()
            .unwrap_or_else(|| self.config_dir().join("database.sqlite"))
    }

    /// Returns the explicit task database file or `tasks.sqlite` in the config directory.
    pub fn tasks_database_file(&self) -> PathBuf {
        self.tasks_database_file
            .clone()
            .unwrap_or_else(|| self.config_dir().join("tasks.sqlite"))
    }

    /// Returns the explicit search directory or `search` in the config directory.
    pub fn search_directory(&self) -> PathBuf {
        self.search_directory
            .clone()
            .unwrap_or_else(|| self.config_dir().join("search"))
    }

    /// Returns the explicit fonts directory or `fonts` in the config directory.
    pub fn fonts_directory(&self) -> PathBuf {
        self.fonts_directory
            .clone()
            .unwrap_or_else(|| self.config_dir().join("fonts"))
    }

    /// Returns the explicit OAuth configuration path or `oauth2.json` in the config directory.
    pub fn oauth2_config_file(&self) -> PathBuf {
        self.oauth2_config_file
            .clone()
            .unwrap_or_else(|| self.config_dir().join("oauth2.json"))
    }

    /// Returns whether a missing OAuth configuration file is a startup error.
    pub fn oauth2_config_file_is_explicit(&self) -> bool {
        self.oauth2_config_file.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_limits_have_compatible_defaults_and_reject_zero() {
        let config = Config::try_parse_from(["komga-server"]).unwrap();
        assert_eq!(config.page_hashing.get(), 3);
        assert_eq!(config.kobo_sync_item_limit.get(), 100);
        assert!(config.kobo_kepubify_path.is_none());
        assert!(!config.kobo_proxy);
        assert!(config.context_path.is_none());
        assert!(config.cors_allowed_origins.is_empty());
        assert!(config.oauth2_config_file.is_none());
        assert_eq!(config.oauth2_config_file(), config.config_dir().join("oauth2.json"));
        assert_eq!(config.fonts_directory(), config.config_dir().join("fonts"));
        let kobo = Config::try_parse_from([
            "komga-server",
            "--kobo-kepubify-path",
            "/usr/local/bin/kepubify",
            "--kobo-proxy",
        ])
        .unwrap();
        assert_eq!(
            kobo.kobo_kepubify_path.as_deref(),
            Some(std::path::Path::new("/usr/local/bin/kepubify"))
        );
        assert!(kobo.kobo_proxy);
        assert!(Config::try_parse_from(["komga-server", "--page-hashing", "0"]).is_err());
        assert!(Config::try_parse_from(["komga-server", "--kobo-sync-item-limit", "0"]).is_err());
        assert_eq!(
            Config::try_parse_from(["komga-server", "--context-path", "/komga/sub-path"])
                .unwrap()
                .context_path
                .as_deref(),
            Some("/komga/sub-path")
        );
        assert!(Config::try_parse_from(["komga-server", "--context-path", "/bad/"]).is_err());
        let cors = Config::try_parse_from([
            "komga-server",
            "--cors-allowed-origins",
            "https://reader.example/,http://localhost:5173",
        ])
        .unwrap();
        assert_eq!(
            cors.cors_allowed_origins
                .iter()
                .map(|origin| origin.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["https://reader.example", "http://localhost:5173"]
        );
        for invalid in [
            "*",
            "file:///tmp",
            "https://reader.example/path",
            "https://user@reader.example",
        ] {
            assert!(
                Config::try_parse_from(["komga-server", "--cors-allowed-origins", invalid]).is_err(),
                "{invalid}"
            );
        }
        let fonts = Config::try_parse_from(["komga-server", "--fonts-directory", "/srv/fonts"]).unwrap();
        assert_eq!(fonts.fonts_directory(), std::path::Path::new("/srv/fonts"));
        let oauth = Config::try_parse_from(["komga-server", "--oauth2-config-file", "/srv/komga/oauth.json"]).unwrap();
        assert_eq!(
            oauth.oauth2_config_file(),
            std::path::Path::new("/srv/komga/oauth.json")
        );
        assert!(oauth.oauth2_config_file_is_explicit());
    }
}
