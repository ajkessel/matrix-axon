//! Interactive SAS device verification (M7a PR6).
//!
//! Drives the matrix-rust-sdk SAS state machine for a logged-in account and
//! surfaces its progress as `verification.*` frames on the live-event bus. The
//! public surface is [`VerificationEngine`], the concrete backend the API's
//! verification port is adapted onto in `axon-server`.
//!
//! **Flows are ephemeral.** The in-flight flow — and the SDK's ephemeral SAS
//! keys — live only in the SDK's in-memory verification machine, which exposes no
//! way to serialize them, so a flow is re-readable across a *client* reconnect
//! (this process up) but never across a restart of this process. We mirror the
//! live SDK objects in an in-memory [`FlowRegistry`]; nothing is persisted. A
//! terminal flow's outcome is retained for [`TERMINAL_TTL`] so a reconnecting
//! client can still read whether it finished or was cancelled, then it is evicted
//! lazily. See ADR 0011 and the M7a PR6 plan for the full crash/reconnect matrix.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axon_core::{LiveFrame, VerificationFrame, VerificationFrameKind};
use axon_store::{Account, AccountState, Store};
use futures_util::{pin_mut, StreamExt};
use matrix_sdk::encryption::verification::{
    SasState, SasVerification, VerificationRequest, VerificationRequestState,
};
use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::{OwnedDeviceId, OwnedUserId};
use matrix_sdk::Client;
use tokio::sync::{broadcast, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::error::GatewayError;
use crate::lifecycle::{lock_for, IdentityLocks};
use crate::manager::ClientManager;

/// The only verification method axon implements: short-auth-string (emoji /
/// decimal). QR is explicitly deferred (the PRD / ADR 0011), so every request we
/// start and every incoming request we accept advertises **SAS only** — otherwise
/// matrix-sdk's default method set (with the `qrcode` feature on) would let a peer
/// pick QR, which this driver would then immediately cancel.
fn sas_only() -> Vec<VerificationMethod> {
    vec![VerificationMethod::SasV1]
}

/// How long a terminal (`Done`/`Cancelled`) flow's outcome is kept readable after
/// it finishes, so a client that missed the one-shot terminal frame can still
/// read the result via `GET …/verify/{flow_id}` before the entry is evicted.
const TERMINAL_TTL: Duration = Duration::from_secs(300);

/// The stage of a flow, axon-sync-side. The `axon-server` adapter maps this to
/// the API's `FlowStage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStage {
    /// A verification was requested; awaiting the peer.
    Requested,
    /// The peer is ready; SAS not yet computed.
    Ready,
    /// Keys exchanged — the SAS emoji/decimals are available to compare.
    KeysExchanged,
    /// This side has confirmed; awaiting the peer's MAC.
    Confirmed,
    /// Completed successfully; the device is now cross-signed.
    Done,
    /// Cancelled (timeout, mismatch, or either side cancelling).
    Cancelled,
}

/// A replayable snapshot of one flow, re-derived from the live SDK object (or a
/// recently-terminal flow's retained outcome). The `axon-server` adapter maps
/// this to the API's `FlowSummary`.
#[derive(Debug, Clone)]
pub struct FlowState {
    pub flow_id: String,
    pub target_device_id: String,
    pub stage: FlowStage,
    pub emoji: Option<Vec<(String, String)>>,
    pub decimals: Option<(u16, u16, u16)>,
    pub cancel_reason: Option<String>,
}

/// What can go wrong driving a verification flow, axon-sync-side. The
/// `axon-server` adapter collapses these onto the API's `VerifyError` (and thus
/// HTTP statuses).
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    /// No account exists for the given id.
    #[error("no such account: {0}")]
    NotFound(Uuid),
    /// No live or recently-terminal flow exists for the given id.
    #[error("no such flow: {0}")]
    FlowNotFound(String),
    /// The account is logged out (`deactivated`) — no live client to verify with.
    #[error("account is not active: {0}")]
    NotActive(Uuid),
    /// The account is mid-teardown (`deleting`).
    #[error("account is being deleted: {0}")]
    BeingDeleted(Uuid),
    /// The named device is not a known device of this account.
    #[error("unknown device: {0}")]
    UnknownDevice(String),
    /// The flow is not in a stage that permits the requested operation.
    #[error("flow not in a state for this operation: {0}")]
    WrongStage(String),
    /// The upstream homeserver rejected or failed a verification send, or the SDK
    /// failed in a way the caller can't fix.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// A store failure.
    #[error("store error: {0}")]
    Store(String),
}

/// The terminal outcome retained for [`TERMINAL_TTL`] after a flow finishes.
#[derive(Debug, Clone)]
enum TerminalOutcome {
    Done,
    Cancelled(Option<String>),
}

