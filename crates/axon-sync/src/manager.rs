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
use axon_store::Store;
use matrix_sdk::Client;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use crate::client::{connect_account, credential_for};
use crate::error::GatewayError;

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
    /// An unknown account id is [`GatewayError::UnknownAccount`]; a connect that
    /// fails (homeserver unreachable, auth/restore error, store error) is
    /// [`GatewayError::NotConnected`] — both retryable from the caller's side.
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

        let credential = credential_for(&self.config, &account)?;
        let client = connect_account(&self.store, &account, &self.config, credential).await?;
        *guard = Some(client.clone());
        Ok(client)
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
