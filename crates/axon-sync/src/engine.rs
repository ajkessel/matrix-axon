//! The sync engine: one supervised task per account.
//!
//! [`SyncEngine::start`] provisions the configured account, then spawns a task
//! per account. Each task builds a [`Client`](matrix_sdk::Client), starts a
//! [`SyncService`] (Simplified Sliding Sync, MSC4186), and watches its state.
//! If the service errors or terminates unexpectedly the task restarts it with
//! exponential backoff; a cancellation token drives graceful shutdown.

use std::time::Duration;

use axon_core::{LiveEvent, SyncConfig};
use axon_store::{Account, AccountDataUpsert, EventCiphertext, NewEvent, RoomStateUpsert, Store};
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::event_handler::{Ctx, RawEvent};
use matrix_sdk::ruma::events::{
    AnyGlobalAccountDataEvent, AnyRoomAccountDataEvent, AnySyncStateEvent, AnySyncTimelineEvent,
};
use matrix_sdk::Room;
use matrix_sdk_ui::sync_service::{State, SyncService};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::client::matches_account;
use crate::error::{sdk_err, SyncError};
use crate::gateway::SdkGateway;
use crate::manager::ClientManager;
use crate::redecrypt;

/// Backoff bounds for restarting a failed per-account task.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Owns the per-account sync tasks. Dropping it does not stop the tasks; call
/// [`SyncEngine::shutdown`] to cancel and join them cleanly.
pub struct SyncEngine {
    handles: Vec<JoinHandle<()>>,
    cancel: CancellationToken,
    /// Producer end of the live-event bus. The sync tasks publish through clones
    /// of this; [`SyncEngine::live_events`] hands a clone to the API layer so
    /// each WebSocket connection can `subscribe()`.
    live_tx: broadcast::Sender<LiveEvent>,
    /// Per-account client lifecycle. Shared by the supervised sync tasks (which
    /// drive connects + retry) and the message gateway handed to the API layer
    /// (see [`SyncEngine::gateway`]).
    manager: ClientManager,
}

