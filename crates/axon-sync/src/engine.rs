//! The sync engine: one supervised task per account.
//!
//! [`SyncEngine::start`] provisions the configured account, then spawns a task
//! per account. Each task builds a [`Client`](matrix_sdk::Client), starts a
//! [`SyncService`] (Simplified Sliding Sync, MSC4186), and watches its state.
//! If the service errors or terminates unexpectedly the task restarts it with
//! exponential backoff; a cancellation token drives graceful shutdown.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axon_core::{LiveEvent, LiveFrame, SenderTrustFrame, SyncConfig};
use axon_search::IndexHandle;
use axon_store::{Account, AccountDataUpsert, EventCiphertext, NewEvent, RoomStateUpsert, Store};
use matrix_sdk::deserialized_responses::EncryptionInfo;
use matrix_sdk::event_handler::{Ctx, RawEvent};
use matrix_sdk::ruma::events::{
    AnyGlobalAccountDataEvent, AnyRoomAccountDataEvent, AnySyncStateEvent, AnySyncTimelineEvent,
};
use matrix_sdk::{Client, Room};
use matrix_sdk_ui::sync_service::{State, SyncService};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::client::matches_account;
use crate::error::{sdk_err, SyncError};
use crate::gateway::SdkGateway;
use crate::lifecycle::{lock_for, AccountLifecycle, IdentityLock, IdentityLocks};
use crate::manager::ClientManager;
use crate::redecrypt;
use crate::verification::{
    cancel_account_flows, new_registry, on_incoming_request, reap_expired_flows, FlowRegistry,
    VerificationEngine, VerificationListenerCtx,
};

/// Backoff bounds for restarting a failed per-account task.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// A supervised task's stop handles: the per-account cancellation token plus
/// the task's join handle. Both are needed because cancellation is cooperative —
/// `cancel` only *requests* the stop; the task then drains its sync service and
/// re-decryption queue (still holding open SQLite handles under the account's
/// store dir) before exiting. A lifecycle verb that must know the store dir is
/// quiescent (logout, ahead of a possible re-login restaging it) awaits `handle`.
pub(crate) struct AccountTask {
    pub(crate) cancel: CancellationToken,
    pub(crate) handle: tokio::task::JoinHandle<()>,
}

/// `account_id → stop handles` for the per-account supervised tasks.
///
/// Each task runs under a child of the engine-wide cancel token, registered here
/// when it is spawned. The engine-wide token still cascades to every child on
/// [`SyncEngine::shutdown`], but a single account can also be stopped on its own
/// (the logout/delete lifecycle verbs cancel just that account's token and await
/// its handle). Shared (clone of the `Arc`) between the engine and the
/// [`AccountLifecycle`] so both the boot loop and the runtime login verb register
/// onto the same map.
pub(crate) type TaskRegistry = Arc<Mutex<HashMap<Uuid, AccountTask>>>;

/// Owns the per-account sync tasks. Dropping it does not stop the tasks; call
/// [`SyncEngine::shutdown`] to cancel and join them cleanly.
pub struct SyncEngine {
    /// Tracks every supervised task — both the ones spawned at boot and the ones
    /// the lifecycle layer spawns at runtime on a successful login. `shutdown`
    /// closes and waits on it. A [`TaskTracker`] (vs. a `Vec<JoinHandle>`) is what
    /// lets tasks be added after `start` returns.
    tracker: TaskTracker,
    cancel: CancellationToken,
    /// Per-account cancellation handles, so a single account's task can be stopped
    /// without touching the others (the logout/delete verbs). Populated by
    /// [`spawn_supervised`] for both boot-time and runtime-login tasks.
    tasks: TaskRegistry,
    /// Producer end of the live-event bus. The sync tasks publish through clones
    /// of this; [`SyncEngine::live_events`] hands a clone to the API layer so
    /// each WebSocket connection can `subscribe()`.
    live_tx: broadcast::Sender<LiveFrame>,
    /// Per-account client lifecycle. Shared by the supervised sync tasks (which
    /// drive connects + retry) and the message gateway handed to the API layer
    /// (see [`SyncEngine::gateway`]).
    manager: ClientManager,
    /// Store + config handles retained so [`SyncEngine::lifecycle`] can build the
    /// runtime login port with everything a freshly logged-in account's
    /// supervised task needs.
    store: Store,
    config: SyncConfig,
    /// Per-identity lifecycle locks, owned here so every [`AccountLifecycle`] this
    /// engine hands out and every supervised task's verification watcher share the
    /// *same* lock per identity (ADR 0026).
    locks: IdentityLocks,
    /// In-memory registry of interactive SAS verification flows, shared between the
    /// [`VerificationEngine`] (the runtime port) and each account's supervised task
    /// (which listens for peer-initiated requests). Ephemeral — never persisted.
    verifications: FlowRegistry,
    /// Producer handle for the search-index actor (M9), or `None` when search is
    /// disabled. Cloned into every supervised task's persist + re-decryption paths
    /// so newly ingested events are indexed, and into the lifecycle port so a
    /// deleted account's documents are purged.
    index: Option<IndexHandle>,
}

