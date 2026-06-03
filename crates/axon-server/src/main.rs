//! Axon binary — wires all crates together and owns the process.
//!
//! Boot sequence: load config, initialize tracing, connect the store (running
//! migrations), build the router, then serve until a shutdown signal arrives.
//! `anyhow` is used here at the binary boundary; library crates use `thiserror`.

use anyhow::Context;
use axon_core::Config;
use axon_store::Store;
use axon_sync::SyncEngine;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present. Silently skipped when the file doesn't exist so
    // container / CI deployments that rely purely on environment variables
    // continue to work unchanged.
    let _ = dotenvy::dotenv();

    // Config first, so we know how to configure logging.
    let config = Config::load_default().context("loading configuration")?;

    init_tracing(&config.log.level);

    let store = Store::connect(&config.database.url, config.database.max_connections)
        .await
        .context("connecting to database")?;

    // Start the sync engine: it provisions the configured account and runs one
    // supervised Simplified Sliding Sync task per account.
    let sync_engine = SyncEngine::start(store.clone(), config.sync.clone())
        .await
        .context("starting sync engine")?;

    // The API shares the sync engine's live-event bus so `/v1/ws` can fan out
    // events as they're persisted.
    let app = axon_api::router(axon_api::AppState::new(store, sync_engine.live_events()));

    let addr = config.socket_addr();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    tracing::info!(%addr, "axon listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    // HTTP has drained; now wind down the sync tasks and wait for them to flush
    // their SDK stores before exiting.
    tracing::info!("stopping sync engine");
    sync_engine.shutdown().await;

    Ok(())
}

/// Initialise the `tracing` subscriber. Honours `RUST_LOG` if set, otherwise
/// falls back to the configured log level.
fn init_tracing(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}

/// Resolve when the process receives Ctrl-C or (on Unix) SIGTERM, so the server
/// drains in-flight requests before exiting — important under a container
/// orchestrator.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
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
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