/// One tracked flow: the live SDK request, its SAS object once it exists, and —
/// once terminal — the retained outcome plus when it became terminal (for TTL
/// eviction). The live objects are what `get`/`list` re-derive state from.
pub(crate) struct FlowEntry {
    request: VerificationRequest,
    sas: Option<SasVerification>,
    target_device_id: String,
    terminal: Option<TerminalOutcome>,
    terminal_at: Option<Instant>,
    /// The token the flow's driver runs under. Held here so a sync-run teardown can
    /// cancel every in-flight flow of an account ([`cancel_account_flows`]) — a
    /// driver must never outlive the SDK client it was attached to (a supervised
    /// restart evicts and rebuilds that client).
    driver_cancel: CancellationToken,
}

/// `(account_id, flow_id) → flow`. Shared (clone of the `Arc`) between the
/// [`VerificationEngine`] and every per-account incoming-request listener.
pub(crate) type FlowRegistry = Arc<Mutex<HashMap<(Uuid, String), FlowEntry>>>;

/// A fresh, empty flow registry. Owned by [`SyncEngine`](crate::SyncEngine).
pub(crate) fn new_registry() -> FlowRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// How often the background reaper sweeps expired terminal flows, so the grace
/// TTL holds even with no verify API traffic to trigger a lazy sweep.
const REAP_INTERVAL: Duration = Duration::from_secs(60);

/// Drop terminal entries whose grace window has elapsed. Called both lazily from
/// the read/start verbs and from the background [`reap_expired_flows`] task, so an
/// idle account's terminal flows still get reclaimed.
fn sweep_expired(registry: &FlowRegistry) {
    let now = Instant::now();
    registry
        .lock()
        .expect("flow registry poisoned")
        .retain(|_, e| match e.terminal_at {
            Some(at) => now.duration_since(at) < TERMINAL_TTL,
            None => true,
        });
}

/// Remove a flow outright (not retained for the grace TTL). Used when a driver
/// exits without a terminal outcome — cancelled mid-flight (e.g. the account
/// logged out) or the SDK stream closed — so the entry can't linger forever with
/// `terminal_at = None`, which the TTL sweep would never reclaim.
fn remove_flow(registry: &FlowRegistry, key: &(Uuid, String)) {
    registry.lock().expect("flow registry poisoned").remove(key);
}

/// Cancel the driver of every non-terminal flow belonging to `account_id`.
/// Called from `engine::run_account` when a sync run tears down or is rebuilt, so
/// a verification driver never keeps running against an evicted SDK client. Each
/// cancelled driver best-effort cancels upstream and drops its registry entry. A
/// driver attached to the engine token would otherwise survive a supervised
/// restart; this is what binds the flow's lifetime to the *run*, not the process.
pub(crate) fn cancel_account_flows(registry: &FlowRegistry, account_id: Uuid) {
    let reg = registry.lock().expect("flow registry poisoned");
    for ((aid, _), entry) in reg.iter() {
        if *aid == account_id && entry.terminal.is_none() {
            entry.driver_cancel.cancel();
        }
    }
}

/// Background task: periodically reclaim terminal flows past their grace TTL.
/// Spawned once per engine; runs until `cancel` fires. Without it the TTL would
/// hold only while verify API calls keep arriving to drive the lazy sweep.
pub(crate) async fn reap_expired_flows(registry: FlowRegistry, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(REAP_INTERVAL) => sweep_expired(&registry),
        }
    }
}

/// Map a SAS emoji array into the wire-neutral `(symbol, description)` pairs.
fn emoji_pairs(sas: &SasVerification) -> Option<Vec<(String, String)>> {
    sas.emoji().map(|emojis| {
        emojis
            .iter()
            .map(|e| (e.symbol.to_owned(), e.description.to_owned()))
            .collect()
    })
}

/// Re-derive a flow's replayable state from its live SDK object, or its retained
/// terminal outcome.
fn snapshot(entry: &FlowEntry) -> FlowState {
    let flow_id = entry.request.flow_id().to_owned();
    let target_device_id = entry.target_device_id.clone();

    if let Some(outcome) = &entry.terminal {
        let (stage, cancel_reason) = match outcome {
            TerminalOutcome::Done => (FlowStage::Done, None),
            TerminalOutcome::Cancelled(reason) => (FlowStage::Cancelled, reason.clone()),
        };
        return FlowState {
            flow_id,
            target_device_id,
            stage,
            emoji: None,
            decimals: None,
            cancel_reason,
        };
    }

    if let Some(sas) = &entry.sas {
        let stage = match sas.state() {
            SasState::Created { .. } | SasState::Started { .. } | SasState::Accepted { .. } => {
                FlowStage::Ready
            }
            SasState::KeysExchanged { .. } => FlowStage::KeysExchanged,
            SasState::Confirmed => FlowStage::Confirmed,
            SasState::Done { .. } => FlowStage::Done,
            SasState::Cancelled(_) => FlowStage::Cancelled,
        };
        FlowState {
            flow_id,
            target_device_id,
            stage,
            emoji: emoji_pairs(sas),
            decimals: sas.decimals(),
            cancel_reason: sas.cancel_info().map(|c| c.reason().to_owned()),
        }
    } else {
        let stage = match entry.request.state() {
            VerificationRequestState::Created { .. }
            | VerificationRequestState::Requested { .. } => FlowStage::Requested,
            VerificationRequestState::Ready { .. }
            | VerificationRequestState::Transitioned { .. } => FlowStage::Ready,
            VerificationRequestState::Done => FlowStage::Done,
            VerificationRequestState::Cancelled(_) => FlowStage::Cancelled,
        };
        FlowState {
            flow_id,
            target_device_id,
            stage,
            emoji: None,
            decimals: None,
            cancel_reason: entry.request.cancel_info().map(|c| c.reason().to_owned()),
        }
    }
}