impl SyncEngine {
    /// Provision the configured account (if any), then spawn one supervised sync
    /// task per account in the store. Returns once tasks are spawned; call
    /// [`SyncEngine::shutdown`] to stop them.
    pub async fn start(
        store: Store,
        config: SyncConfig,
        index: Option<IndexHandle>,
    ) -> Result<Self, SyncError> {
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

        // One tracker holds every task; the runtime login verb spawns onto the
        // same tracker so a logged-in-at-runtime account shuts down with the rest.
        // The task registry is shared the same way, so the lifecycle verbs can
        // cancel a single account's task. Built here (ahead of the spawn loop) so
        // the boot reconcile can drive the runtime delete verb over the same
        // machinery.
        let tracker = TaskTracker::new();
        let tasks: TaskRegistry = Arc::new(Mutex::new(HashMap::new()));
        // Owned here (not inside `AccountLifecycle`) so the reconcile-time port, the
        // API-time port, and the supervised watchers all share one lock per identity.
        let locks: IdentityLocks = Arc::new(Mutex::new(HashMap::new()));
        // Shared by the verification port and every account's incoming-request
        // listener. Ephemeral — lives only as long as the engine.
        let verifications = new_registry();

        // Crash recovery (ADR 0024), before any account is brought online and
        // before the HTTP listener binds (`axon-server` serves only after `start`
        // returns), so neither sweep races API traffic or a supervised task
        // creating a fresh store dir: finish any interrupted account deletion, then
        // prune row-less store dirs.
        let lifecycle = AccountLifecycle::new(
            store.clone(),
            config.clone(),
            manager.clone(),
            live_tx.clone(),
            cancel.clone(),
            tracker.clone(),
            tasks.clone(),
            locks.clone(),
            verifications.clone(),
            index.clone(),
        );
        crate::reconcile::reconcile_deleting(&lifecycle, &store).await;
        crate::reconcile::prune_orphan_store_dirs(&config, &store).await;

        // `list_accounts` returns only `active` rows, so a `deactivated` or
        // `deleting` account never gets a supervised task (ADR 0022). Listed after
        // the reconcile so a just-completed deletion is already gone.
        let accounts = store.list_accounts().await?;
        if accounts.is_empty() {
            tracing::warn!("no active accounts; sync engine idle");
        }

        for account in accounts {
            spawn_supervised(
                &tracker,
                &tasks,
                store.clone(),
                config.clone(),
                account,
                cancel.clone(),
                live_tx.clone(),
                manager.clone(),
                locks.clone(),
                verifications.clone(),
                index.clone(),
            );
        }

        // Background reaper for terminal verification flows, so their grace TTL is
        // honoured even on an account with no further verify API traffic to drive
        // the lazy sweep. Runs on the same tracker under a child of the engine
        // token, so `shutdown` cancels and joins it with everything else.
        tracker.spawn(reap_expired_flows(
            verifications.clone(),
            cancel.child_token(),
        ));

        Ok(SyncEngine {
            tracker,
            cancel,
            tasks,
            live_tx,
            manager,
            store,
            config,
            locks,
            verifications,
            index,
        })
    }

    /// A message gateway over the per-account clients, for the API layer's send
    /// path. `axon-server` wraps this in an adapter implementing its
    /// `MessageSender` port; the returned value is cheap to construct and clone.
    pub fn gateway(&self) -> SdkGateway {
        SdkGateway::new(self.manager.clone())
    }

