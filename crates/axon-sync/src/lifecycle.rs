//! Runtime account lifecycle: the verbs that add, reactivate, and (later) stop
//! or remove a Matrix account while axon is running.
//!
//! [`AccountLifecycle`] is the concrete capability `axon-server` adapts onto the
//! API layer's `AccountLifecycle` port (mirroring how [`SdkGateway`](crate::gateway)
//! backs `MessageSender`) — `axon-api` never sees this type or any SDK type. It
//! owns the *lifecycle* state transitions (ADR 0022); connection mechanics live in
//! the [`ClientManager`], task supervision in the [`engine`](crate::engine).
//!
//! Concurrency: lifecycle verbs for one account must not interleave (a login
//! racing a future logout could strand a half-built session), so each verb runs
//! under a per-identity async lock keyed by the canonical
//! `(user_id, homeserver_url)` — the natural key login starts from, before any
//! `account_id` exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axon_core::{LiveEvent, SyncConfig};
use axon_store::{Account, AccountState, Store, StoreError};
use matrix_sdk::ruma::OwnedUserId;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::engine::{spawn_supervised, AccountTask, TaskRegistry};
use crate::error::SyncError;
use crate::manager::ClientManager;

/// How long logout waits for a cancelled supervised task to finish draining
/// (sync-service stop + re-decryption join) before escalating to an abort
/// (see [`AccountLifecycle::reap_task`]). Generous — a healthy drain is
/// sub-second — so hitting it means the task is wedged.
#[cfg(not(test))]
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for an aborted task to actually terminate. An abort lands
/// at the task's next await point, so this expires only for a task stuck in
/// non-yielding code — the one case reaping can fail.
#[cfg(not(test))]
const ABORT_TIMEOUT: Duration = Duration::from_secs(5);

// Test builds shrink the reap timeouts so the escalation paths (cancel-ignoring
// task → abort; unabortable task → Draining refusal) run in milliseconds.
#[cfg(test)]
const DRAIN_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(test)]
const ABORT_TIMEOUT: Duration = Duration::from_millis(250);

/// Cap on the best-effort upstream `/logout` call, so the endpoint's response
/// time never depends on a stalled homeserver (the row is already deactivated
/// by the time this request is made).
const UPSTREAM_LOGOUT_TIMEOUT: Duration = Duration::from_secs(10);

/// What can go wrong running a lifecycle verb. Wire-neutral, like
/// [`GatewayError`](crate::GatewayError): the composition-root adapter
/// (`axon-server`) maps these onto the API layer's own login error so `axon-api`
/// never depends on this crate. Variants map cleanly to HTTP status: bad MXID →
/// 400, rejected credentials → 401, an account mid-teardown → 409, an
/// upstream/homeserver failure → 502, a store failure → 500.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// `username` is not a usable Matrix user ID: either syntactically invalid,
    /// or its domain is the homeserver's hostname rather than the server name
    /// its user IDs use (the message then suggests the canonical spelling).
    #[error("invalid matrix user id: {0}")]
    InvalidUserId(String),

    /// The account for this identity is mid-teardown (a transient `deleting`
    /// state), so a login can't be processed against it. Carries the account's id.
    /// (An *active* account is not an error — login is an idempotent no-op there.)
    #[error("account is being deleted: {0}")]
    BeingDeleted(Uuid),

    /// No account exists for the given id. Raised by the id-keyed verbs
    /// (logout/delete); login never returns it (it mints a row for a new
    /// identity). Carries the id that was looked up. → 404.
    #[error("no such account: {0}")]
    NotFound(Uuid),

    /// The account's previous supervised task has not terminated — it survived
    /// both cancellation and an abort (wedged in non-yielding code), so its SDK
    /// store dir cannot be treated as quiescent. Verbs that would touch or
    /// restage that dir are refused until a retry reaps the task. → 409.
    #[error("sync task for account {0} is still draining; retry shortly")]
    Draining(Uuid),

    /// The account is still named by `sync.account`, so a `DELETE` is refused: boot
    /// provisioning would `upsert` the identity straight back as a fresh active row,
    /// making the deletion non-durable. Remove it from config first. Carries the
    /// account's id. → 409. (A transitional guard — it becomes a no-op once
    /// config-based provisioning is retired; see ADR 0024.)
    #[error("account {0} is provisioned from config; remove it from sync.account before deleting")]
    Provisioned(Uuid),

    /// The homeserver rejected the supplied credentials.
    #[error("authentication failed: {0}")]
    AuthFailed(String),

    /// The login could not be completed for a transient reason (homeserver
    /// unreachable, a 5xx, a malformed response).
    #[error("upstream homeserver error: {0}")]
    Upstream(String),

    /// A storage-layer failure while resolving or transitioning the account row.
    #[error("store error: {0}")]
    Store(String),
}

impl From<StoreError> for LifecycleError {
    fn from(err: StoreError) -> Self {
        LifecycleError::Store(err.to_string())
    }
}

impl From<SyncError> for LifecycleError {
    /// Map a login failure onto the lifecycle error: a rejected credential stays
    /// an auth failure (→ 401); a store failure stays a store error; everything
    /// else (connection, SDK build, bad response) is an upstream failure (→ 502).
    fn from(err: SyncError) -> Self {
        match err {
            SyncError::AuthFailed(msg) => LifecycleError::AuthFailed(msg),
            SyncError::Store(e) => LifecycleError::Store(e.to_string()),
            other => LifecycleError::Upstream(other.to_string()),
        }
    }
}