/// Publish a verification frame to the live-event bus, skipping the work when no
/// `/v1/ws` client is listening (mirrors the timeline publish path).
#[allow(clippy::too_many_arguments)]
fn publish(
    live_tx: &broadcast::Sender<LiveFrame>,
    account_id: Uuid,
    flow_id: &str,
    target_device_id: &str,
    kind: VerificationFrameKind,
    emoji: Option<Vec<(String, String)>>,
    decimals: Option<(u16, u16, u16)>,
    outcome: Option<String>,
) {
    if live_tx.receiver_count() == 0 {
        return;
    }
    let _ = live_tx.send(LiveFrame::Verification(VerificationFrame {
        account_id,
        flow_id: flow_id.to_owned(),
        kind,
        target_device_id: target_device_id.to_owned(),
        emoji,
        decimals,
        outcome,
    }));
}

/// A `WrongStage` error describing a flow that was cancelled — so `confirm` on a
/// cancelled flow is reported as a failure, never as a successful confirmation.
fn cancelled_err(reason: Option<&str>) -> VerifyError {
    match reason {
        Some(r) => VerifyError::WrongStage(format!("flow was cancelled: {r}")),
        None => VerifyError::WrongStage("flow was cancelled".to_owned()),
    }
}

/// Mark a flow terminal in the registry (so a reconnecting client reads the
/// outcome within the grace window) and stamp the eviction clock.
fn mark_terminal(registry: &FlowRegistry, key: &(Uuid, String), outcome: TerminalOutcome) {
    if let Some(entry) = registry
        .lock()
        .expect("flow registry poisoned")
        .get_mut(key)
    {
        entry.terminal = Some(outcome);
        entry.terminal_at = Some(Instant::now());
    }
}

/// Shared context for the per-account incoming-request listener (added to each
/// account's client as an event-handler context in `engine::run_account`).
#[derive(Clone)]
pub(crate) struct VerificationListenerCtx {
    pub(crate) account_id: Uuid,
    pub(crate) registry: FlowRegistry,
    pub(crate) live_tx: broadcast::Sender<LiveFrame>,
    pub(crate) tracker: TaskTracker,
    /// The account's cancellation token: drivers spawned for incoming requests run
    /// under a child of it, so they end when the account stops.
    pub(crate) cancel: CancellationToken,
}

/// Event handler for peer-initiated `m.key.verification.request` to-device events.
/// Registers the request and spawns a driver — there is no HTTP kickoff, so this
/// is the only place a peer-initiated flow surfaces (as a `verification.requested`
/// frame).
pub(crate) async fn on_incoming_request(
    ev: ToDeviceKeyVerificationRequestEvent,
    client: Client,
    ctx: Ctx<VerificationListenerCtx>,
) {
    let flow_id = ev.content.transaction_id.to_string();
    let Some(request) = client
        .encryption()
        .get_verification_request(&ev.sender, &flow_id)
        .await
    else {
        return;
    };

    // M7a only verifies axon's *own* device against another of the user's trusted
    // devices. Actively verifying another user's identity is explicitly out of
    // scope, so reject (cancel) anything that isn't a self-verification rather than
    // registering and accepting it. This also keeps the registry's `(account_id,
    // flow_id)` key unambiguous: only the user's own devices initiate, so the
    // transaction id can't collide with a different sender's.
    if !request.is_self_verification() {
        tracing::debug!(
            account_id = %ctx.account_id, %flow_id, sender = %ev.sender,
            "ignoring non-self verification request (out of scope)"
        );
        let _ = request.cancel().await;
        return;
    }

    let target_device_id = ev.content.from_device.to_string();
    let key = (ctx.account_id, flow_id);
    let driver_cancel = ctx.cancel.child_token();

    {
        let mut reg = ctx.registry.lock().expect("flow registry poisoned");
        // A duplicate request event (re-delivered on reconnect) must not spawn a
        // second driver for a flow we already track.
        if reg.contains_key(&key) {
            return;
        }
        reg.insert(
            key.clone(),
            FlowEntry {
                request: request.clone(),
                sas: None,
                target_device_id: target_device_id.clone(),
                terminal: None,
                terminal_at: None,
                driver_cancel: driver_cancel.clone(),
            },
        );
    }

    ctx.tracker.spawn(drive_request(
        request,
        ctx.account_id,
        target_device_id,
        ctx.registry.clone(),
        ctx.live_tx.clone(),
        driver_cancel,
    ));
}

