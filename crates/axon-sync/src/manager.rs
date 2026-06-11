//! Per-account [`Client`] lifecycle: the single authority on whether a client
//! exists for an account and how it is built.
//!
//! [`ClientManager`] owns connection only — building the SQLite-backed
//! [`Client`], authenticating it (login on first boot, session restore
//! thereafter, via [`connect_account`](crate::client::connect_account)), and
//! caching one Arc-backed client per `account_id`. It runs no retry loop of its
//! own: the sync supervisor (see [`engine`](crate::engine)) is the always-on
//! caller that keeps each account online via its backoff loop, and the message
//! gateway ([`SdkGateway`](crate::gateway)) is an occasional lazy caller. Both go
//! through [`get_or_connect`](ClientManager::get_or_connect), whose per-account
//! single-flight guard ensures concurrent callers coalesce onto one connect
//! rather than building two clients.
//!
//! Message semantics (send/edit/redact/react) deliberately live in a separate
//! type so this one stays purely about connections (single responsibility).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axon_core::SyncConfig;
use axon_store::{Account, AccountState, Store};
use matrix_sdk::Client;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::client::{connect_account, credential_for, login_new_device};
use crate::error::{GatewayError, SyncError};

/// A per-account connection slot. The slot's [`AsyncMutex`] is what makes a
/// connect single-flight: the first caller holds it across the (awaiting)
/// connect while later callers for the same account wait, then observe the
/// freshly cached client instead of starting their own connect. `None` means
/// "not connected yet"; `Some` caches the live client (clones are cheap — the
/// SDK client is Arc-backed).
type Slot = Arc<AsyncMutex<Option<Client>>>;

/// Owns and caches one matrix-rust-sdk [`Client`] per account. Cheap to
/// [`Clone`] — every field is a handle — so it is shared by both the sync
/// supervisor and the message gateway.
#[derive(Clone)]
pub struct ClientManager {
    store: Store,
    config: SyncConfig,
    /// `account_id → slot`. The outer (std) mutex is held only briefly to fetch
    /// or insert a slot; the awaiting connect happens under the slot's own async
    /// mutex, so connects for different accounts never block each other and a
    /// connect never blocks the map.
    slots: Arc<Mutex<HashMap<Uuid, Slot>>>,
}

impl ClientManager {
    /// Build a manager over the store handle and sync config. No clients are
    /// connected until [`get_or_connect`](Self::get_or_connect) is first called
    /// for an account.
    pub fn new(store: Store, config: SyncConfig) -> Self {
        Self {
            store,
            config,
            slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Fetch (or create) the connection slot for `account_id`.
    fn slot(&self, account_id: Uuid) -> Slot {
        let mut map = self.slots.lock().expect("client slot map poisoned");
        map.entry(account_id).or_default().clone()
    }

    /// Return the cached client for `account_id`, building and authenticating one
    /// if the account isn't connected yet. Single-flight per account: concurrent
    /// callers coalesce onto a single connect.
    ///
    /// An unknown account id is [`GatewayError::UnknownAccount`]; a non-`active`
    /// account is [`GatewayError::AccountNotActive`] (not retryable without a
    /// login); a connect that fails (homeserver unreachable, auth/restore error,
    /// store error) is [`GatewayError::NotConnected`] — retryable.
    pub async fn get_or_connect(&self, account_id: Uuid) -> Result<Client, GatewayError> {
        let slot = self.slot(account_id);
        let mut guard = slot.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }

        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| GatewayError::NotConnected(e.to_string()))?
            .ok_or(GatewayError::UnknownAccount(account_id))?;

        // Only `active` accounts get a *new* client. The supervisor lists only
        // active rows, but the gateway connects lazily for any id an API send
        // names, so the cold-connect gate lives here too. Note this runs only on a
        // cache miss: an account that already has a cached client (above) is not
        // re-checked, so this gate alone doesn't sever a *connected* account on a
        // state change. The lifecycle verbs do that actively — logout/delete flip
        // the row out of `active` *first* (so this gate refuses any new connect),
        // then stop the account's supervised task and take its cached client out
        // of the slot (see `take`) (ADR 0022).
        if account.state != AccountState::Active {
            return Err(GatewayError::AccountNotActive(account_id));
        }

        let credential = credential_for(&self.config, &account)?;
        let client = connect_account(&self.store, &account, &self.config, credential).await?;
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Log `account` in as a fresh device and cache the resulting live client in
    /// its connection slot, so the supervised sync task's first
    /// [`get_or_connect`](Self::get_or_connect) reuses this client rather than
    /// building a second one (ADR 0021). The lifecycle layer calls this under its
    /// per-identity lock, having already resolved/minted the row; the caller flips
    /// the row to `active` and spawns the task only on success.
    ///
    /// Holding the slot lock across the login makes it single-flight with any
    /// concurrent `get_or_connect`. Any previously cached client is discarded
    /// first (a reactivated row may carry a stale slot from a prior session).
    pub async fn login(&self, account: &Account, password: &str) -> Result<Client, SyncError> {
        let slot = self.slot(account.account_id);
        let mut guard = slot.lock().await;
        *guard = None;
        let client = login_new_device(&self.store, account, &self.config, password).await?;
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Take the cached client for `account_id` out of its slot, returning it (or
    /// `None` if nothing is cached). This both *yields* the client — so the caller
    /// can do one last thing with it, e.g. invalidate the device token upstream on
    /// logout — and *evicts* it (the slot is left empty), atomically under the slot
    /// lock so a concurrent [`get_or_connect`](Self::get_or_connect) can't observe a
    /// half-removed client. Unlike [`evict`](Self::evict) this awaits the slot lock
    /// rather than skipping a slot that is mid-connect.
    pub async fn take(&self, account_id: Uuid) -> Option<Client> {
        let slot = self.slot(account_id);
        let mut guard = slot.lock().await;
        guard.take()
    }

    /// Put a client straight into an account's slot, standing in for a connect.
    /// Lets a test exercise the cached-client paths (eviction on logout, the
    /// cache-hit fast path) without a reachable homeserver.
    #[cfg(test)]
    pub(crate) async fn inject_for_test(&self, account_id: Uuid, client: Client) {
        let slot = self.slot(account_id);
        *slot.lock().await = Some(client);
    }

    /// Drop the cached client for `account_id` so the next
    /// [`get_or_connect`](Self::get_or_connect) rebuilds it. Called by the sync
    /// supervisor when a run fails, so a supervised restart reconnects cleanly.
    /// A no-op if nothing is cached. If a connect is in flight (the slot is
    /// locked) this skips — that connect is already producing a fresh client.
    pub fn evict(&self, account_id: Uuid) {
        let slot = {
            let map = self.slots.lock().expect("client slot map poisoned");
            map.get(&account_id).cloned()
        };
        if let Some(slot) = slot {
            if let Ok(mut guard) = slot.try_lock() {
                *guard = None;
            }
        }
    }
}
