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
    /// `username` was not a syntactically valid full Matrix user ID.
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
    pub async fn login(
        &self,
        homeserver_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Uuid, LifecycleError> {
        // Validate the MXID up front so identity resolves before we touch the DB
        // or build an SDK store. We don't need the parsed value — just the check.
        OwnedUserId::try_from(username)
            .map_err(|e| LifecycleError::InvalidUserId(format!("{username}: {e}")))?;

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

        // Stop supervision and wait until the task has *terminated* (not merely
        // been asked to stop) — see `reap_task`. A re-login restages the store
        // dir the draining task still holds, so logout must not return (releasing
        // the identity lock) while the task may still be using it.
        self.reap_task(account_id).await?;

        // Take the cached client out of its slot (this also evicts it) and use it
        // to invalidate the device token upstream. `take` awaits the slot lock, so
        // a connect that read `active` before the flip above finishes caching and
        // then has its client taken right back out here. The upstream call is
        // best-effort with a short cap: an unreachable or stalled homeserver must
        // not stall logout (the row is already deactivated) — the device then
        // lingers upstream until reachable.
        if let Some(client) = self.manager.take(account_id).await {
            match tokio::time::timeout(UPSTREAM_LOGOUT_TIMEOUT, client.matrix_auth().logout()).await
            {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(
                    %account_id,
                    error = %err,
                    "upstream logout failed; account deactivated locally"
                ),
                Err(_) => tracing::warn!(
                    %account_id,
                    timeout_secs = UPSTREAM_LOGOUT_TIMEOUT.as_secs(),
                    "upstream logout timed out; account deactivated locally"
                ),
            }
        }

        tracing::info!(%account_id, user_id = %account.user_id, "account logged out");
        Ok(())
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
            .login(hs, &user, "not-the-password")
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

        let err = lc.login(hs, &user, "pw").await.unwrap_err();
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
            .login("https://hs.example.org", "not-an-mxid", "pw")
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
        let err = lc.login(hs, &user, "pw").await.unwrap_err();
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
}