/// Drive one verification request from its current state through to a
/// `SasVerification`, then hand off to [`drive_sas`]. Publishes the
/// `verification.requested` frame, accepts a peer-initiated request, and starts
/// SAS once ready for a request we initiated.
async fn drive_request(
    request: VerificationRequest,
    account_id: Uuid,
    target_device_id: String,
    registry: FlowRegistry,
    live_tx: broadcast::Sender<LiveFrame>,
    cancel: CancellationToken,
) {
    let flow_id = request.flow_id().to_owned();
    let key = (account_id, flow_id.clone());

    publish(
        &live_tx,
        account_id,
        &flow_id,
        &target_device_id,
        VerificationFrameKind::Requested,
        None,
        None,
        None,
    );

    // A peer-initiated request must be accepted to advance; for one we initiated
    // we wait for the peer to be ready instead. Accept advertising SAS only — never
    // the SDK default set (which includes QR) — so the peer can't steer the flow to
    // a method this driver doesn't implement.
    if !request.we_started() {
        if let Err(err) = request.accept_with_methods(sas_only()).await {
            tracing::warn!(%account_id, %flow_id, error = %err, "failed to accept verification request");
        }
    }

    let changes = request.changes();
    pin_mut!(changes);

    let sas = loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // Torn down mid-flight (typically the account logging out). Best-
                // effort cancel upstream, then drop the entry — it has no terminal
                // outcome to retain, and leaving it would leak (the TTL sweep only
                // reclaims entries with `terminal_at` set).
                let _ = request.cancel().await;
                remove_flow(&registry, &key);
                return;
            }
            next = changes.next() => match next {
                None => {
                    remove_flow(&registry, &key);
                    return;
                }
                Some(state) => match state {
                    VerificationRequestState::Created { .. }
                    | VerificationRequestState::Requested { .. } => {}
                    VerificationRequestState::Ready { .. } => {
                        // The initiating side starts SAS; the accepting side waits
                        // for the Transitioned below (avoids both starting at once).
                        if request.we_started() {
                            match request.start_sas().await {
                                Ok(Some(sas)) => break sas,
                                Ok(None) => {}
                                Err(err) => tracing::warn!(
                                    %account_id, %flow_id, error = %err,
                                    "failed to start SAS"
                                ),
                            }
                        }
                    }
                    VerificationRequestState::Transitioned { verification } => {
                        match verification.sas() {
                            Some(sas) => break sas,
                            // QR (or any non-SAS) isn't supported here — cancel.
                            None => {
                                let _ = request.cancel().await;
                            }
                        }
                    }
                    VerificationRequestState::Done => {
                        mark_terminal(&registry, &key, TerminalOutcome::Done);
                        publish(
                            &live_tx, account_id, &flow_id, &target_device_id,
                            VerificationFrameKind::Done, None, None, None,
                        );
                        return;
                    }
                    VerificationRequestState::Cancelled(info) => {
                        let reason = info.reason().to_owned();
                        mark_terminal(
                            &registry, &key,
                            TerminalOutcome::Cancelled(Some(reason.clone())),
                        );
                        publish(
                            &live_tx, account_id, &flow_id, &target_device_id,
                            VerificationFrameKind::Cancelled, None, None, Some(reason),
                        );
                        return;
                    }
                }
            }
        }
    };

    // Stash the SAS object so the read verbs and `confirm`/`cancel` can reach it,
    // then accept it (a no-op send for the side that started SAS).
    if let Some(entry) = registry
        .lock()
        .expect("flow registry poisoned")
        .get_mut(&key)
    {
        entry.sas = Some(sas.clone());
    }
    if let Err(err) = sas.accept().await {
        tracing::warn!(%account_id, %flow_id, error = %err, "failed to accept SAS");
    }

    drive_sas(
        sas,
        account_id,
        flow_id,
        target_device_id,
        registry,
        live_tx,
        cancel,
    )
    .await;
}