impl SyncEngine {
    /// Provision the configured account (if any), then spawn one supervised sync
    /// task per account in the store. Returns once tasks are spawned; call
    /// [`SyncEngine::shutdown`] to stop them.
    pub async fn start(store: Store, config: SyncConfig) -> Result<Self, SyncError> {
        let cancel = CancellationToken::new();
        // The bus exists for the lifetime of the engine regardless of how many
        // accounts there are (zero accounts → an idle but valid `/v1/ws`). The
        // held `_rx` is dropped immediately; `broadcast` keeps the channel open
        // as long as a `Sender` exists, so this does not close it. Capacity is
        // configurable (`sync.live_event_buffer`) — see that field's docs.
        let (live_tx, _rx) = broadcast::channel(config.live_event_buffer);
        // The client manager is the single owner of per-account clients; both the
        // supervised sync tasks and the gateway pull from it.
        let manager = ClientManager::new(store.clone(), config.clone());
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
                let live_tx = live_tx.clone();
                let manager = manager.clone();
                tokio::spawn(supervise_account(
                    store, config, account, cancel, live_tx, manager,
                ))
            })
            .collect();

        Ok(SyncEngine {
            handles,
            cancel,
            live_tx,
            manager,
        })
    }

    /// A message gateway over the per-account clients, for the API layer's send
    /// path. `axon-server` wraps this in an adapter implementing its
    /// `MessageSender` port; the returned value is cheap to construct and clone.
    pub fn gateway(&self) -> SdkGateway {
        SdkGateway::new(self.manager.clone())
    }

    /// A producer handle for the live-event bus. The API layer holds this in its
    /// router state and calls [`broadcast::Sender::subscribe`] once per
    /// `/v1/ws` connection. Cloning is cheap and does not affect delivery.
    pub fn live_events(&self) -> broadcast::Sender<LiveEvent> {
        self.live_tx.clone()
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
    live_tx: broadcast::Sender<LiveEvent>,
    manager: ClientManager,
) {
    let mut backoff = BACKOFF_START;

    loop {
        if cancel.is_cancelled() {
            return;
        }

        match run_account(&store, &config, &account, &cancel, &live_tx, &manager).await {
            Ok(()) => {
                // Clean stop (cancellation requested).
                return;
            }
            Err(err) => {
                // Drop the cached client so the next attempt reconnects cleanly
                // (a stale session/connection won't be reused across a restart).
                manager.evict(account.account_id);
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
    /// Producer end of the live-event bus; [`persist_timeline_event`] publishes
    /// each freshly persisted event to it for `/v1/ws` fan-out.
    live_tx: broadcast::Sender<LiveEvent>,
}

/// Event handler: persist every synced timeline event to Postgres.
///
/// For E2EE rooms, matrix-rust-sdk decrypts the megolm payload before
/// dispatching, so `ev` and `raw` already carry the plaintext content and
/// `enc_info` describes how it was decrypted. UTDs arrive as `m.room.encrypted`
/// events with the ciphertext as content and `enc_info = None`; the
/// re-decryption queue back-fills their `content` once keys arrive.
///
/// Alongside the `events` row this writes the crypto sibling rows (ADR 0015): the
/// ciphertext sibling for UTDs (the only events whose ciphertext the SDK hands
/// us), and the crypto-provenance siblings from `enc_info` for decrypted events.
async fn persist_timeline_event(
    ev: AnySyncTimelineEvent,
    room: Room,
    raw: RawEvent,
    enc_info: Option<EncryptionInfo>,
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
    let is_utd = event_type == "m.room.encrypted";
    // An event that is still `m.room.encrypted` at dispatch is one the SDK could
    // not decrypt (a UTD): its `content` is the megolm ciphertext envelope, not
    // plaintext. Persist `content = NULL` so the column means "decrypted payload"
    // — `content IS NOT NULL` is then a true decrypted signal, and the
    // re-decryption queue can find pending UTDs by `content IS NULL`. The full
    // ciphertext (incl. `session_id`) is preserved in `raw_event` for re-decryption.
    // Once the SDK decrypts a megolm event it dispatches it with the cleartext
    // type, so this branch is skipped and the real plaintext content is stored.
    let content = if is_utd {
        None
    } else {
        raw_val.get("content").cloned()
    };
    // For a UTD, lift the megolm `session_id` into its own column so the
    // re-decryption queue can match arriving room keys to this row without
    // re-parsing the envelope. Owned (not borrowed from `raw_val`) so `raw_val`
    // can still move into `raw_event` below.
    let megolm_session_id: Option<String> = if is_utd {
        crate::redecrypt::megolm_session_id(&raw_val).map(str::to_owned)
    } else {
        None
    };
    // Hot columns. `redacts` applies to redaction events (never encrypted);
    // `relates_to` / `decrypted_body_text` come from the plaintext content, so
    // they're only available once decrypted (a re-decrypted UTD picks them up via
    // the re-decryption back-fill, not here). Owned so raw_val can still move.
    let redacts: Option<String> = crate::meta::redacts(&raw_val).map(str::to_owned);
    let relates_to = content.as_ref().and_then(crate::meta::relates_to);
    let decrypted_body_text: Option<String> = content
        .as_ref()
        .and_then(|c| crate::meta::body_text(c).map(str::to_owned));
    // Capture the ciphertext envelope before raw_val is moved — only UTDs carry it.
    let ciphertext = if is_utd {
        raw_val.get("content").cloned()
    } else {
        None
    };
    let origin_ts = i64::try_from(u64::from(ev.origin_server_ts().0)).unwrap_or(i64::MAX);
    let event_id = ev.event_id().as_str().to_owned();
    let room_id = room.room_id().as_str().to_owned();
    let state_key = raw_val
        .get("state_key")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    let new_ev = NewEvent {
        event_id: &event_id,
        room_id: &room_id,
        account_id: ctx.account_id,
        sender: ev.sender().as_str(),
        origin_ts,
        event_type: &event_type,
        content,
        raw_event: raw_val,
        megolm_session_id: megolm_session_id.as_deref(),
        redacts: redacts.as_deref(),
        relates_to,
        decrypted_body_text: decrypted_body_text.as_deref(),
    };

    if let Err(err) = ctx.store.upsert_event(&new_ev).await {
        // Don't write sibling rows if the event row didn't land — they FK to it.
        tracing::warn!(
            account_id = %ctx.account_id,
            event_id = %event_id,
            error = %err,
            "failed to persist event"
        );
        return;
    }
    tracing::debug!(
        account_id = %ctx.account_id,
        event_id = %event_id,
        room_id = %room_id,
        event_type = event_type.as_str(),
        "persisted event"
    );

    // Fan the event out to any live `/v1/ws` subscribers. Skip the work entirely
    // when nobody is listening (the common case for a headless server) so we
    // don't clone the content needlessly. `send` errors only when there are no
    // receivers — harmless to ignore (a receiver may have dropped between the
    // count check and the send), and never fatal to sync.
    if ctx.live_tx.receiver_count() > 0 {
        let _ = ctx.live_tx.send(LiveEvent {
            account_id: ctx.account_id,
            event_id: event_id.clone(),
            room_id: room_id.clone(),
            sender: ev.sender().as_str().to_owned(),
            state_key: state_key.clone(),
            origin_ts,
            event_type: event_type.clone(),
            content: new_ev.content.clone(),
            body: decrypted_body_text.clone(),
            relates_to: new_ev.relates_to.clone(),
        });
    }

    // Sibling rows are best-effort: a failure here must not take down sync.
    persist_event_siblings(&ctx, &event_id, &room_id, ciphertext, enc_info.as_ref()).await;
}

/// Write the crypto sibling rows for an event already persisted to `events`.
/// `ciphertext` is the `m.room.encrypted` content for UTDs (`None` otherwise);
/// `enc_info` is the SDK decryption info for decrypted events (`None` for UTDs).
async fn persist_event_siblings(
    ctx: &PersistContext,
    event_id: &str,
    room_id: &str,
    ciphertext: Option<serde_json::Value>,
    enc_info: Option<&EncryptionInfo>,
) {
    if let Some(ciphertext) = ciphertext {
        let algorithm = ciphertext
            .get("algorithm")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let sender_key = ciphertext
            .get("sender_key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let session_id = ciphertext
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let row = EventCiphertext {
            account_id: ctx.account_id,
            event_id,
            room_id,
            algorithm: &algorithm,
            sender_key: sender_key.as_deref(),
            session_id: session_id.as_deref(),
            ciphertext,
        };
        if let Err(err) = ctx.store.insert_event_ciphertext(&row).await {
            tracing::warn!(account_id = %ctx.account_id, event_id, error = %err, "failed to persist ciphertext sibling");
        }
    }

    if let Some(info) = enc_info {
        let meta = crate::meta::crypto_meta(info);
        if let Err(err) = ctx
            .store
            .upsert_event_crypto(&meta.as_event_crypto(ctx.account_id, event_id))
            .await
        {
            tracing::warn!(account_id = %ctx.account_id, event_id, error = %err, "failed to persist crypto sibling");
        }
    }
}

/// Event handler: project a room-state event into the `room_state` table (the
/// derived current-value view, maintained by upsert). The raw state event is
/// also persisted to `events` by [`persist_timeline_event`]; this writes the
/// resolved tuple a room-summary read needs. Identity fields come from the typed
/// event; `type`/`state_key`/`content` from the raw JSON so the exact content
/// (incl. unknown fields) is preserved.
async fn persist_room_state_event(
    ev: AnySyncStateEvent,
    room: Room,
    raw: RawEvent,
    Ctx(ctx): Ctx<PersistContext>,
) {
    let raw_val: serde_json::Value = match serde_json::from_str(raw.get()) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(account_id = %ctx.account_id, error = %err, "failed to parse raw state event JSON; skipping");
            return;
        }
    };
    let event_type = raw_val
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    // Singleton state (m.room.name, m.room.topic) carries state_key "".
    let state_key = raw_val
        .get("state_key")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let content = raw_val.get("content").cloned();
    let event_id = ev.event_id().as_str().to_owned();
    let sender = ev.sender().as_str().to_owned();
    let origin_ts = i64::try_from(u64::from(ev.origin_server_ts().0)).unwrap_or(i64::MAX);
    let room_id = room.room_id().as_str().to_owned();

    let upsert = RoomStateUpsert {
        account_id: ctx.account_id,
        room_id: &room_id,
        event_type: &event_type,
        state_key: &state_key,
        event_id: &event_id,
        sender: &sender,
        origin_ts,
        content,
    };
    if let Err(err) = ctx.store.upsert_room_state(&upsert).await {
        tracing::warn!(account_id = %ctx.account_id, room_id = %room_id, event_type = event_type.as_str(), error = %err, "failed to persist room state");
    } else {
        tracing::debug!(account_id = %ctx.account_id, room_id = %room_id, event_type = event_type.as_str(), state_key = state_key.as_str(), "persisted room state");
    }
}

/// Event handler: per-room account data (fully-read markers, tags, …) → the
/// `account_data` table, scoped to the room.
async fn persist_room_account_data(
    _ev: AnyRoomAccountDataEvent,
    room: Room,
    raw: RawEvent,
    Ctx(ctx): Ctx<PersistContext>,
) {
    let room_id = room.room_id().as_str().to_owned();
    persist_account_data(&ctx, Some(&room_id), &raw).await;
}

/// Event handler: global (account-wide) account data (push rules, m.direct,
/// ignored users, …) → the `account_data` table, global scope. No `Room` arg —
/// global account data has no room.
async fn persist_global_account_data(
    _ev: AnyGlobalAccountDataEvent,
    raw: RawEvent,
    Ctx(ctx): Ctx<PersistContext>,
) {
    persist_account_data(&ctx, None, &raw).await;
}

/// Shared account-data upsert for both scopes. `room_id = None` is global.
/// Account-data events carry only `type` + `content` (no event_id/sender/ts),
/// both read from the raw JSON; `content` is required (the column is NOT NULL).
async fn persist_account_data(ctx: &PersistContext, room_id: Option<&str>, raw: &RawEvent) {
    let raw_val: serde_json::Value = match serde_json::from_str(raw.get()) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(account_id = %ctx.account_id, error = %err, "failed to parse raw account data JSON; skipping");
            return;
        }
    };
    let event_type = raw_val
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let Some(content) = raw_val.get("content").cloned() else {
        tracing::warn!(account_id = %ctx.account_id, event_type = event_type.as_str(), "account data event has no content; skipping");
        return;
    };

    let upsert = AccountDataUpsert {
        account_id: ctx.account_id,
        room_id,
        event_type: &event_type,
        content,
    };
    if let Err(err) = ctx.store.upsert_account_data(&upsert).await {
        tracing::warn!(account_id = %ctx.account_id, room_id = ?room_id, event_type = event_type.as_str(), error = %err, "failed to persist account data");
    } else {
        tracing::debug!(account_id = %ctx.account_id, room_id = ?room_id, event_type = event_type.as_str(), "persisted account data");
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
    live_tx: &broadcast::Sender<LiveEvent>,
    manager: &ClientManager,
) -> Result<(), SyncError> {
    // The manager owns client construction + caching (and single-flight with the
    // gateway, which may have connected this account already). A connect failure
    // surfaces as a SyncError so the supervisor's backoff/retry is unchanged.
    let client = manager.get_or_connect(account.account_id).await?;

    // Transient key recovery (ADR 0011, ADR 0014): if a recovery key is
    // configured, import the megolm key backup + cross-signing keys once so this
    // fresh device can decrypt historical messages. The key is held only across
    // this call and never persisted. A wrong/rotated key is a readable error,
    // not a silent permanent UTD, and is non-fatal — sync still runs.
    if let Some(recovery_key) = recovery_key_for(config, account) {
        match client.encryption().recovery().recover(recovery_key).await {
            Ok(()) => tracing::info!(
                account_id = %account.account_id,
                "imported key backup via recovery key"
            ),
            Err(err) => tracing::error!(
                account_id = %account.account_id,
                error = %err,
                "recovery key import failed; historical messages may remain undecryptable"
            ),
        }
    }

    // Register event persistence before starting the sync service so no events
    // are missed between SyncService::start() and handler registration.
    let persist_ctx = PersistContext {
        store: store.clone(),
        account_id: account.account_id,
        live_tx: live_tx.clone(),
    };
    client.add_event_handler_context(persist_ctx);
    client.add_event_handler(persist_timeline_event);
    // Room state + account data (ADR 0016). These reuse the same PersistContext.
    // The global-account-data handler must not take a `Room` argument — it has no
    // room, and the SDK skips a handler whose `Room` extractor fails.
    client.add_event_handler(persist_room_state_event);
    client.add_event_handler(persist_room_account_data);
    client.add_event_handler(persist_global_account_data);

    // `SyncService::builder` consumes the client; keep a clone for the
    // re-decryption queue and the startup sweep (the client is Arc-backed, so
    // clones are cheap and share one underlying connection + crypto store).
    // Raise the room-list timeline window from the SDK default of 1 (latest
    // event only) so each room archives its last N events. See ADR 0015.
    let sync_service = SyncService::builder(client.clone())
        .with_room_list_timeline_limit(config.timeline_limit)
        .build()
        .await
        .map_err(sdk_err)?;
    sync_service.start().await;
    tracing::info!(account_id = %account.account_id, "sync service started");

    // Re-decryption queue: a child token so it ends with this run, and a join
    // handle so we drain it cleanly before returning (or restarting).
    let redecrypt_cancel = cancel.child_token();
    let redecrypt_handle = tokio::spawn(redecrypt::run(
        client.clone(),
        store.clone(),
        account.account_id,
        redecrypt_cancel.clone(),
    ));
    // One sweep now that the service is up and `recover()` (if any) has imported
    // keys: keys already in the crypto store don't fire the arrival stream.
    redecrypt::sweep_pending(&client, store, account.account_id).await;

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

    // Always drain the service so its SQLite store flushes before we drop it,
    // then stop and join the re-decryption queue so it doesn't outlive this run
    // (which would leak a task or duplicate one across a supervised restart).
    sync_service.stop().await;
    redecrypt_cancel.cancel();
    if let Err(err) = redecrypt_handle.await {
        tracing::warn!(
            account_id = %account.account_id,
            error = %err,
            "re-decryption task did not shut down cleanly"
        );
    }
    result
}

/// Resolve the transient recovery key for `account` from the configured
/// provision, if one is set and matches. Returns `None` when absent — recovery
/// is optional. The returned reference is consumed immediately by `recover()`
/// and never stored.
fn recovery_key_for<'c>(config: &'c SyncConfig, account: &Account) -> Option<&'c str> {
    config
        .account
        .as_ref()
        .filter(|p| matches_account(p, account))
        .and_then(|p| p.recovery_key.as_deref())
}
