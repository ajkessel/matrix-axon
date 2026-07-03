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
mod media;
mod search;
mod status;
mod token;
mod trust;
mod verification;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axon_core::Config;
use axon_store::Store;
use axon_sync::SyncEngine;
use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::cli::{Cli, Command};
use crate::gateway::GatewayAdapter;
use crate::lifecycle::LifecycleAdapter;
use crate::media::MediaProxyAdapter;
use crate::trust::TrustAdapter;
use crate::verification::VerificationAdapter;

const SEARCH_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

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
        Some(Command::Search { action }) => search::run(action, &config),
        None => serve(config).await,
    }
}

/// Run the long-lived HTTP/WebSocket server until a shutdown signal arrives.
async fn serve(config: Config) -> anyhow::Result<()> {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        git_hash = env!("GIT_HASH"),
        "axon starting",
    );

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

    // Open the search index and spawn its background indexing actor (M9), when
    // enabled. The actor owns the sole Tantivy writer. `open` reports whether the
    // physical index is `fresh` (new, empty, or built against a different schema
    // version); if so the actor seeds the corpus from the store as a background
    // task — boot never blocks on it, even for a large corpus. Either way it then
    // drains the durable `search_outbox` change log, indexing live events as they
    // arrive. We hand its producer handle to the sync engine. We also retain the
    // read side (`SearchIndex`) and adapt it onto the API's `SearchQuery` port so
    // `GET /v1/search` can query it (M9b); the actor's writer and our retained Arc
    // both keep the underlying index alive.
    let (index_handle, index_join, index_cancel, search_port) = if config.search.enabled {
        let (search_index, fresh) = axon_search::SearchIndex::open(&config.search.index_path)
            .context("opening search index")?;
        let search_index = Arc::new(search_index);
        let handles = search_index
            .spawn_indexer(store.clone(), fresh, indexer_options(&config.search))
            .context("starting search indexer")?;
        let search_port: Arc<dyn axon_api::SearchQuery> = Arc::new(search::SearchAdapter::new(
            search_index,
            config.search.max_concurrent_queries,
            std::time::Duration::from_millis(config.search.query_timeout_ms),
        ));
        (
            Some(handles.handle),
            Some(handles.join),
            Some(handles.cancel),
            Some(search_port),
        )
    } else {
        tracing::info!("search disabled (search.enabled = false); not indexing");
        (None, None, None, None)
    };

    // Start the sync engine: it provisions the configured account and runs one
    // supervised Simplified Sliding Sync task per account.
    let sync_engine = SyncEngine::start(store.clone(), config.sync.clone(), index_handle)
        .await
        .context("starting sync engine")?;

    // The API shares the sync engine's live-event bus so `/v1/ws` can fan out
    // events as they're persisted, its message gateway (adapted onto the API's
    // MessageSender port) so the mutation routes can send via the SDK, its
    // lifecycle engine (adapted onto the AccountLifecycle port) so the login route
    // can add/reactivate accounts at runtime, and its verification engine (adapted
    // onto the VerificationService port) so the verify routes can drive SAS flows,
    // and its sender-trust engine (adapted onto the SenderTrustService port) so the
    // verification-bundle route can read per-event trust (M7c). The bearer-token
    // verifier (M7b) is backed straight by the store.
    let sender = Arc::new(GatewayAdapter(sync_engine.gateway()));
    let lifecycle = Arc::new(LifecycleAdapter(sync_engine.lifecycle()));
    let verify = Arc::new(VerificationAdapter(sync_engine.verification()));
    let trust = Arc::new(TrustAdapter(sync_engine.sender_trust()));
    let verifier = Arc::new(axon_api::StoreTokenVerifier::new(store.clone()));
    let media = Arc::new(MediaProxyAdapter(sync_engine.media_proxy()));
    let backfill_status = Arc::new(status::BackfillStatusAdapter(sync_engine.backfill_health()));
    let app = axon_api::router(
        axon_api::AppState::new(
            store,
            sync_engine.live_events(),
            sender,
            lifecycle,
            verify,
            trust,
            verifier,
            media,
            search_port,
        )
        .with_backfill_status(backfill_status),
    );

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    tracing::info!(%addr, "axon listening");

    axum::serve(listener, app.into_make_service())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    // HTTP has drained; now wind down the sync tasks and wait for them to flush
    // their SDK stores before exiting. This drops every `IndexHandle` the sync
    // engine held, closing the search actor's channel.
    tracing::info!("stopping sync engine");
    let (sync_drained, search_drained) = tokio::join!(
        sync_engine.shutdown(),
        shutdown_search_indexer(index_cancel, index_join),
    );
    if !sync_drained || !search_drained {
        anyhow::bail!(
            "shutdown completed with timed-out tasks: sync_drained={sync_drained}, \
             search_drained={search_drained}"
        );
    }

    Ok(())
}

async fn shutdown_search_indexer(
    index_cancel: Option<tokio_util::sync::CancellationToken>,
    index_join: Option<tokio::task::JoinHandle<()>>,
) -> bool {
    // The search actor normally exits once all IndexHandles are dropped (channel
    // close). However, the matrix-sdk keeps internal background tasks that hold an
    // Arc to the Client — and the PersistContext (with its IndexHandle) lives inside
    // that Arc — so the channel may never close. Cancel the actor directly to ensure
    // a prompt exit; the durable outbox self-heals any un-drained work on next boot.
    if let Some(cancel) = index_cancel {
        cancel.cancel();
    }
    let Some(join) = index_join else {
        return true;
    };

    tracing::info!("stopping search indexer");
    match tokio::time::timeout(SEARCH_SHUTDOWN_DRAIN_TIMEOUT, join).await {
        Ok(_) => true,
        Err(_) => {
            tracing::warn!(
                timeout_secs = SEARCH_SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
                "search indexer did not stop within the timeout; continuing shutdown"
            );
            false
        }
    }
}

/// Build the search indexer's tuning from config. The channel capacity and idle
/// poll cadence are fixed (not operator-facing); the batch size, seed throttle, and
/// writer heap come from `[search]`.
fn indexer_options(search: &axon_core::SearchConfig) -> axon_search::IndexerOptions {
    axon_search::IndexerOptions {
        // The channel only carries coalescing wakeup hints (the durable obligation
        // is the outbox), so a small bound is plenty; a full channel just drops a
        // redundant hint.
        channel_capacity: 64,
        // Drain at least this often even without a wakeup — the safety net against a
        // dropped hint, and the steady-state poll when notifications are quiet.
        idle_poll_interval: std::time::Duration::from_secs(2),
        writer_heap_mb: search.writer_heap_mb,
        batch_size: search.index_batch_size,
        seed_throttle: std::time::Duration::from_millis(search.build_throttle_ms),
    }
}

/// Initialize the `tracing` subscriber. Honors `RUST_LOG` if set, otherwise
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