/// Drive a SAS verification to a terminal state, publishing the `sas`, `done`,
/// and `cancelled` frames and keeping the registry's terminal marker current.
async fn drive_sas(
    sas: SasVerification,
    account_id: Uuid,
    flow_id: String,
    target_device_id: String,
    registry: FlowRegistry,
    live_tx: broadcast::Sender<LiveFrame>,
    cancel: CancellationToken,
) {
    let key = (account_id, flow_id.clone());
    let changes = sas.changes();
    pin_mut!(changes);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                // See drive_request: drop the entry rather than leak a non-terminal
                // one the TTL sweep would never reclaim.
                let _ = sas.cancel().await;
                remove_flow(&registry, &key);
                return;
            }
            next = changes.next() => match next {
                None => {
                    remove_flow(&registry, &key);
                    return;
                }
                Some(state) => match state {
                    SasState::Created { .. }
                    | SasState::Started { .. }
                    | SasState::Accepted { .. }
                    | SasState::Confirmed => {}
                    SasState::KeysExchanged { .. } => {
                        publish(
                            &live_tx, account_id, &flow_id, &target_device_id,
                            VerificationFrameKind::Sas,
                            emoji_pairs(&sas), sas.decimals(), None,
                        );
                    }
                    SasState::Done { .. } => {
                        mark_terminal(&registry, &key, TerminalOutcome::Done);
                        publish(
                            &live_tx, account_id, &flow_id, &target_device_id,
                            VerificationFrameKind::Done, None, None, None,
                        );
                        return;
                    }
                    SasState::Cancelled(info) => {
                        let reason = info.reason().to_owned();
                        mark_terminal(
                            &registry, &key,
                            TerminalOutcome::Cancelled(Some(reason.clone())),
                        );
                        publish(
                            &live_tx, account_id, &flow_id, &target_device_id,
                            VerificationFrameKind::Cancelled, None, None, Some(reason),
                        );
                        return;
                    }
                }
            }
        }
    }
}

/// The concrete device-verification backend. Cheap to clone (all handles); built
/// by [`SyncEngine::verification`](crate::SyncEngine::verification) and adapted
/// onto the API's verification port by `axon-server`.
#[derive(Clone)]
pub struct VerificationEngine {
    store: Store,
    manager: ClientManager,
    registry: FlowRegistry,
    live_tx: broadcast::Sender<LiveFrame>,
    tracker: TaskTracker,
    cancel: CancellationToken,
    /// Per-identity lifecycle locks, shared with [`AccountLifecycle`]. `start`
    /// takes the same lock the login/logout/delete verbs take, so a flow can't be
    /// started against an account that a concurrent logout/delete is severing, and
    /// the login activation window (row `active` but the supervised task not yet
    /// registered) is closed — login holds the lock across both.
    locks: IdentityLocks,
}

impl VerificationEngine {
    pub(crate) fn new(
        store: Store,
        manager: ClientManager,
        registry: FlowRegistry,
        live_tx: broadcast::Sender<LiveFrame>,
        tracker: TaskTracker,
        cancel: CancellationToken,
        locks: IdentityLocks,
    ) -> Self {
        Self {
            store,
            manager,
            registry,
            live_tx,
            tracker,
            cancel,
            locks,
        }
    }

    /// Take the per-identity lifecycle lock for `account_id` and require the
    /// account still be `active` under it, returning the held guard for the caller
    /// to keep alive across its client operation.
    ///
    /// This is the serialization point for the verbs that drive the live SDK flow
    /// (`confirm`/`cancel`): login/logout/delete hold this *same* lock across their
    /// whole teardown (deactivate → sever session → cancel in-flight flows), so
    /// once this returns the account can't be severed until the guard drops — and a
    /// already-severed account is rejected here, never allowed to send another MAC
    /// or cancel after teardown has begun (which would continue a trust flow on a
    /// logged-out device whose `verified` flag was just reset).
    async fn lock_active(&self, account_id: Uuid) -> Result<OwnedMutexGuard<()>, VerifyError> {
        // Read once (unlocked) to resolve the identity the lock is keyed by.
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?
            .ok_or(VerifyError::NotFound(account_id))?;
        let lock = lock_for(&self.locks, &account.user_id, &account.homeserver_url);
        let guard = lock.lock_owned().await;
        // Re-read under the lock: a verb that ran while we waited may have changed
        // the account's lifecycle state.
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?
            .ok_or(VerifyError::NotFound(account_id))?;
        match account.state {
            AccountState::Active => Ok(guard),
            AccountState::Deactivated => Err(VerifyError::NotActive(account_id)),
            AccountState::Deleting => Err(VerifyError::BeingDeleted(account_id)),
        }
    }

    /// Gate an already-read account on `active` and return its live client and
    /// parsed user id. Called under the identity lock (see [`start`](Self::start))
    /// so the state it gates on can't change under it.
    async fn active_client(&self, account: &Account) -> Result<(Client, OwnedUserId), VerifyError> {
        match account.state {
            AccountState::Active => {}
            AccountState::Deactivated => return Err(VerifyError::NotActive(account.account_id)),
            AccountState::Deleting => return Err(VerifyError::BeingDeleted(account.account_id)),
        }
        let client = self
            .manager
            .get_or_connect(account.account_id)
            .await
            .map_err(|e| match e {
                GatewayError::UnknownAccount(id) => VerifyError::NotFound(id),
                GatewayError::AccountNotActive(id) => VerifyError::NotActive(id),
                other => VerifyError::Upstream(other.to_string()),
            })?;
        let user_id = account.user_id.parse::<OwnedUserId>().map_err(|_| {
            VerifyError::Upstream(format!("invalid stored user id: {}", account.user_id))
        })?;
        Ok((client, user_id))
    }