/// Runtime account-lifecycle capability. Cheap to [`Clone`] — every field is a
/// handle — so the adapter can hold one and call it per request. Shares the sync
/// engine's task tracker, cancellation token, and live-event bus, so an account
/// logged in here is supervised and shut down exactly like a boot-time one.
#[derive(Clone)]
pub struct AccountLifecycle {
    store: Store,
    config: SyncConfig,
    manager: ClientManager,
    live_tx: broadcast::Sender<LiveEvent>,
    cancel: CancellationToken,
    tracker: TaskTracker,
    /// Per-account task cancellation handles, shared with the engine. Logout
    /// cancels (and removes) the entry for the account it stops.
    tasks: TaskRegistry,
    /// `canonical-identity → lock`. The std mutex is held only to fetch/insert a
    /// lock; the verb runs under the per-identity async mutex, so verbs for
    /// different accounts never block each other.
    locks: Arc<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    /// HTTP client for homeserver discovery (see [`discovery`](crate::discovery)).
    /// Cheap to clone (an `Arc` internally), shared across logins.
    http: matrix_sdk::reqwest::Client,
}

impl AccountLifecycle {
    /// Build the lifecycle port. Called by [`SyncEngine::lifecycle`](crate::SyncEngine::lifecycle).
    pub(crate) fn new(
        store: Store,
        config: SyncConfig,
        manager: ClientManager,
        live_tx: broadcast::Sender<LiveEvent>,
        cancel: CancellationToken,
        tracker: TaskTracker,
        tasks: TaskRegistry,
    ) -> Self {
        Self {
            store,
            config,
            manager,
            live_tx,
            cancel,
            tracker,
            tasks,
            locks: Arc::new(Mutex::new(HashMap::new())),
            http: crate::discovery::http_client(),
        }
    }

    /// The per-identity lock for `(user_id, homeserver_url)`, created on first use.
    fn lock_for(&self, user_id: &str, homeserver_url: &str) -> Arc<AsyncMutex<()>> {
        let key = format!("{user_id}\u{0}{homeserver_url}");
        // NOTE: this map grows unbounded — one entry per identity ever seen, never
        // removed. Pruning belongs to delete (which retires the identity for good),
        // not logout: a logged-out identity can be logged back in, and removing its
        // lock while a verb still holds it would let a concurrent login mint a fresh
        // lock and run without mutual exclusion. The leak is one small entry per
        // distinct identity, bounded by the accounts a user ever adds.
        let mut map = self.locks.lock().expect("lifecycle lock map poisoned");
        map.entry(key).or_default().clone()
    }

    /// Log a Matrix account in at runtime as a fresh device, returning its Axon
    /// `account_id`. Idempotent by canonical `(user_id, homeserver_url)`:
    ///
    /// - **New identity** → mint a row.
    /// - **`deactivated` row** → reuse its `account_id` (and its retained Postgres
    ///   archive), logging in as a fresh device with a fresh SDK crypto store.
    /// - **`active` row** → **idempotent no-op**: return the existing `account_id`
    ///   unchanged. The account is already logged in and supervised, so we do *not*
    ///   re-log-in (which would wipe its store out from under the running task).
    /// - **`deleting` row** → [`LifecycleError::BeingDeleted`] (409): a row
    ///   mid-teardown can't be logged into.
    ///
    /// For a new/deactivated row the row is held `deactivated` until the homeserver
    /// login succeeds, so a failed login never leaves a dangling `active` account
    /// and never deletes the row. On success it flips to `active` and a supervised
    /// sync task is spawned. `username` must be a full MXID; the password is
    /// consumed once, never stored (and not consulted at all for the active no-op).
    ///
    /// `homeserver_url` is optional: when absent it is resolved from the MXID's
    /// server name (see [`discovery`](crate::discovery)), so the canonical URL —
    /// not whatever each client guessed — keys the identity. A failed discovery
    /// is an upstream error and touches nothing. On both paths the MXID's domain
    /// is then checked against the homeserver's own declared server name
    /// (best-effort): an MXID written with the homeserver's hostname
    /// (`@adam:matrix.example.org` for `@adam:example.org`) is rejected with a
    /// did-you-mean error naming the canonical spelling, rather than failing as
    /// a misleading auth error — or, worse, being logged in as an identity other
    /// than the one typed.
    pub async fn login(
        &self,
        homeserver_url: Option<&str>,
        username: &str,
        password: &str,
    ) -> Result<Uuid, LifecycleError> {
        // Validate the MXID up front so identity resolves before we touch the DB
        // or build an SDK store.
        let user_id = OwnedUserId::try_from(username)
            .map_err(|e| LifecycleError::InvalidUserId(format!("{username}: {e}")))?;

        // Resolve the canonical homeserver before taking the identity lock — the
        // lock is keyed by `(user_id, homeserver_url)`, and discovery is a pure
        // read of external state.
        let homeserver_url = match homeserver_url {
            // Normalize + scheme-check the caller's URL (trailing slash trimmed
            // so it keys identically to a discovered one; plain-HTTP public hosts
            // refused so the password can't leave in cleartext). Not probed —
            // it's caller-asserted; a bad URL surfaces at the SDK login below.
            Some(url) => crate::discovery::accept_explicit_homeserver(user_id.server_name(), url)
                .map_err(|e| LifecycleError::Upstream(e.to_string()))?,
            None => crate::discovery::resolve_homeserver(&self.http, user_id.server_name())
                .await
                .map_err(|e| LifecycleError::Upstream(e.to_string()))?,
        };
        let homeserver_url = homeserver_url.as_str();

        // Refuse an MXID whose domain is actually the homeserver's hostname —
        // no such user can exist there, so fail now with the spelling they
        // meant instead of a misleading auth error. Also pre-lock: nothing has
        // been touched yet.
        crate::discovery::check_user_id_domain(&self.http, homeserver_url, &user_id)
            .await
            .map_err(|e| LifecycleError::InvalidUserId(e.to_string()))?;

        let lock = self.lock_for(username, homeserver_url);
        let _guard = lock.lock().await;

        // Resolve the target row: no-op on an already-active one, reactivate a
        // deactivated one, reject one mid-deletion, or mint a new one held
        // `deactivated` until login succeeds.
        let account = match self
            .store
            .find_account_by_identity(username, homeserver_url)
            .await?
        {
            Some(existing) => match existing.state {
                // Already logged in and supervised: idempotent no-op. Return the
                // existing account untouched rather than re-logging-in (which would
                // wipe the store under the running task) — the desired end state is
                // already in place. The password is not consulted.
                AccountState::Active => return Ok(existing.account_id),
                // A row mid-teardown can't be logged into (409).
                AccountState::Deleting => {
                    return Err(LifecycleError::BeingDeleted(existing.account_id));
                }
                // Reactivation restages the account's SDK store dir, so it must
                // not proceed while a previous supervised task still holds it. A
                // registry entry for a deactivated row exists only when a logout
                // failed to reap the task (`reap_task` re-registers a wedged one);
                // refuse, checked before any store-dir or homeserver work. A
                // logout retry is what reaps the leftover and clears this.
                AccountState::Deactivated => {
                    if self
                        .tasks
                        .lock()
                        .expect("task registry poisoned")
                        .contains_key(&existing.account_id)
                    {
                        return Err(LifecycleError::Draining(existing.account_id));
                    }
                    existing
                }
            },
            None => {
                let minted = self.store.upsert_account(username, homeserver_url).await?;
                // A freshly inserted row defaults to `active`; hold it
                // `deactivated` so a login failure below leaves no live account.
                //
                // NOTE: these two calls are not atomic. A crash between the insert
                // and this flip leaves an orphaned `active` row with no stored
                // session and no running task; the boot loop then retries it
                // indefinitely (the login can't be replayed). The boot reconcile /
                // orphan GC is what retires such rows.
                self.store
                    .set_account_state(minted.account_id, AccountState::Deactivated)
                    .await?;
                minted
            }
        };

        // Log in as a fresh device; the manager caches the live client in the
        // account's slot so the supervised task reuses it (ADR 0021). A failure
        // leaves the row `deactivated`.
        self.manager.login(&account, password).await?;

        // The slot now holds a live client for a row that is still `deactivated`.
        // Activate it and re-read so the supervised task and the returned id see
        // the `active` row. If activation fails, evict that cached client before
        // returning: no supervised task will consume it, and leaving it cached
        // would let a later send reach a live client behind the active-state gate
        // on an account that never became active.
        let active = match self.activate(account.account_id).await {
            Ok(active) => active,
            Err(err) => {
                self.manager.evict(account.account_id);
                return Err(err);
            }
        };
        let account_id = active.account_id;
        spawn_supervised(
            &self.tracker,
            &self.tasks,
            self.store.clone(),
            self.config.clone(),
            active,
            self.cancel.clone(),
            self.live_tx.clone(),
            self.manager.clone(),
        );
        tracing::info!(%account_id, user_id = %username, "account logged in and supervised");
        Ok(account_id)
    }

