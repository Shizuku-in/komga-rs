//! Resolution of process settings from CLI flags and environment variables.

use std::{net::IpAddr, path::PathBuf};

use clap::Parser;

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

    /// Address on which the HTTP server listens.
    #[arg(long, env = "KOMGA_HOST", default_value = "0.0.0.0")]
    pub host: IpAddr,

    /// TCP port on which the HTTP server listens.
    #[arg(long, env = "KOMGA_PORT", default_value_t = 25600)]
    pub port: u16,
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
}
