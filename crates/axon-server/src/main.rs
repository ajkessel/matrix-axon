//! Axon binary — wires all crates together and owns the process.
//!
//! Boot sequence: load config, initialize tracing, connect the store (running
//! migrations), build the router, then serve until a shutdown signal arrives.
//! `anyhow` is used here at the binary boundary; library crates use `thiserror`.

use std::future::IntoFuture;
use std::time::Duration;

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

    let app = axon_api::router(store);

    let addr = config.socket_addr();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    tracing::info!(%addr, "axon listening");

    // Serve until a shutdown signal arrives. We `select!` the server against the
    // signal rather than using `with_graceful_shutdown` so that pressing Ctrl-C
    // stops serving *immediately* — a stuck or idle keep-alive connection can't
    // wedge shutdown waiting for it to drain.
    tokio::select! {
        result = axum::serve(listener, app).into_future() => result.context("server error")?,
        _ = shutdown_signal() => tracing::info!("shutdown signal received"),
    }

    // Wind down the sync tasks so their SDK stores flush. But never let a hung
    // SDK `stop()` (e.g. blocked on an in-flight 30s long-poll) wedge process
    // exit: cap the drain, and let a second Ctrl-C force an immediate exit.
    tracing::info!("stopping sync engine");
    tokio::select! {
        _ = sync_engine.shutdown() => tracing::info!("sync engine stopped"),
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            tracing::warn!("sync engine did not stop within 10s; exiting anyway");
        }
        _ = shutdown_signal() => tracing::warn!("second shutdown signal; exiting immediately"),
    }

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
