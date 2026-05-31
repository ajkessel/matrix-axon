//! The sync engine: one supervised task per account.
//!
//! [`SyncEngine::start`] provisions the configured account, then spawns a task
//! per account. Each task builds a [`Client`](matrix_sdk::Client), starts a
//! [`SyncService`] (Simplified Sliding Sync, MSC4186), and watches its state.
//! If the service errors or terminates unexpectedly the task restarts it with
//! exponential backoff; a cancellation token drives graceful shutdown.

use std::time::Duration;

use axon_core::{AccountProvision, SyncConfig};
use axon_store::{Account, Store};
use matrix_sdk_ui::sync_service::{State, SyncService};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::client::connect_account;
use crate::error::{sdk_err, SyncError};

/// Backoff bounds for restarting a failed per-account task.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Owns the per-account sync tasks. Dropping it does not stop the tasks; call
/// [`SyncEngine::shutdown`] to cancel and join them cleanly.
pub struct SyncEngine {
    handles: Vec<JoinHandle<()>>,
    cancel: CancellationToken,
}

impl SyncEngine {
    /// Provision the configured account (if any), then spawn one supervised sync
    /// task per account in the store. Returns once tasks are spawned; call
    /// [`SyncEngine::shutdown`] to stop them.
    pub async fn start(store: Store, config: SyncConfig) -> Result<Self, SyncError> {
        let cancel = CancellationToken::new();
        if let Some(provision) = &config.account {
            // Validate the credential up front so a misconfiguration fails fast
            // with a readable error rather than inside a spawned task.
            provision.credential()?;
            let account = store
                .upsert_account(&provision.user_id, &provision.homeserver_url)
                .await?;
            tracing::info!(account_id = %account.account_id, user_id = %account.user_id, "provisioned account");
        }

        let accounts = store.list_accounts().await?;
        if accounts.is_empty() {
            tracing::warn!("no accounts configured; sync engine idle");
        }

        let handles = accounts
            .into_iter()
            .map(|account| {
                let store = store.clone();
                let config = config.clone();
                let cancel = cancel.clone();
                tokio::spawn(supervise_account(store, config, account, cancel))
            })
            .collect();

        Ok(SyncEngine { handles, cancel })
    }

    /// Cancel all per-account tasks and wait for them to finish. Safe to call
    /// without canceling the token first — this method cancels it internally.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        for handle in self.handles {
            if let Err(err) = handle.await {
                tracing::warn!(error = %err, "sync task did not shut down cleanly");
            }
        }
    }
}

/// Supervise a single account: run it, and on failure restart with exponential
/// backoff until the cancellation token fires.
async fn supervise_account(
    store: Store,
    config: SyncConfig,
    account: Account,
    cancel: CancellationToken,
) {
    let mut backoff = BACKOFF_START;

    loop {
        if cancel.is_cancelled() {
            return;
        }

        match run_account(&store, &config, &account, &cancel).await {
            Ok(()) => {
                // Clean stop (cancellation requested).
                return;
            }
            Err(err) => {
                tracing::error!(
                    account_id = %account.account_id,
                    error = %err,
                    backoff_secs = backoff.as_secs(),
                    "account sync task failed; restarting after backoff"
                );
            }
        }

        // Wait out the backoff, but wake immediately on cancellation.
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Run one account's sync to completion: authenticate, start the sync service,
/// and monitor its state until cancellation (returns `Ok`) or an error/terminal
/// state (returns `Err`, triggering a supervised restart).
async fn run_account(
    store: &Store,
    config: &SyncConfig,
    account: &Account,
    cancel: &CancellationToken,
) -> Result<(), SyncError> {
    let credential = credential_for(config, account)?;
    let client = connect_account(store, account, config, credential).await?;

    let sync_service = SyncService::builder(client)
        .build()
        .await
        .map_err(sdk_err)?;
    sync_service.start().await;
    tracing::info!(account_id = %account.account_id, "sync service started");

    let mut state = sync_service.state();
    let result = loop {
        tokio::select! {
            _ = cancel.cancelled() => break Ok(()),
            next = state.next() => match next {
                Some(State::Running) | Some(State::Idle) | Some(State::Offline) => continue,
                Some(State::Error(err)) => break Err(SyncError::Sdk(format!("sync service error: {err}"))),
                Some(State::Terminated) => break Err(SyncError::Sdk("sync service terminated".into())),
                // The state stream ended; treat as a terminal condition.
                None => break Err(SyncError::Sdk("sync service state stream closed".into())),
            },
        }
    };

    // Always drain the service so its SQLite store flushes before we drop it.
    sync_service.stop().await;
    result
}

/// Resolve the login credential for `account` from the configured provision,
/// matching on `(user_id, homeserver_url)`. Returns `None` if no provision
/// matches (the account must then have a stored session to authenticate).
fn credential_for<'c>(
    config: &'c SyncConfig,
    account: &Account,
) -> Result<Option<axon_core::Credential<'c>>, SyncError> {
    let Some(provision) = config
        .account
        .as_ref()
        .filter(|p| matches_account(p, account))
    else {
        return Ok(None);
    };
    Ok(Some(provision.credential()?))
}

/// Whether a configured provision refers to the same account as a stored row.
fn matches_account(provision: &AccountProvision, account: &Account) -> bool {
    provision.user_id == account.user_id && provision.homeserver_url == account.homeserver_url
}
