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

use axon_core::{LiveEvent, SyncConfig};
use axon_store::{Account, AccountState, Store, StoreError};
use matrix_sdk::ruma::OwnedUserId;
use tokio::sync::broadcast;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::engine::spawn_supervised;
use crate::error::SyncError;
use crate::manager::ClientManager;

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
    ) -> Self {
        Self {
            store,
            config,
            manager,
            live_tx,
            cancel,
            tracker,
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The per-identity lock for `(user_id, homeserver_url)`, created on first use.
    fn lock_for(&self, user_id: &str, homeserver_url: &str) -> Arc<AsyncMutex<()>> {
        let key = format!("{user_id}\u{0}{homeserver_url}");
        // NOTE: this map grows unbounded — one entry per identity ever seen, never
        // removed. Entries are pruned when logout/delete evict the account (the
        // lifecycle teardown verbs); until then it leaks one small entry per
        // distinct identity, which is bounded by the accounts a user ever adds.
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
                AccountState::Deactivated => existing,
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
}