    /// Start a SAS verification of `account_id` against its trusted `device_id`,
    /// returning the new flow's id.
    pub async fn start(&self, account_id: Uuid, device_id: &str) -> Result<String, VerifyError> {
        sweep_expired(&self.registry);

        // Read once (unlocked) to resolve the identity the lock is keyed by — the
        // `(user_id, homeserver_url)` pair never changes for an account id.
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?
            .ok_or(VerifyError::NotFound(account_id))?;

        // Serialize against login/logout/delete on this identity by taking the
        // *same* lock those verbs hold, held across the whole critical section so a
        // concurrent teardown can't deactivate/evict the client under us, and so
        // the login activation window (row `active` but task not yet registered) is
        // closed — login holds this lock across both the activation and the task
        // spawn.
        let lock = lock_for(&self.locks, &account.user_id, &account.homeserver_url);
        let _guard = lock.lock().await;

        // Re-read under the lock: a verb that ran while we waited may have changed
        // the account's lifecycle state.
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?
            .ok_or(VerifyError::NotFound(account_id))?;
        let (client, user_id) = self.active_client(&account).await?;

        let device_id_owned: OwnedDeviceId = device_id.into();
        let device = client
            .encryption()
            .get_device(&user_id, &device_id_owned)
            .await
            .map_err(|e| VerifyError::Upstream(e.to_string()))?
            .ok_or_else(|| VerifyError::UnknownDevice(device_id.to_owned()))?;

        // SAS only — never the SDK default method set (which advertises QR).
        let request = device
            .request_verification_with_methods(sas_only())
            .await
            .map_err(|e| VerifyError::Upstream(e.to_string()))?;
        let flow_id = request.flow_id().to_owned();

        // The driver runs under a child of the engine token (so engine shutdown
        // cascades to it) and is also reachable for cancellation via the registry,
        // so a sync-run teardown for this account stops it (see
        // [`cancel_account_flows`]) — it never outlives the SDK client it drives.
        let driver_cancel = self.cancel.child_token();
        self.registry
            .lock()
            .expect("flow registry poisoned")
            .insert(
                (account_id, flow_id.clone()),
                FlowEntry {
                    request: request.clone(),
                    sas: None,
                    target_device_id: device_id.to_owned(),
                    terminal: None,
                    terminal_at: None,
                    driver_cancel: driver_cancel.clone(),
                },
            );

        self.tracker.spawn(drive_request(
            request,
            account_id,
            device_id.to_owned(),
            self.registry.clone(),
            self.live_tx.clone(),
            driver_cancel,
        ));

        Ok(flow_id)
    }

    /// List the account's currently-tracked flows (live + recently-terminal).
    pub async fn list(&self, account_id: Uuid) -> Result<Vec<FlowState>, VerifyError> {
        sweep_expired(&self.registry);
        if self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?
            .is_none()
        {
            return Err(VerifyError::NotFound(account_id));
        }
        let reg = self.registry.lock().expect("flow registry poisoned");
        Ok(reg
            .iter()
            .filter(|((aid, _), _)| *aid == account_id)
            .map(|(_, entry)| snapshot(entry))
            .collect())
    }