    /// An authenticated media proxy over the per-account clients, for the API
    /// layer's `GET /v1/media/{account_id}/…` route. `axon-server` wraps this
    /// in an adapter implementing its `MediaProxy` port.
    pub fn media_proxy(&self) -> crate::media::SdkMediaProxy {
        crate::media::SdkMediaProxy::new(self.manager.clone())
    }

    /// A producer handle for the live-event bus. The API layer holds this in its
    /// router state and calls [`broadcast::Sender::subscribe`] once per
    /// `/v1/ws` connection. Cloning is cheap and does not affect delivery.
    pub fn live_events(&self) -> broadcast::Sender<LiveFrame> {
        self.live_tx.clone()
    }

    /// The runtime account-lifecycle port (login, …), for the API layer's
    /// lifecycle routes. `axon-server` wraps this in an adapter implementing its
    /// `AccountLifecycle` port; the returned value shares this engine's task
    /// tracker, so an account logged in at runtime is supervised and shut down
    /// alongside the boot-time ones.
    pub fn lifecycle(&self) -> AccountLifecycle {
        AccountLifecycle::new(
            self.store.clone(),
            self.config.clone(),
            self.manager.clone(),
            self.live_tx.clone(),
            self.cancel.clone(),
            self.tracker.clone(),
            self.tasks.clone(),
            self.locks.clone(),
            self.verifications.clone(),
            self.index.clone(),
        )
    }

    /// The runtime device-verification port, for the API layer's verify routes.
    /// `axon-server` wraps this in an adapter implementing its `VerificationService`
    /// port. Shares this engine's flow registry, task tracker, and cancel token, so
    /// verification driver tasks are supervised and shut down with the engine, and
    /// the flows it tracks are the same ones each account's incoming-request
    /// listener registers.
    pub fn verification(&self) -> VerificationEngine {
        VerificationEngine::new(
            self.store.clone(),
            self.manager.clone(),
            self.verifications.clone(),
            self.live_tx.clone(),
            self.tracker.clone(),
            self.cancel.clone(),
            self.locks.clone(),
        )
    }

    /// The runtime sender-trust port, for the API layer's verification-bundle
    /// route (M7c). `axon-server` wraps this in an adapter implementing its
    /// `SenderTrustService` port. Shares this engine's store and client manager;
    /// it's a read-only port and takes no lifecycle lock (see [`trust`]).
    pub fn sender_trust(&self) -> crate::trust::SenderTrustEngine {
        crate::trust::SenderTrustEngine::new(self.store.clone(), self.manager.clone())
    }

    /// Cancel all per-account tasks and wait for them to finish. Safe to call
    /// without canceling the token first — this method cancels it internally.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        // Close the tracker so `wait` can complete; no new tasks are spawned after
        // shutdown begins (the lifecycle port would spawn onto a closed tracker,
        // which is a no-op-and-error it already guards against).
        self.tracker.close();
        self.tracker.wait().await;
    }
}

/// Spawn a supervised sync task for `account` onto `tracker`. Shared by boot
/// (one call per active account) and the runtime login verb (one call when a
/// login succeeds), so both paths get identical supervision + shutdown.
///
/// The task runs under a fresh child of the engine-wide `cancel` token; its
/// token + join handle are registered in `tasks` under the account id so a
/// lifecycle verb can stop just this account *and await its drain*. A stale
/// entry for the id should not exist (logout awaits termination before the
/// identity can log back in), but if one does it is cancelled and replaced as a
/// backstop, so an account can never end up with two registered tasks.
// The supervised task genuinely needs every one of these handles; bundling them
// into a context struct would only move the same fields behind one more name.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_supervised(
    tracker: &TaskTracker,
    tasks: &TaskRegistry,
    store: Store,
    config: SyncConfig,
    account: Account,
    cancel: CancellationToken,
    live_tx: broadcast::Sender<LiveFrame>,
    manager: ClientManager,
    locks: IdentityLocks,
    verifications: FlowRegistry,
    index: Option<IndexHandle>,
) {
    let task_cancel = cancel.child_token();
    let account_id = account.account_id;
    let handle = tracker.spawn(supervise_account(
        store,
        config,
        account,
        task_cancel.clone(),
        live_tx,
        manager,
        locks,
        tracker.clone(),
        verifications,
        index,
    ));
    if let Some(stale) = tasks.lock().expect("task registry poisoned").insert(
        account_id,
        AccountTask {
            cancel: task_cancel,
            handle,
        },
    ) {
        stale.cancel.cancel();
    }
}

