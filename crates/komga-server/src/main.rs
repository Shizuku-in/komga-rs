//! `komga-server` process entry point.

use std::net::SocketAddr;

use anyhow::Context;
use clap::Parser;
use komga_search::SearchIndex;
use komga_server::{AppState, ServerBindSettings, build_app, config::Config};
use komga_storage::{Storage, TaskStorage};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("komga_server=info,tower_http=info")),
        )
        .init();

    log_native_image_capabilities();

    let config = Config::parse();
    let database_file = config.database_file();
    let tasks_database_file = config.tasks_database_file();
    let search_directory = config.search_directory();
    let fonts_directory = config.fonts_directory();
    let oauth2_config_file = config.oauth2_config_file();
    let oauth2_config_file_is_explicit = config.oauth2_config_file_is_explicit();
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
    let database_settings = storage.server_settings().await?;
    let effective_context_path = resolve_context_path(&config.context_path, &database_settings.server_context_path);
    let address = SocketAddr::new(
        config.host,
        resolve_server_port(config.port, database_settings.server_port),
    );
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;
    let effective_address = listener.local_addr().context("failed to read bound server address")?;

    let search = SearchIndex::open(&search_directory)
        .with_context(|| format!("failed to open search index {}", search_directory.display()))?;
    let state = AppState::with_file_search(storage, tasks, search)
        .await?
        .with_page_hashing(config.page_hashing)
        .with_kobo_sync_item_limit(config.kobo_sync_item_limit)
        .with_fonts_directory(fonts_directory)
        .with_kobo_proxy(config.kobo_proxy)
        .with_kobo_kepubify_path(config.kobo_kepubify_path)
        .await
        .with_server_bind_settings(ServerBindSettings {
            configuration_port: config.port,
            configuration_context_path: config.context_path,
            effective_port: Some(effective_address.port()),
            effective_context_path,
        })
        .with_cors_allowed_origins(config.cors_allowed_origins)
        .with_trusted_proxies(config.trusted_proxies)
        .with_oauth2_config(&oauth2_config_file, oauth2_config_file_is_explicit)
        .await
        .with_context(|| format!("failed to load OAuth configuration {}", oauth2_config_file.display()))?;
    let runtime = state.start_runtime().await?;
    let signal_shutdown = runtime.shutdown_handle();
    let server_shutdown = runtime.shutdown_handle();
    tokio::spawn(async move {
        shutdown_signal().await;
        signal_shutdown.cancel();
    });

    tracing::info!(address = %effective_address, database = %database_file.display(), tasks_database = %tasks_database_file.display(), "Komga Rust backend listening");
    let result = axum::serve(
        listener,
        build_app(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(server_shutdown.cancelled_owned())
    .await;
    runtime.shutdown().await;
    result.context("HTTP server failed")
}

#[cfg(feature = "heif-native")]
fn log_native_image_capabilities() {
    let hevc = komga_media::can_convert_image_media_type("image/heic");
    let av1 = komga_media::can_convert_image_media_type("image/avif");
    if hevc && av1 {
        tracing::info!(hevc, av1, "native HEIF image conversion is available");
    } else {
        tracing::warn!(
            hevc,
            av1,
            "native HEIF feature is enabled but one or more decoders are unavailable"
        );
    }
}

#[cfg(not(feature = "heif-native"))]
fn log_native_image_capabilities() {}

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

fn resolve_server_port(configuration: u16, database: Option<u16>) -> u16 {
    database.unwrap_or(configuration)
}

fn resolve_context_path(configuration: &Option<String>, database: &Option<String>) -> String {
    database.clone().or_else(|| configuration.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_bindings_override_configuration_and_clear_back_to_it() {
        assert_eq!(resolve_server_port(25_600, Some(12_345)), 12_345);
        assert_eq!(resolve_server_port(25_600, None), 25_600);
        assert_eq!(
            resolve_context_path(&Some("/config".into()), &Some("/database".into())),
            "/database"
        );
        assert_eq!(resolve_context_path(&Some("/config".into()), &None), "/config");
        assert_eq!(resolve_context_path(&None, &None), "");
    }
}
