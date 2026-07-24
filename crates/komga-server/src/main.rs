//! `komga-server` process entry point.

use std::net::SocketAddr;

use anyhow::Context;
use clap::Parser;
use komga_server::{AppState, build_app, config::Config};
use komga_storage::{Storage, TaskStorage};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("komga_server=info,tower_http=info")),
        )
        .init();

    let config = Config::parse();
    let database_file = config.database_file();
    let tasks_database_file = config.tasks_database_file();
    if let Some(parent) = database_file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }
    let storage = Storage::connect_path(&database_file)
        .await
        .with_context(|| format!("failed to open database {}", database_file.display()))?;
    if let Some(parent) = tasks_database_file.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create task database directory {}", parent.display()))?;
    }
    let tasks = TaskStorage::connect_path(&tasks_database_file)
        .await
        .with_context(|| format!("failed to open task database {}", tasks_database_file.display()))?;
    let address = SocketAddr::new(config.host, config.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;

    tracing::info!(%address, database = %database_file.display(), tasks_database = %tasks_database_file.display(), "Komga Rust backend listening");
    axum::serve(listener, build_app(AppState::new(storage, tasks).await?))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