/// Supervise a single account: run it, and on failure restart with exponential
/// backoff until the cancellation token fires.
#[allow(clippy::too_many_arguments)]
async fn supervise_account(
    store: Store,
    config: SyncConfig,
    account: Account,
    cancel: CancellationToken,
    live_tx: broadcast::Sender<LiveFrame>,
    manager: ClientManager,
    locks: IdentityLocks,
    tracker: TaskTracker,
    verifications: FlowRegistry,
    index: Option<IndexHandle>,
) {
    let mut backoff = BACKOFF_START;

    loop {
        if cancel.is_cancelled() {
            return;
        }

        match run_account(
            &store,
            &config,
            &account,
            &cancel,
            &live_tx,
            &manager,
            &locks,
            &tracker,
            &verifications,
            index.as_ref(),
        )
        .await
        {
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
    live_tx: broadcast::Sender<LiveFrame>,
    /// Search-index producer (M9), or `None` when search is disabled. Each
    /// persisted event is enqueued for (re)indexing from the resolved projection.
    index: Option<IndexHandle>,
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

    // Poke the search indexer (M9). The durable indexing obligation — this
    // event's own document plus any relation/redaction *target* whose document it
    // changes — was already written to `search_outbox` transactionally by
    // `upsert_event`, so this is only a best-effort wakeup hint: a dropped notify
    // costs nothing because the next drain (a later notify, the periodic tick, or a
    // restart) still applies the on-disk obligation.
    if let Some(index) = ctx.index.as_ref() {
        index.notify();
    }

    // Sibling rows are best-effort: a failure here must not take down sync. Done
    // *before* the live emit so the frame can carry the verdict actually persisted
    // (the `COALESCE`d snapshot), not the freshly-derived one — they diverge on a
    // duplicate delivery whose trust changed, and a live subscriber must see the
    // same immutable snapshot a later timeline read returns (ADR 0031).
    let stored_trust =
        persist_event_siblings(&ctx, &event_id, &room_id, ciphertext, enc_info.as_ref()).await;

    // Fan the event out to any live `/v1/ws` subscribers. Skip the work entirely
    // when nobody is listening (the common case for a headless server) so we
    // don't clone the content needlessly. `send` errors only when there are no
    // receivers — harmless to ignore (a receiver may have dropped between the
    // count check and the send), and never fatal to sync.
    if ctx.live_tx.receiver_count() > 0 {
        // The effective stored verdict, so a live subscriber and a subsequent
        // timeline read agree. `None` for UTDs (no `enc_info` yet) and unencrypted
        // events.
        let _ = ctx.live_tx.send(LiveFrame::Timeline(LiveEvent {
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
            sender_trust: stored_trust,
        }));
    }
}

/// Write the crypto sibling rows for an event already persisted to `events`.
/// `ciphertext` is the `m.room.encrypted` content for UTDs (`None` otherwise);
/// `enc_info` is the SDK decryption info for decrypted events (`None` for UTDs).
///
/// Returns the **effective stored `sender_trust`** verdict (the `COALESCE`d value
/// the crypto-sibling upsert settled on), so the caller can emit a live frame
/// carrying the persisted snapshot rather than the freshly-derived verdict — they
/// diverge on a duplicate delivery whose trust changed (ADR 0031). `None` for a
/// UTD (no `enc_info`), an unencrypted event, or if the sibling write failed.
async fn persist_event_siblings(
    ctx: &PersistContext,
    event_id: &str,
    room_id: &str,
    ciphertext: Option<serde_json::Value>,
    enc_info: Option<&EncryptionInfo>,
) -> Option<String> {
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
        match ctx
            .store
            .upsert_event_crypto(&meta.as_event_crypto(ctx.account_id, event_id))
            .await
        {
            Ok(stored_trust) => return stored_trust,
            Err(err) => {
                tracing::warn!(account_id = %ctx.account_id, event_id, error = %err, "failed to persist crypto sibling");
            }
        }
    }
    None
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
#[allow(clippy::too_many_arguments)]
async fn run_account(
    store: &Store,
    config: &SyncConfig,
    account: &Account,
    cancel: &CancellationToken,
    live_tx: &broadcast::Sender<LiveFrame>,
    manager: &ClientManager,
    locks: &IdentityLocks,
    tracker: &TaskTracker,
    verifications: &FlowRegistry,
    index: Option<&IndexHandle>,
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
        index: index.cloned(),
    };
    client.add_event_handler_context(persist_ctx);
    client.add_event_handler(persist_timeline_event);
    // Room state + account data (ADR 0016). These reuse the same PersistContext.
    // The global-account-data handler must not take a `Room` argument — it has no
    // room, and the SDK skips a handler whose `Room` extractor fails.
    client.add_event_handler(persist_room_state_event);
    client.add_event_handler(persist_room_account_data);
    client.add_event_handler(persist_global_account_data);

    // Interactive SAS verification (M7a PR6): surface peer-initiated requests with
    // no HTTP kickoff. The handler registers the flow and spawns a driver under a
    // child of this account's cancel token, onto the engine tracker, so it shuts
    // down with the account. Outgoing flows (started via the API) are driven by the
    // VerificationEngine directly; both share `verifications`.
    client.add_event_handler_context(VerificationListenerCtx {
        account_id: account.account_id,
        registry: verifications.clone(),
        live_tx: live_tx.clone(),
        tracker: tracker.clone(),
        cancel: cancel.clone(),
    });
    client.add_event_handler(on_incoming_request);

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
        index.cloned(),
    ));
    // One sweep now that the service is up and `recover()` (if any) has imported
    // keys: keys already in the crypto store don't fire the arrival stream.
    redecrypt::sweep_pending_utds(&client, store, account.account_id, index).await;

    // Verification watcher (ADR 0026): keep the persisted `verified` flag tracking
    // the SDK's current cross-signing state. Same child-token + join-handle
    // lifecycle as the re-decryption queue, so it ends with this run and is drained
    // cleanly below rather than leaking across a supervised restart.
    let verify_cancel = cancel.child_token();
    // The watcher's `verified` write is serialized against the lifecycle verbs
    // (recover/logout) by taking this identity's lock — the *same* lock those verbs
    // hold — so a watcher derive can't clobber a concurrent recover's write (ADR
    // 0026, the lost-update race).
    let verify_lock = lock_for(locks, &account.user_id, &account.homeserver_url);
    let verify_handle = tokio::spawn(watch_verification(
        client.clone(),
        store.clone(),
        account.account_id,
        verify_lock,
        verify_cancel.clone(),
    ));

    // Sender-trust overlay watcher (M7c): push `sender_trust.violation` frames when
    // a sender's identity enters a verification violation. Same child-token +
    // join-handle lifecycle as the watchers above, so it ends with this run and is
    // drained cleanly below. Read-only (no persisted state), so no identity lock.
    let trust_cancel = cancel.child_token();
    let trust_handle = tokio::spawn(watch_sender_trust(
        client.clone(),
        account.account_id,
        live_tx.clone(),
        trust_cancel.clone(),
    ));

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
    verify_cancel.cancel();
    if let Err(err) = verify_handle.await {
        tracing::warn!(
            account_id = %account.account_id,
            error = %err,
            "verification watcher did not shut down cleanly"
        );
    }
    trust_cancel.cancel();
    if let Err(err) = trust_handle.await {
        tracing::warn!(
            account_id = %account.account_id,
            error = %err,
            "sender-trust watcher did not shut down cleanly"
        );
    }

    // Cancel any in-flight SAS verification drivers for this account: they hold the
    // SDK client this run is tearing down (or rebuilding), so they must not survive
    // it. Each cancelled driver best-effort cancels upstream and drops its registry
    // entry — binding a flow's lifetime to the *run*, not just the account or the
    // process. (Incoming drivers run under this account's token; API-started ones
    // under the engine token, so neither is reliably reached by token cascade on a
    // single-account stop — this sweep covers both.)
    cancel_account_flows(verifications, account.account_id);

    result
}