    /// Flip a freshly-logged-in account to `active` and re-read the row. Split out
    /// of [`login`](Self::login) so the caller can evict the login's cached client
    /// if any step here fails — otherwise a failed activation would strand a usable
    /// client on a non-`active` account.
    async fn activate(&self, account_id: Uuid) -> Result<Account, LifecycleError> {
        self.store
            .set_account_state(account_id, AccountState::Active)
            .await?;
        self.store.get_account(account_id).await?.ok_or_else(|| {
            LifecycleError::Store(format!("account {account_id} vanished after login"))
        })
    }

    /// Log a Matrix account out at runtime: move the row to `deactivated`, stop
    /// its supervised sync task **and await its drain**, then invalidate its
    /// device token upstream (best-effort, capped). All of axon's data is
    /// **retained** (the Postgres archive and the on-disk SDK store), so a later
    /// [`login`](Self::login) reactivates the same `account_id` as a fresh
    /// device. On `Ok` the account's task has *terminated* and its store dir is
    /// quiescent, so an immediate re-login is safe; if the task cannot be made
    /// to terminate (survives cancel **and** abort — see
    /// [`reap_task`](Self::reap_task)) this fails with
    /// [`LifecycleError::Draining`] instead, the task stays registered, and
    /// [`login`](Self::login) refuses the identity until a logout retry reaps
    /// it — the postcondition is never traded away for a return. Keyed by
    /// `account_id`:
    ///
    /// - **`active` row** → stop + deactivate.
    /// - **`deactivated` row** → idempotent re-run of the severing (a no-op when
    ///   the row was cleanly logged out; finishes the job after a logout that
    ///   failed midway).
    /// - **`deleting` row** → [`LifecycleError::BeingDeleted`] (409): a delete is in
    ///   flight; don't interfere.
    /// - **no such row** → [`LifecycleError::NotFound`] (404).
    pub async fn logout(&self, account_id: Uuid) -> Result<(), LifecycleError> {
        // Resolve identity so we can take the per-identity lock (keyed by
        // `(user_id, homeserver_url)`, the key space login uses). A 404 is cheap
        // and needs no lock.
        let account = self
            .store
            .get_account(account_id)
            .await?
            .ok_or(LifecycleError::NotFound(account_id))?;

        let lock = self.lock_for(&account.user_id, &account.homeserver_url);
        let _guard = lock.lock().await;

        // Re-read under the lock: the state may have moved between the unlocked
        // resolve above and acquiring the lock.
        let account = self
            .store
            .get_account(account_id)
            .await?
            .ok_or(LifecycleError::NotFound(account_id))?;
        match account.state {
            // Mid-teardown: a delete is in flight (409).
            AccountState::Deleting => return Err(LifecycleError::BeingDeleted(account_id)),
            // Deactivate FIRST. `get_or_connect`'s cold-connect gate refuses a
            // non-`active` row, so once this write lands no *new* client can be
            // built for the account — without it, a send racing the steps below
            // could cold-connect into the just-emptied slot while the row still
            // reads `active`, and the cached client it leaves behind would
            // outlive the deactivation (the gate doesn't re-check state on a
            // cache hit).
            AccountState::Active => {
                self.store
                    .set_account_state(account_id, AccountState::Deactivated)
                    .await?;
            }
            // Already logged out — but fall through to the severing below rather
            // than returning. On a cleanly logged-out row it's all no-ops; after
            // a logout that failed midway (a wedged task, a 500 between the state
            // flip and the eviction) it's what lets a retry finish the job.
            AccountState::Deactivated => {}
        }

        // Sever the live session: reap the supervised task (awaiting its drain)
        // and take + upstream-invalidate the cached client. Shared with `delete`.
        // A wedged task surfaces as `Draining` and leaves the row `deactivated` for
        // a retry to reap.
        self.sever_session(account_id).await?;

        tracing::info!(%account_id, user_id = %account.user_id, "account logged out");
        Ok(())
    }

