//! Axon binary — wires all crates together and owns the process.
//!
//! Boot sequence: load config, initialize tracing, install the shutdown-signal
//! handler, connect the store (running migrations), build the router, then serve
//! until a shutdown signal arrives. `anyhow` is used here at the binary
//! boundary; library crates use `thiserror`.
//!
//! Shutdown signals are handled by *blocking* SIGINT/SIGTERM process-wide and
//! dequeuing them with `sigwait` on a dedicated thread, rather than via a signal
//! handler (e.g. `tokio::signal::ctrl_c`). A compiled C dependency in the
//! matrix-sdk crypto stack installs its own `sigaction(SIGINT, …)` during
//! encryption setup, which clobbers any handler-based approach and left Ctrl-C
//! dead. Blocked signals never invoke a handler, so `sigwait` keeps working no
//! matter who calls `sigaction` afterward.

use std::future::IntoFuture;
use std::time::Duration;

use anyhow::Context;
use axon_core::Config;
use axon_store::Store;
use axon_sync::SyncEngine;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

fn main() -> anyhow::Result<()> {
    // Load .env if present. Silently skipped when the file doesn't exist so
    // container / CI deployments that rely purely on environment variables
    // continue to work unchanged.
    let _ = dotenvy::dotenv();

    // Config first, so we know how to configure logging.
    let config = Config::load_default().context("loading configuration")?;
    init_tracing(&config.log.level);

    // Install shutdown handling *before* building the runtime so the runtime's
    // worker threads inherit the blocked signal mask (see module docs).
    let shutdown = CancellationToken::new();
    #[cfg(unix)]
    install_unix_signal_handler(shutdown.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    runtime.block_on(run(config, shutdown))
}

/// Async entry point: connect the store, start sync, serve HTTP, and drain
/// cleanly when `shutdown` fires.
async fn run(config: Config, shutdown: CancellationToken) -> anyhow::Result<()> {
    // Non-unix has no sigwait; fall back to tokio's Ctrl-C handler there.
    #[cfg(not(unix))]
    {
        let token = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                token.cancel();
            }
        });
    }

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
    // cancellation token rather than using `with_graceful_shutdown` so a stuck
    // or idle keep-alive connection can't wedge shutdown waiting for it to drain.
    tokio::select! {
        result = axum::serve(listener, app).into_future() => result.context("server error")?,
        _ = shutdown.cancelled() => tracing::info!("draining after shutdown signal"),
    }

    // Wind down the sync tasks so their SDK stores flush. But never let a hung
    // SDK `stop()` (e.g. blocked on an in-flight 30s long-poll) wedge process
    // exit: cap the drain. A second signal force-exits via the signal thread.
    tracing::info!("stopping sync engine");
    tokio::select! {
        _ = sync_engine.shutdown() => tracing::info!("sync engine stopped"),
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            tracing::warn!("sync engine did not stop within 10s; exiting anyway");
        }
    }

    Ok(())
}

/// Block SIGINT/SIGTERM on the calling (main) thread and spawn a dedicated
/// thread that dequeues them with `sigwait`. Must be called before the tokio
/// runtime is built so its worker threads inherit the block. The first signal
/// requests a graceful shutdown via `shutdown`; a second forces an immediate
/// exit.
#[cfg(unix)]
fn install_unix_signal_handler(shutdown: CancellationToken) {
    // SAFETY: standard POSIX signal-set construction and masking. The libc calls
    // operate only on the stack-allocated `set`; the spawned thread owns its own
    // copy (`sigset_t: Copy`) and uses it solely for `sigwait`.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);

        if libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) != 0 {
            tracing::warn!("failed to block shutdown signals; Ctrl-C may not work");
            return;
        }

        std::thread::Builder::new()
            .name("axon-signal".to_owned())
            .spawn(move || {
                let mut sig: libc::c_int = 0;
                // First signal: request graceful shutdown.
                if libc::sigwait(&set, &mut sig) != 0 {
                    return;
                }
                tracing::info!(signal = sig, "shutdown signal received");
                shutdown.cancel();
                // Any subsequent signal: force an immediate exit.
                let _ = libc::sigwait(&set, &mut sig);
                tracing::warn!("second shutdown signal; exiting immediately");
                std::process::exit(130);
            })
            .expect("failed to spawn signal handler thread");
    }
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