/// Keep the persisted `verified` flag tracking axon's own device verification
/// state (ADR 0026). The SDK's `verification_state()` is a `subscribe_reset`
/// subscriber, so the first poll yields the current value — persisting the
/// initial state (`false` for a fresh unverified device, `true` once
/// `recover`/`verify` has cross-signed it) — and each later change (cross-signing
/// rotated, the device's trust reset) re-derives and re-persists. That is what
/// makes the column track the SDK rather than being written once. Runs until
/// `cancel` fires or the subscription closes; a persist failure is logged and
/// skipped (best-effort, never fatal to the run).
async fn watch_verification(
    client: Client,
    store: Store,
    account_id: Uuid,
    lock: IdentityLock,
    cancel: CancellationToken,
) {
    let mut state = client.encryption().verification_state();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            next = state.next() => match next {
                Some(_) => {
                    // Derive AND persist under the per-identity lock — the same lock
                    // `recover`/`logout` hold (ADR 0026). Without it the two derive+
                    // persist steps could interleave with a concurrent `recover`: the
                    // watcher reads pre-import state (`false`), recover imports keys
                    // and writes `true`, then the watcher writes its stale `false` —
                    // a lost update with no guaranteed later emission to self-heal it.
                    // The shared helper holds the lock across the derive, so the
                    // observation always reflects post-recover state.
                    // Pass `cancel`: a lifecycle verb holds this lock while it drains
                    // (awaits) this very task, so the lock wait MUST be cancellation-
                    // aware or shutdown deadlocks and a detached watcher could later
                    // clobber the verb's reset `verified` (ADR 0026).
                    crate::lifecycle::lock_and_persist_verified(
                        &lock,
                        &client,
                        &store,
                        account_id,
                        &cancel,
                    )
                    .await;
                }
                None => return,
            },
        }
    }
}