    /// Read one flow's replayable state.
    pub async fn get(&self, account_id: Uuid, flow_id: &str) -> Result<FlowState, VerifyError> {
        sweep_expired(&self.registry);
        // Gate on account existence (like `list`) so a flow that outlived its
        // account — e.g. a terminal entry still inside its grace TTL when the
        // account was deleted — reads as gone, not as stale state.
        if self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| VerifyError::Store(e.to_string()))?
            .is_none()
        {
            return Err(VerifyError::NotFound(account_id));
        }
        let reg = self.registry.lock().expect("flow registry poisoned");
        reg.get(&(account_id, flow_id.to_owned()))
            .map(snapshot)
            .ok_or_else(|| VerifyError::FlowNotFound(flow_id.to_owned()))
    }

    /// Confirm that the SAS matches. Requires the flow to have reached the
    /// key-exchanged stage; confirming an already-confirmed or already-done flow is
    /// an idempotent success, but confirming a cancelled flow is an error (it must
    /// never be reported as a successful confirmation).
    pub async fn confirm(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError> {
        // Serialize against login/logout/delete and reject a severed account before
        // touching the live SAS object: `confirm` sends this side's MAC, a
        // trust-advancing client op that must not run after teardown has begun.
        // Held across the whole verb, including `sas.confirm()` below.
        let _guard = self.lock_active(account_id).await?;
        let sas = {
            let reg = self.registry.lock().expect("flow registry poisoned");
            let entry = reg
                .get(&(account_id, flow_id.to_owned()))
                .ok_or_else(|| VerifyError::FlowNotFound(flow_id.to_owned()))?;
            match &entry.terminal {
                Some(TerminalOutcome::Done) => return Ok(()),
                Some(TerminalOutcome::Cancelled(reason)) => {
                    return Err(cancelled_err(reason.as_deref()))
                }
                None => {}
            }
            entry
                .sas
                .clone()
                .ok_or_else(|| VerifyError::WrongStage("SAS not yet started".to_owned()))?
        };

        // Presence of the SAS object does not mean it can be confirmed: the driver
        // stashes it before keys are exchanged, and matrix-rust-sdk treats
        // `confirm()` before the key exchange as a silent no-op — which would return
        // success with no MAC actually sent. Gate on the live SAS state.
        match sas.state() {
            SasState::Created { .. } | SasState::Started { .. } | SasState::Accepted { .. } => {
                return Err(VerifyError::WrongStage(
                    "SAS keys not yet exchanged; nothing to confirm".to_owned(),
                ));
            }
            // Already confirmed our side (awaiting the peer's MAC) or fully done —
            // a client retry. Idempotent success.
            SasState::Confirmed | SasState::Done { .. } => return Ok(()),
            SasState::Cancelled(info) => return Err(cancelled_err(Some(info.reason()))),
            SasState::KeysExchanged { .. } => {}
        }

        if let Err(err) = sas.confirm().await {
            // The flow may have raced to terminal between the lock release above and
            // this call: a done flow is idempotent success, a cancelled one is not.
            if sas.is_done() {
                return Ok(());
            }
            if sas.is_cancelled() {
                return Err(cancelled_err(None));
            }
            return Err(VerifyError::Upstream(err.to_string()));
        }
        Ok(())
    }

    /// Cancel the flow. Idempotent — cancelling a terminal flow is a no-op, and an
    /// already-terminal SDK error is swallowed.
    pub async fn cancel(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError> {
        // Same lifecycle serialization as `confirm`: `cancel` drives the live SDK
        // object (a to-device send), so it must not race a concurrent teardown
        // either; a severed account is rejected rather than reaching an evicted
        // client. Held across the cancel send below.
        let _guard = self.lock_active(account_id).await?;
        let (sas, request) = {
            let reg = self.registry.lock().expect("flow registry poisoned");
            let entry = reg
                .get(&(account_id, flow_id.to_owned()))
                .ok_or_else(|| VerifyError::FlowNotFound(flow_id.to_owned()))?;
            if entry.terminal.is_some() {
                return Ok(());
            }
            (entry.sas.clone(), entry.request.clone())
        };
        // Cancel via the SAS object if there is one, else the request. A send
        // failure is only safe to swallow when the flow is *already* terminal (it
        // raced to done/cancelled under us — cancel is idempotent); a genuine
        // homeserver/network failure must surface as `Upstream`, or the caller would
        // believe it cancelled while the peer keeps driving the exchange.
        match sas {
            Some(sas) => {
                if let Err(err) = sas.cancel().await {
                    if sas.is_done() || sas.is_cancelled() {
                        return Ok(());
                    }
                    return Err(VerifyError::Upstream(err.to_string()));
                }
            }
            None => {
                if let Err(err) = request.cancel().await {
                    if request.is_done() || request.is_cancelled() {
                        return Ok(());
                    }
                    return Err(VerifyError::Upstream(err.to_string()));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::ClientManager;
    use axon_core::SyncConfig;

    /// `cancelled_err` is a pure mapping — no DB needed. It must carry the reason
    /// when there is one and never read as a success.
    #[test]
    fn cancelled_err_carries_reason_and_is_wrong_stage() {
        match cancelled_err(Some("m.mismatched_sas")) {
            VerifyError::WrongStage(msg) => {
                assert!(msg.contains("m.mismatched_sas"), "reason missing: {msg}");
            }
            other => panic!("expected WrongStage, got {other:?}"),
        }
        match cancelled_err(None) {
            VerifyError::WrongStage(msg) => assert!(msg.contains("cancelled")),
            other => panic!("expected WrongStage, got {other:?}"),
        }
    }

    /// Build a verification engine over the test DB. The branches exercised here
    /// all return before any homeserver/SDK contact (account-state gating and
    /// registry lookups), so the manager/data_dir are never used.
    async fn engine() -> VerificationEngine {
        let url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
        let store = Store::connect(&url, 5).await.expect("connect + migrate");
        let config = SyncConfig {
            data_dir: std::env::temp_dir().join("axon-verify-test"),
            store_key: Some("test-key".to_owned()),
            account: None,
            timeline_limit: 1,
            live_event_buffer: 16,
            ..SyncConfig::default()
        };
        let manager = ClientManager::new(store.clone(), config.clone());
        let (live_tx, _rx) = broadcast::channel(16);
        VerificationEngine::new(
            store,
            manager,
            new_registry(),
            live_tx,
            TaskTracker::new(),
            CancellationToken::new(),
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    async fn delete_account(store: &Store, account_id: Uuid) {
        sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
            .bind(account_id)
            .execute(store.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn start_on_unknown_account_is_not_found() {
        let eng = engine().await;
        let err = eng.start(Uuid::new_v4(), "DEVICEID").await.unwrap_err();
        assert!(matches!(err, VerifyError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn start_on_deactivated_account_is_not_active() {
        let eng = engine().await;
        let user = format!("@deact-{}:localhost", Uuid::new_v4());
        let acct = eng
            .store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();
        eng.store
            .set_account_state(acct.account_id, AccountState::Deactivated)
            .await
            .unwrap();

        let err = eng.start(acct.account_id, "DEVICEID").await.unwrap_err();
        assert!(
            matches!(err, VerifyError::NotActive(id) if id == acct.account_id),
            "got {err:?}"
        );
        delete_account(&eng.store, acct.account_id).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn start_on_deleting_account_is_being_deleted() {
        let eng = engine().await;
        let user = format!("@del-{}:localhost", Uuid::new_v4());
        let acct = eng
            .store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();
        eng.store
            .set_account_state(acct.account_id, AccountState::Deleting)
            .await
            .unwrap();

        let err = eng.start(acct.account_id, "DEVICEID").await.unwrap_err();
        assert!(
            matches!(err, VerifyError::BeingDeleted(id) if id == acct.account_id),
            "got {err:?}"
        );
        delete_account(&eng.store, acct.account_id).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn get_on_unknown_account_is_not_found() {
        let eng = engine().await;
        let err = eng.get(Uuid::new_v4(), "flow").await.unwrap_err();
        assert!(matches!(err, VerifyError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn get_unknown_flow_on_known_account_is_flow_not_found() {
        let eng = engine().await;
        let user = format!("@getf-{}:localhost", Uuid::new_v4());
        let acct = eng
            .store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();

        let err = eng.get(acct.account_id, "no-such-flow").await.unwrap_err();
        assert!(matches!(err, VerifyError::FlowNotFound(_)), "got {err:?}");
        delete_account(&eng.store, acct.account_id).await;
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn list_on_unknown_account_is_not_found() {
        let eng = engine().await;
        let err = eng.list(Uuid::new_v4()).await.unwrap_err();
        assert!(matches!(err, VerifyError::NotFound(_)), "got {err:?}");
    }

    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn list_on_known_account_with_no_flows_is_empty() {
        let eng = engine().await;
        let user = format!("@listf-{}:localhost", Uuid::new_v4());
        let acct = eng
            .store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();

        let flows = eng.list(acct.account_id).await.unwrap();
        assert!(flows.is_empty());
        delete_account(&eng.store, acct.account_id).await;
    }

    /// `confirm`/`cancel` take the lifecycle lock first, so an unknown account is a
    /// `NotFound` before any registry lookup.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn confirm_and_cancel_on_unknown_account_are_not_found() {
        let eng = engine().await;
        let id = Uuid::new_v4();
        assert!(matches!(
            eng.confirm(id, "nope").await.unwrap_err(),
            VerifyError::NotFound(_)
        ));
        assert!(matches!(
            eng.cancel(id, "nope").await.unwrap_err(),
            VerifyError::NotFound(_)
        ));
    }

    /// On a known, active account the lifecycle gate passes and an unknown flow is
    /// a `FlowNotFound` — exercising the idempotent entry points without a live SAS
    /// object.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn confirm_and_cancel_unknown_flow_on_active_account_are_flow_not_found() {
        let eng = engine().await;
        let user = format!("@cf-{}:localhost", Uuid::new_v4());
        let acct = eng
            .store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();
        assert!(matches!(
            eng.confirm(acct.account_id, "nope").await.unwrap_err(),
            VerifyError::FlowNotFound(_)
        ));
        assert!(matches!(
            eng.cancel(acct.account_id, "nope").await.unwrap_err(),
            VerifyError::FlowNotFound(_)
        ));
        delete_account(&eng.store, acct.account_id).await;
    }

    /// `confirm`/`cancel` reject a severed (deactivated) account *before* touching
    /// the flow — the lifecycle-lock fix that stops a MAC/cancel send after a
    /// logout/delete has begun tearing the session down.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn confirm_and_cancel_on_deactivated_account_are_not_active() {
        let eng = engine().await;
        let user = format!("@cfd-{}:localhost", Uuid::new_v4());
        let acct = eng
            .store
            .upsert_account(&user, "https://hs.example.org")
            .await
            .unwrap();
        eng.store
            .set_account_state(acct.account_id, AccountState::Deactivated)
            .await
            .unwrap();
        assert!(matches!(
            eng.confirm(acct.account_id, "any").await.unwrap_err(),
            VerifyError::NotActive(id) if id == acct.account_id
        ));
        assert!(matches!(
            eng.cancel(acct.account_id, "any").await.unwrap_err(),
            VerifyError::NotActive(id) if id == acct.account_id
        ));
        delete_account(&eng.store, acct.account_id).await;
    }
}