    /// Stop an account's live session: reap its supervised task **awaiting its
    /// drain** (cooperative cancel → abort escalation; a task that survives both
    /// is left registered and surfaces as [`LifecycleError::Draining`]), then take
    /// its cached client out of the connection slot and best-effort, time-capped,
    /// invalidate the device token upstream. Shared by [`logout`](Self::logout)
    /// and [`delete`](Self::delete) — the "sever the running session" tail both
    /// need.
    ///
    /// Preconditions (the caller's job): the row has already been flipped out of
    /// `active` (so the cold-connect gate refuses any *new* client — this is the
    /// flip-before-take that closes the reconnect race), and the per-identity lock
    /// is held. `take` awaits the slot lock, so a connect that read `active` before
    /// the flip and cached a client has it taken right back out here.
    ///
    /// Returns the reap result: on `Draining` the caller must **not** proceed to
    /// remove or restage anything the still-live task may hold — its store dir is
    /// not quiescent. The upstream call never fails the verb: an unreachable or
    /// stalled homeserver must not stall it (the local state is already changed),
    /// so the device merely lingers upstream until reachable.
    async fn sever_session(&self, account_id: Uuid) -> Result<(), LifecycleError> {
        self.reap_task(account_id).await?;

        if let Some(client) = self.manager.take(account_id).await {
            match tokio::time::timeout(UPSTREAM_LOGOUT_TIMEOUT, client.matrix_auth().logout()).await
            {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(
                    %account_id,
                    error = %err,
                    "upstream logout failed; session severed locally"
                ),
                Err(_) => tracing::warn!(
                    %account_id,
                    timeout_secs = UPSTREAM_LOGOUT_TIMEOUT.as_secs(),
                    "upstream logout timed out; session severed locally"
                ),
            }
        }
        Ok(())
    }

    /// Permanently delete a Matrix account and every trace of it — an **ordered,
    /// idempotent, crash-recoverable** teardown (ADR 0024). Unlike
    /// [`logout`](Self::logout), which is a reversible pause that *retains* all
    /// data, this is a hard removal: the row, its Postgres archive (via FK
    /// cascade), and its on-disk SDK store are gone, and re-adding the same Matrix
    /// account later is a fresh [`login`](Self::login) with a new `account_id`.
    ///
    /// The order is load-bearing — the row is the only durable key a boot reconcile
    /// can re-find the external resources from, so it is deleted **last**:
    /// 1. flip the row to `deleting` (a durable "external cleanup owed" marker;
    ///    also moves it out of `active` so the cold-connect gate refuses any new
    ///    client *before* the cached one is taken — flip-before-take);
    /// 2. [`sever_session`](Self::sever_session) the live session;
    /// 3. remove the on-disk SDK store dir (and its staging backup);
    /// 4. delete the row (FK cascade drops events/account_data/room_state).
    ///
    /// Then the identity's lock-map entry is pruned (it is retired for good). Two
    /// steps the spec orders in here — search-index doc deletion and media-cache
    /// purge — are deferred until those subsystems exist; see ADR 0024.
    ///
    /// Idempotent and resumable, keyed by id:
    /// - **`active` / `deactivated` row** → full teardown.
    /// - **`deleting` row** → resume it (a crash or earlier failure left it
    ///   mid-flight); every step is idempotent. This is the branch the boot
    ///   reconcile and a client retry hit.
    /// - **no such row** → [`LifecycleError::NotFound`] (404): already gone. A
    ///   second concurrent delete observes this once the first completes.
    ///
    /// If the supervised task cannot be made to terminate (survives cancel **and**
    /// abort — see [`reap_task`](Self::reap_task)), this fails with
    /// [`LifecycleError::Draining`] **before** the store dir is touched, leaving the
    /// row `deleting` for a retry — a live task's store dir is never removed out
    /// from under it.
    pub async fn delete(&self, account_id: Uuid) -> Result<(), LifecycleError> {
        // Resolve identity to take the per-identity lock; a 404 needs no lock.
        let account = self
            .store
            .get_account(account_id)
            .await?
            .ok_or(LifecycleError::NotFound(account_id))?;

        let lock = self.lock_for(&account.user_id, &account.homeserver_url);
        let _guard = lock.lock().await;

        // Re-read under the lock: a concurrent verb may have moved or removed the
        // row between the unlocked resolve and acquiring the lock.
        let account = self
            .store
            .get_account(account_id)
            .await?
            .ok_or(LifecycleError::NotFound(account_id))?;

        // Refuse to *initiate* a delete on an account `sync.account` still names:
        // boot provisioning `upsert`s it straight back as a fresh active row, so the
        // deletion wouldn't survive a restart. The fix is to remove it from config
        // first (or retire config provisioning entirely — ADR 0024). A row already
        // `deleting` is exempt: that delete is in flight (and is what the boot
        // reconcile resumes), so it must be allowed to finish rather than wedge.
        if account.state != AccountState::Deleting {
            if let Some(provision) = &self.config.account {
                if crate::client::matches_account(provision, &account) {
                    return Err(LifecycleError::Provisioned(account_id));
                }
            }
        }

        // Flip to `deleting` first (unless already there — a resume). Durably marks
        // that external cleanup is owed, and — like logout's flip — moves the row
        // out of `active` so `get_or_connect`'s cold-connect gate refuses any new
        // client before `sever_session` takes the cached one.
        if account.state != AccountState::Deleting {
            self.store
                .set_account_state(account_id, AccountState::Deleting)
                .await?;
        }

        // Sever the live session. A wedged task returns `Draining` and we stop here
        // — the row stays `deleting`, nothing on disk is touched, and a retry (or
        // the boot reconcile) finishes once the task finally dies. This is why the
        // store-dir removal below sits *after* a successful sever.
        self.sever_session(account_id).await?;

        // External resources before the row (the row is the reconcile's only handle
        // to them): the SDK store dir + its staging backup. Idempotent on a resume.
        crate::client::remove_account_store_dirs(&self.config, account_id).await?;

        // Only now drop the row — FK cascades remove events/account_data/room_state.
        self.store.delete_account_row(account_id).await?;

        // The identity is retired for good, so prune its lock-map entry — but only
        // if no other verb is parked on it (see `prune_lock`).
        self.prune_lock(&account.user_id, &account.homeserver_url, &lock);

        tracing::info!(%account_id, user_id = %account.user_id, "account deleted");
        Ok(())
    }