/// Watch for sender identity changes and surface verification-violation
/// *transitions* as live `sender_trust.violation` overlay frames (M7c). The
/// per-event `sender_trust` snapshot the read API returns is immutable (what
/// Matrix's evidence said when the event arrived); a *current* identity can later
/// enter — or leave — a violation (the sender's cross-signing key changed). This
/// watcher pushes that fact so a client re-evaluates the affected sender — it
/// names the `user_id`, not per-event diffs; the verification bundle / timeline
/// re-read is the source of truth.
///
/// Subscribes to the SDK's `user_identities_stream` and tracks which senders it
/// has reported as in-violation, so it emits exactly on a *change*: a frame with
/// `verification_violation: true` when a sender enters a violation and one with
/// `false` when it clears — the latter is what lets a client un-badge from the
/// live stream alone. Runs until `cancel` fires or the subscription closes; never
/// persists anything (the snapshot is the durable record).
async fn watch_sender_trust(
    client: Client,
    account_id: Uuid,
    live_tx: broadcast::Sender<LiveFrame>,
    cancel: CancellationToken,
) {
    use futures_util::{pin_mut, StreamExt};
    use matrix_sdk::ruma::OwnedUserId;
    use std::collections::HashSet;

    let stream = match client.encryption().user_identities_stream().await {
        Ok(stream) => stream,
        Err(err) => {
            // Known limitation (GH issue #101): a subscribe failure disables the
            // overlay for the rest of this run with no retry. The bundle read
            // endpoint still reports current trust, so it's degraded, not blind.
            tracing::warn!(%account_id, error = %err, "could not subscribe to identity changes; sender-trust overlay disabled for this run");
            return;
        }
    };
    pin_mut!(stream);
    // Senders we've reported as in-violation, so we emit only on a transition (and
    // a clear emits a single `false` frame rather than going silent).
    let mut in_violation: HashSet<OwnedUserId> = HashSet::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            next = stream.next() => match next {
                Some(updates) => {
                    for identity in updates.new.values().chain(updates.changed.values()) {
                        let user_id = identity.user_id().to_owned();
                        let violation = identity.has_verification_violation();
                        // `insert`/`remove` return whether the set actually changed —
                        // our transition test, so an unrelated identity change for a
                        // sender whose violation state is unchanged emits nothing.
                        let changed = if violation {
                            in_violation.insert(user_id.clone())
                        } else {
                            in_violation.remove(&user_id)
                        };
                        if changed {
                            tracing::debug!(%account_id, %user_id, verification_violation = violation, "sender trust changed");
                            let _ = live_tx.send(LiveFrame::SenderTrustChanged(SenderTrustFrame {
                                account_id,
                                user_id: user_id.as_str().to_owned(),
                                verification_violation: violation,
                            }));
                        }
                    }
                }
                None => return,
            },
        }
    }
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
