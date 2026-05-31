//! The sync engine: one supervised task per account.
//!
//! [`SyncEngine::start`] provisions the configured account, then spawns a task
//! per account. Each task builds a [`Client`](matrix_sdk::Client), starts a
//! [`SyncService`] (Simplified Sliding Sync, MSC4186), and watches its state.
//! If the service errors or terminates unexpectedly the task restarts it with
//! exponential backoff; a cancellation token drives graceful shutdown.

use std::time::Duration;

use axon_core::{AccountProvision, SyncConfig};
use axon_store::{Account, NewEvent, Store};
use matrix_sdk::event_handler::{Ctx, RawEvent};
use matrix_sdk::ruma::events::AnySyncTimelineEvent;
use matrix_sdk::Room;
use matrix_sdk_ui::sync_service::{State, SyncService};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

/// Shared context injected into the per-account event handler.
#[derive(Clone)]
struct PersistContext {
    store: Store,
    account_id: Uuid,
}

/// Event handler: persist every synced timeline event to Postgres.
///
/// For E2EE rooms, matrix-rust-sdk decrypts the megolm payload before
/// dispatching, so `ev` and `raw` already carry the plaintext content. UTDs
/// arrive as `m.room.encrypted` events with the ciphertext as content; the
/// re-decryption queue (M3c) will back-fill their `content` column.
async fn persist_timeline_event(
    ev: AnySyncTimelineEvent,
    room: Room,
    raw: RawEvent,
    Ctx(ctx): Ctx<PersistContext>,
) {
    let raw_val: serde_json::Value = match serde_json::from_str(raw.get()) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(
                account_id = %ctx.account_id,
                error = %err,
                "failed to parse raw event JSON; skipping persistence"
            );
            return;
        }
    };

    // Extract event_type as an owned String so raw_val can be moved into NewEvent below.
    let event_type: String = raw_val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    // An event that is still `m.room.encrypted` at dispatch is one the SDK could
    // not decrypt (a UTD): its `content` is the megolm ciphertext envelope, not
    // plaintext. Persist `content = NULL` so the column means "decrypted payload"
    // — `content IS NOT NULL` is then a true decrypted signal, and the M3c
    // re-decryption queue can find pending UTDs by `content IS NULL`. The full
    // ciphertext (incl. `session_id`) is preserved in `raw_event` for re-decryption.
    // Once the SDK decrypts a megolm event it dispatches it with the cleartext
    // type, so this branch is skipped and the real plaintext content is stored.
    let content = if event_type == "m.room.encrypted" {
        None
    } else {
        raw_val.get("content").cloned()
    };
    let origin_ts = i64::try_from(u64::from(ev.origin_server_ts().0)).unwrap_or(i64::MAX);

    let new_ev = NewEvent {
        event_id: ev.event_id().as_str(),
        room_id: room.room_id().as_str(),
        account_id: ctx.account_id,
        sender: ev.sender().as_str(),
        origin_ts,
        event_type: &event_type,
        content,
        raw_event: raw_val,
    };

    if let Err(err) = ctx.store.upsert_event(&new_ev).await {
        tracing::warn!(
            account_id = %ctx.account_id,
            event_id = %ev.event_id(),
            error = %err,
            "failed to persist event"
        );
    } else {
        tracing::debug!(
            account_id = %ctx.account_id,
            event_id = %ev.event_id(),
            room_id = %room.room_id(),
            event_type = event_type.as_str(),
            "persisted event"
        );
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

    // Register event persistence before starting the sync service so no events
    // are missed between SyncService::start() and handler registration.
    let persist_ctx = PersistContext {
        store: store.clone(),
        account_id: account.account_id,
    };
    client.add_event_handler_context(persist_ctx);
    client.add_event_handler(persist_timeline_event);

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
