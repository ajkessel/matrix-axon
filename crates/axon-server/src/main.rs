//! Axon binary — wires all crates together and owns the process.
//!
//! With no subcommand it runs the server. Boot sequence: load config, initialize
//! tracing, connect the store (running migrations), build the router, then serve
//! until a shutdown signal arrives. The `token` subcommand (M7b) is a short-lived
//! DB-only path for managing client bearer tokens — see [`token`].
//! `anyhow` is used here at the binary boundary; library crates use `thiserror`.

mod cli;
mod gateway;
mod lifecycle;
mod token;
mod verification;

use std::sync::Arc;

use anyhow::Context;
use axon_core::Config;
use axon_store::Store;
use axon_sync::SyncEngine;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::cli::{Cli, Command};
use crate::gateway::GatewayAdapter;
use crate::lifecycle::LifecycleAdapter;
use crate::verification::VerificationAdapter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present. Silently skipped when the file doesn't exist so
    // container / CI deployments that rely purely on environment variables
    // continue to work unchanged.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    // Config first, so we know how to configure logging.
    let config = Config::load_default().context("loading configuration")?;

    init_tracing(&config.log.level);

    match cli.command {
        Some(Command::Token { action }) => token::run(action, &config).await,
        None => serve(config).await,
    }
}

/// Run the long-lived HTTP/WebSocket server until a shutdown signal arrives.
async fn serve(config: Config) -> anyhow::Result<()> {
    // Fail fast (before any side effects) on an unsafe bind. Axon serves plain
    // HTTP and the /v1 API carries credentials (login passwords, recovery keys,
    // bearer tokens); the tech spec requires client↔Axon TLS. So a non-loopback
    // bind is refused unless the operator explicitly accepts cleartext on the
    // wire — the safe setup is loopback + a TLS-terminating reverse proxy.
    let addr = config.socket_addr();
    if !addr.ip().is_loopback() {
        anyhow::ensure!(
            config.server.allow_insecure_bind,
            "refusing to bind non-loopback address {addr} over plain HTTP: Axon serves \
             credentials in cleartext. Front it with a TLS-terminating reverse proxy and bind \
             loopback, or set server.allow_insecure_bind = true \
             (AXON_SERVER__ALLOW_INSECURE_BIND=true) to override on a trusted network.",
        );
        tracing::warn!(
            %addr,
            "binding a non-loopback address over plain HTTP (server.allow_insecure_bind set); \
             ensure a TLS reverse proxy or trusted private network fronts Axon"
        );
    }

    let store = Store::connect(&config.database.url, config.database.max_connections)
        .await
        .context("connecting to database")?;

    // Start the sync engine: it provisions the configured account and runs one
    // supervised Simplified Sliding Sync task per account.
    let sync_engine = SyncEngine::start(store.clone(), config.sync.clone())
        .await
        .context("starting sync engine")?;

    // The API shares the sync engine's live-event bus so `/v1/ws` can fan out
    // events as they're persisted, its message gateway (adapted onto the API's
    // MessageSender port) so the mutation routes can send via the SDK, its
    // lifecycle engine (adapted onto the AccountLifecycle port) so the login route
    // can add/reactivate accounts at runtime, and its verification engine (adapted
    // onto the VerificationService port) so the verify routes can drive SAS flows.
    // The bearer-token verifier (M7b) is backed straight by the store.
    let sender = Arc::new(GatewayAdapter(sync_engine.gateway()));
    let lifecycle = Arc::new(LifecycleAdapter(sync_engine.lifecycle()));
    let verify = Arc::new(VerificationAdapter(sync_engine.verification()));
    let verifier = Arc::new(axon_api::StoreTokenVerifier::new(store.clone()));
    let app = axon_api::router(axon_api::AppState::new(
        store,
        sync_engine.live_events(),
        sender,
        lifecycle,
        verify,
        verifier,
    ));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    tracing::info!(%addr, "axon listening");

    axum::serve(listener, app.into_make_service())
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