    /// Prune the per-identity lock-map entry for a retired (deleted) identity —
    /// but only when no other verb still holds the lock `Arc`. ADR 0024: pruning
    /// while a verb is *parked* on the same `Arc` would let a later
    /// [`lock_for`](Self::lock_for) mint a *fresh* lock for the identity and run
    /// without mutual exclusion against that waiter. Performed under the std map
    /// mutex so no new waiter can clone the `Arc` between the count check and the
    /// removal.
    fn prune_lock(&self, user_id: &str, homeserver_url: &str, lock: &Arc<AsyncMutex<()>>) {
        let key = format!("{user_id}\u{0}{homeserver_url}");
        let mut map = self.locks.lock().expect("lifecycle lock map poisoned");
        // Live strong refs: the map entry (1) + our own `lock` handle (1) + one per
        // parked waiter. `== 2` ⇒ only map + us, so removing the entry can't strand
        // a waiter on an orphaned lock. `> 2` ⇒ leave it (a tiny bounded leak,
        // reclaimed by the next delete of this identity — correctness over a slot).
        if Arc::strong_count(lock) == 2 {
            map.remove(&key);
        }
    }

    /// Stop the account's supervised task and wait until it has actually
    /// terminated, so on `Ok` the caller may treat the account's SDK store dir
    /// as quiescent (a no-op if no task is registered). Cancellation is
    /// cooperative, so this escalates: cancel → await ([`DRAIN_TIMEOUT`]);
    /// on timeout abort → await ([`ABORT_TIMEOUT`], aborts land at the task's
    /// next await point). A task that survives even the abort — wedged in
    /// non-yielding code — is re-registered and the verb fails with
    /// [`LifecycleError::Draining`]: never "proceed with the task alive", which
    /// would let a re-login restage the store dir out from under it. The
    /// retained entry is what makes [`login`](Self::login) refuse the identity
    /// and a logout retry try the reap again. A join error (panic or abort)
    /// still means the task is gone, which is all the caller needs.
    async fn reap_task(&self, account_id: Uuid) -> Result<(), LifecycleError> {
        // The map guard is dropped before any await.
        let task = self
            .tasks
            .lock()
            .expect("task registry poisoned")
            .remove(&account_id);
        let Some(AccountTask { cancel, mut handle }) = task else {
            return Ok(());
        };

        cancel.cancel();
        match tokio::time::timeout(DRAIN_TIMEOUT, &mut handle).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(err)) => {
                tracing::warn!(
                    %account_id,
                    error = %err,
                    "supervised task panicked during logout drain"
                );
                return Ok(());
            }
            Err(_) => tracing::warn!(
                %account_id,
                timeout_secs = DRAIN_TIMEOUT.as_secs(),
                "supervised task did not finish draining within the timeout; aborting it"
            ),
        }

        handle.abort();
        match tokio::time::timeout(ABORT_TIMEOUT, &mut handle).await {
            // Finished or cancelled — terminated either way.
            Ok(_) => Ok(()),
            Err(_) => {
                tracing::error!(
                    %account_id,
                    timeout_secs = ABORT_TIMEOUT.as_secs(),
                    "supervised task survived abort (wedged in non-yielding code); \
                     its store dir cannot be treated as free"
                );
                self.tasks
                    .lock()
                    .expect("task registry poisoned")
                    .insert(account_id, AccountTask { cancel, handle });
                Err(LifecycleError::Draining(account_id))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a lifecycle over the test DB. The branches exercised here all return
    /// before any homeserver/SDK contact, so the manager/data_dir are never used.
    async fn lifecycle() -> AccountLifecycle {
        let url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
        let store = Store::connect(&url, 5).await.expect("connect + migrate");
        let config = SyncConfig {
            data_dir: std::env::temp_dir().join("axon-lifecycle-test"),
            store_key: Some("test-key".to_owned()),
            account: None,
            timeline_limit: 1,
            live_event_buffer: 16,
        };
        let manager = ClientManager::new(store.clone(), config.clone());
        let (live_tx, _rx) = broadcast::channel(16);
        AccountLifecycle::new(
            store,
            config,
            manager,
            live_tx,
            CancellationToken::new(),
            TaskTracker::new(),
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    /// A lifecycle whose `sync.account` names `(user_id, homeserver_url)`, for the
    /// configured-account delete guard. The credential is unused (these paths return
    /// before any login), but `matches_account` keys on the identity fields.
    async fn lifecycle_with_provision(user_id: &str, homeserver_url: &str) -> AccountLifecycle {
        let url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
        let store = Store::connect(&url, 5).await.expect("connect + migrate");
        let config = SyncConfig {
            data_dir: std::env::temp_dir().join("axon-lifecycle-test"),
            store_key: Some("test-key".to_owned()),
            account: Some(axon_core::AccountProvision {
                user_id: user_id.to_owned(),
                homeserver_url: homeserver_url.to_owned(),
                password: Some("unused".to_owned()),
                access_token: None,
                device_id: None,
                recovery_key: None,
            }),
            timeline_limit: 1,
            live_event_buffer: 16,
        };
        let manager = ClientManager::new(store.clone(), config.clone());
        let (live_tx, _rx) = broadcast::channel(16);
        AccountLifecycle::new(
            store,
            config,
            manager,
            live_tx,
            CancellationToken::new(),
            TaskTracker::new(),
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    async fn delete_account(store: &Store, account_id: Uuid) {
        sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
            .bind(account_id)
            .execute(store.pool())
            .await
            .expect("cleanup");
    }

    /// Login on an already-`active` account is an idempotent no-op: it returns the
    /// existing id, doesn't consult the password, and changes nothing.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn login_on_active_account_is_idempotent_noop() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@noop-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active by default

        // Deliberately wrong password — an active account never consults it.
        let id = lc
            .login(Some(hs), &user, "not-the-password")
            .await
            .expect("active login is a no-op");
        assert_eq!(id, acct.account_id);

        // Untouched: still active, same row.
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Active);

        delete_account(&lc.store, acct.account_id).await;
    }

    /// Login on a `deleting` row is a conflict (→ 409), not a no-op.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn login_on_deleting_account_conflicts() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@del-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        lc.store
            .set_account_state(acct.account_id, AccountState::Deleting)
            .await
            .unwrap();

        let err = lc.login(Some(hs), &user, "pw").await.unwrap_err();
        assert!(matches!(err, LifecycleError::BeingDeleted(id) if id == acct.account_id));

        delete_account(&lc.store, acct.account_id).await;
    }

    /// A username that isn't a valid full MXID is rejected (→ 400) before any
    /// store/identity work.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn login_with_invalid_mxid_is_rejected() {
        let lc = lifecycle().await;
        let err = lc
            .login(Some("https://hs.example.org"), "not-an-mxid", "pw")
            .await
            .unwrap_err();
        assert!(matches!(err, LifecycleError::InvalidUserId(_)));
    }

    /// Logout on an already-`deactivated` account is an idempotent no-op: it
    /// succeeds and leaves the row `deactivated`.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_on_deactivated_account_is_idempotent_noop() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-noop-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        lc.store
            .set_account_state(acct.account_id, AccountState::Deactivated)
            .await
            .unwrap();

        lc.logout(acct.account_id)
            .await
            .expect("logout on a deactivated account is a no-op");

        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deactivated);

        delete_account(&lc.store, acct.account_id).await;
    }

    /// Logout on a `deleting` row is a conflict (→ 409): a delete is in flight.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_on_deleting_account_conflicts() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-del-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        lc.store
            .set_account_state(acct.account_id, AccountState::Deleting)
            .await
            .unwrap();

        let err = lc.logout(acct.account_id).await.unwrap_err();
        assert!(matches!(err, LifecycleError::BeingDeleted(id) if id == acct.account_id));

        delete_account(&lc.store, acct.account_id).await;
    }

    /// Logout on an id with no matching row is a 404, raised before any lock work.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_on_unknown_account_is_not_found() {
        let lc = lifecycle().await;
        let missing = Uuid::new_v4();
        let err = lc.logout(missing).await.unwrap_err();
        assert!(matches!(err, LifecycleError::NotFound(id) if id == missing));
    }

    /// Logout on an `active` row with no live task or cached client (nothing to
    /// cancel or invalidate upstream) still transitions it to `deactivated` — the
    /// state machinery exercised without a homeserver. (The real path, where a live
    /// client is invalidated upstream, is covered manually.)
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_on_active_account_with_no_client_deactivates() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-active-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active by default

        lc.logout(acct.account_id)
            .await
            .expect("logout on an active account deactivates it");

        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deactivated);

        delete_account(&lc.store, acct.account_id).await;
    }

    /// Logout must *await* the supervised task's drain, not merely request
    /// cancellation: cancellation is cooperative, and the task keeps using the
    /// account's SQLite store dir while draining — returning early would let an
    /// immediate re-login restage that dir out from under it. Stands in a fake
    /// task whose "drain" (a short post-cancellation sleep) flips a flag; logout
    /// returning with the flag unset is the regression.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_awaits_supervised_task_drain() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::engine::AccountTask;

        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-drain-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active by default

        let drained = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let handle = tokio::spawn({
            let cancel = cancel.clone();
            let drained = Arc::clone(&drained);
            async move {
                cancel.cancelled().await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                drained.store(true, Ordering::SeqCst);
            }
        });
        lc.tasks
            .lock()
            .unwrap()
            .insert(acct.account_id, AccountTask { cancel, handle });

        lc.logout(acct.account_id).await.expect("logout succeeds");

        assert!(
            drained.load(Ordering::SeqCst),
            "logout returned before the supervised task finished draining"
        );
        assert!(
            !lc.tasks.lock().unwrap().contains_key(&acct.account_id),
            "logout must prune the task-registry entry"
        );
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deactivated);

        delete_account(&lc.store, acct.account_id).await;
    }

    /// Regression for the logout/reconnect race: a client sitting in the
    /// account's connection slot at logout time must be taken out, and the row
    /// deactivated *before* the take, so a connect racing the eviction is either
    /// refused by the cold-connect state gate or has its freshly cached client
    /// taken right back out. A cached client left behind would outlive the
    /// deactivation — `get_or_connect` returns a cache hit without re-checking
    /// state — letting a logged-out account keep sending. The injected client is
    /// offline and unauthenticated; its best-effort upstream logout fails fast
    /// and is swallowed by design.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_takes_cached_client_out_of_its_slot() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-evict-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active by default

        // `server_versions` skips the discovery request, so this builds offline.
        let client = matrix_sdk::Client::builder()
            .homeserver_url("http://127.0.0.1:9") // nothing listens; requests fail fast
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_11])
            .build()
            .await
            .expect("offline client");
        lc.manager.inject_for_test(acct.account_id, client).await;

        lc.logout(acct.account_id).await.expect("logout succeeds");

        assert!(
            lc.manager.take(acct.account_id).await.is_none(),
            "logout must leave no cached client behind"
        );
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deactivated);

        delete_account(&lc.store, acct.account_id).await;
    }

    /// A task that ignores cancellation is escalated to an abort: logout still
    /// succeeds — with the task genuinely terminated, not detached — rather than
    /// returning behind a wedged drain.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn logout_aborts_task_that_ignores_cancellation() {
        use crate::engine::AccountTask;

        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-abort-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active by default

        // Ignores its token entirely; the sleep is an await point, so the abort
        // lands there.
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        });
        lc.tasks
            .lock()
            .unwrap()
            .insert(acct.account_id, AccountTask { cancel, handle });

        lc.logout(acct.account_id)
            .await
            .expect("logout aborts a cancel-ignoring task and succeeds");

        assert!(
            !lc.tasks.lock().unwrap().contains_key(&acct.account_id),
            "the aborted task's registry entry must be pruned"
        );
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deactivated);

        delete_account(&lc.store, acct.account_id).await;
    }

    /// Regression for the reap-timeout escape hatch: a task that survives both
    /// cancel and abort (wedged in non-yielding code) must fail the logout with
    /// `Draining` — task re-registered — and a re-login must be refused while it
    /// lives, so nothing can restage the store dir under it. Once the task
    /// finally dies, a logout retry reaps it and clears the refusal.
    /// (Multi-threaded runtime: the wedged task blocks a worker by design.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres"]
    async fn logout_wedged_task_blocks_relogin_until_reaped() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::engine::AccountTask;

        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@logout-wedged-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active by default

        // No await points at all, so neither cancellation nor abort can land
        // until `unwedge` is set.
        let unwedge = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let handle = tokio::spawn({
            let unwedge = Arc::clone(&unwedge);
            async move {
                while !unwedge.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        lc.tasks
            .lock()
            .unwrap()
            .insert(acct.account_id, AccountTask { cancel, handle });

        let err = lc.logout(acct.account_id).await.unwrap_err();
        assert!(matches!(err, LifecycleError::Draining(id) if id == acct.account_id));
        assert!(
            lc.tasks.lock().unwrap().contains_key(&acct.account_id),
            "a wedged task must stay registered so re-login keeps being refused"
        );
        // The row still deactivated (the flip precedes the reap)...
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deactivated);
        // ...but reactivating it is refused before any store-dir or homeserver
        // work, while the old task may still be using the dir.
        let err = lc.login(Some(hs), &user, "pw").await.unwrap_err();
        assert!(matches!(err, LifecycleError::Draining(id) if id == acct.account_id));

        // Let the task die, then retry: the leftover is reaped and logout
        // completes, clearing the way for a login.
        unwedge.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        lc.logout(acct.account_id)
            .await
            .expect("retry reaps the now-dead task");
        assert!(!lc.tasks.lock().unwrap().contains_key(&acct.account_id));

        delete_account(&lc.store, acct.account_id).await;
    }

    // ---- delete (ADR 0024) ----

    /// Delete of an `active` account removes the row, its on-disk SDK store dir
    /// and staging backup, and retires the identity (a later login would mint a
    /// fresh id, since `find_account_by_identity` now returns `None`).
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_on_active_removes_row_and_store_dir() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@delete-active-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap(); // active

        // Stand in an on-disk store dir + staging backup so we can assert both go.
        let dir = lc.config.data_dir.join(acct.account_id.to_string());
        let backup = lc.config.data_dir.join(format!("{}.prev", acct.account_id));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&backup).unwrap();

        lc.delete(acct.account_id).await.expect("delete succeeds");

        assert!(
            lc.store
                .get_account(acct.account_id)
                .await
                .unwrap()
                .is_none(),
            "row removed"
        );
        assert!(!dir.exists(), "store dir removed");
        assert!(!backup.exists(), "staging backup removed");
        assert!(
            lc.store
                .find_account_by_identity(&user, hs)
                .await
                .unwrap()
                .is_none(),
            "identity retired — a fresh login would mint a new id"
        );
    }

    /// DELETE is refused while the identity is still named by `sync.account`: boot
    /// provisioning would `upsert` it straight back as a fresh active row, so the
    /// deletion wouldn't survive a restart (the resurrection regression). The row
    /// and its store dir are left fully intact.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_of_configured_account_is_refused() {
        let hs = "https://hs.example.org";
        let user = format!("@delete-configured-{}:localhost", Uuid::new_v4());
        let lc = lifecycle_with_provision(&user, hs).await;
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        let dir = lc.config.data_dir.join(acct.account_id.to_string());
        std::fs::create_dir_all(&dir).unwrap();

        let err = lc.delete(acct.account_id).await.unwrap_err();
        assert!(matches!(err, LifecycleError::Provisioned(id) if id == acct.account_id));

        // Nothing was torn down: the row is still present and active, dir intact.
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Active);
        assert!(
            dir.exists(),
            "a configured account's store dir must be left intact"
        );

        delete_account(&lc.store, acct.account_id).await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Delete of a `deactivated` account also fully removes it (retained archive
    /// and all). Logout keeps data; delete erases it.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_on_deactivated_removes_row() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@delete-deact-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        lc.store
            .set_account_state(acct.account_id, AccountState::Deactivated)
            .await
            .unwrap();

        lc.delete(acct.account_id).await.expect("delete succeeds");
        assert!(lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .is_none());
    }

    /// Delete of a row already in `deleting` (a crash/failure left it mid-flight)
    /// **resumes** the teardown to completion rather than erroring — the branch the
    /// boot reconcile and a client retry both take.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_on_deleting_row_resumes_to_completion() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@delete-resume-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        lc.store
            .set_account_state(acct.account_id, AccountState::Deleting)
            .await
            .unwrap();
        let dir = lc.config.data_dir.join(acct.account_id.to_string());
        std::fs::create_dir_all(&dir).unwrap();

        lc.delete(acct.account_id).await.expect("resume completes");
        assert!(lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .is_none());
        assert!(!dir.exists());
    }

    /// Delete twice: the first removes the row, the second finds nothing — the
    /// shape a second concurrent delete sees once the first wins the identity lock.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_is_idempotent_then_not_found() {
        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@delete-twice-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();

        lc.delete(acct.account_id).await.expect("first delete");
        let err = lc.delete(acct.account_id).await.unwrap_err();
        assert!(matches!(err, LifecycleError::NotFound(id) if id == acct.account_id));
    }

    /// Delete on an unknown id is a 404, raised before any lock work.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_on_unknown_account_is_not_found() {
        let lc = lifecycle().await;
        let missing = Uuid::new_v4();
        let err = lc.delete(missing).await.unwrap_err();
        assert!(matches!(err, LifecycleError::NotFound(id) if id == missing));
    }

    /// Delete reaps the account's supervised task (awaiting its drain) before
    /// removing anything, then completes — mirrors the logout drain test.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn delete_reaps_supervised_task_then_completes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::engine::AccountTask;

        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@delete-drain-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();

        let drained = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let handle = tokio::spawn({
            let cancel = cancel.clone();
            let drained = Arc::clone(&drained);
            async move {
                cancel.cancelled().await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                drained.store(true, Ordering::SeqCst);
            }
        });
        lc.tasks
            .lock()
            .unwrap()
            .insert(acct.account_id, AccountTask { cancel, handle });

        lc.delete(acct.account_id).await.expect("delete succeeds");

        assert!(
            drained.load(Ordering::SeqCst),
            "delete returned before the supervised task finished draining"
        );
        assert!(!lc.tasks.lock().unwrap().contains_key(&acct.account_id));
        assert!(lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .is_none());
    }

    /// Regression for the load-bearing ordering: a task that survives cancel **and**
    /// abort fails the delete with `Draining` **before** the store dir is touched —
    /// the row stays `deleting` and the on-disk store is left intact, so nothing is
    /// removed out from under a still-live task. A retry once the task dies finishes
    /// the job.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires Postgres"]
    async fn delete_wedged_task_is_draining_and_preserves_store_dir() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::engine::AccountTask;

        let lc = lifecycle().await;
        let hs = "https://hs.example.org";
        let user = format!("@delete-wedged-{}:localhost", Uuid::new_v4());
        let acct = lc.store.upsert_account(&user, hs).await.unwrap();
        let dir = lc.config.data_dir.join(acct.account_id.to_string());
        std::fs::create_dir_all(&dir).unwrap();

        // No await points: survives both cancel and abort until `unwedge` is set.
        let unwedge = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        let handle = tokio::spawn({
            let unwedge = Arc::clone(&unwedge);
            async move {
                while !unwedge.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        lc.tasks
            .lock()
            .unwrap()
            .insert(acct.account_id, AccountTask { cancel, handle });

        let err = lc.delete(acct.account_id).await.unwrap_err();
        assert!(matches!(err, LifecycleError::Draining(id) if id == acct.account_id));
        // Row left `deleting`, task still registered, and — critically — the store
        // dir is untouched (the teardown aborted before the removal step).
        let after = lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.state, AccountState::Deleting);
        assert!(lc.tasks.lock().unwrap().contains_key(&acct.account_id));
        assert!(dir.exists(), "a live task's store dir must not be removed");

        // Let it die and retry: the leftover is reaped and the delete completes.
        unwedge.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        lc.delete(acct.account_id).await.expect("retry completes");
        assert!(lc
            .store
            .get_account(acct.account_id)
            .await
            .unwrap()
            .is_none());
        assert!(!dir.exists());
    }

    /// The lock-map prune guard (ADR 0024): pruning removes the identity's entry
    /// only when no other verb still holds the lock `Arc`. A parked waiter (a live
    /// extra clone) must keep the entry alive, or it would let a fresh `lock_for`
    /// mint a second lock and break mutual exclusion.
    #[tokio::test]
    #[ignore = "requires Postgres"]
    async fn prune_lock_keeps_entry_while_a_waiter_holds_it() {
        let lc = lifecycle().await;
        let user = format!("@prune-{}:localhost", Uuid::new_v4());
        let hs = "https://hs.example.org";
        let key = format!("{user}\u{0}{hs}");

        // `lock_for` inserts the entry and returns a clone: map(1) + ours(1).
        let lock = lc.lock_for(&user, hs);

        // A second live clone stands in for a parked waiter — strong_count is now 3,
        // so the guard must refuse to prune.
        let waiter = lock.clone();
        lc.prune_lock(&user, hs, &lock);
        assert!(
            lc.locks.lock().unwrap().contains_key(&key),
            "must not prune while a waiter holds the lock"
        );

        // Drop the waiter (back to map + us = 2): now pruning is safe and removes it.
        drop(waiter);
        lc.prune_lock(&user, hs, &lock);
        assert!(
            !lc.locks.lock().unwrap().contains_key(&key),
            "uncontended prune removes the entry"
        );
    }
}
