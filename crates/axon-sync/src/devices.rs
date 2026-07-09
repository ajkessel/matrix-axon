//! Device-list / discovery: `GET …/accounts/{id}/devices` (M16, ADR 0060).
//!
//! Lists a Matrix user's devices as the SDK currently knows them — the
//! account's own user by default, or an arbitrary `user_id` (cross-user
//! picker, ADR 0040 parity). `client.encryption().get_user_devices()` is a
//! **local crypto-store read**, not a homeserver round-trip: it calls
//! `OlmMachine::get_user_devices(user_id, None)`, and a `None` timeout makes
//! `wait_if_user_pending` a no-op, so the call never issues or waits on a
//! `/keys/query`. Freshness is therefore bounded by however current the SDK's
//! own crypto store is — kept up to date by the account's background sync
//! loop, not by this endpoint — so a device added moments ago and not yet
//! synced can be briefly absent. That existing SDK-side cache is why there's
//! still no new axon-owned table: the SDK already caches this, so a second,
//! axon-maintained materialization would just be a second cache to keep in
//! sync with the first.
//!
//! Mirrors [`SenderTrustEngine`](crate::trust::SenderTrustEngine)'s shape: a
//! concrete engine owned by [`SyncEngine`](crate::SyncEngine), adapted onto
//! the API's `DeviceListService` port by `axon-server`. Also **read-only and
//! takes no per-identity lock** — see [`DeviceListEngine::list`] for why.

use axon_store::{AccountState, Store};
use matrix_sdk::encryption::LocalTrust;
use matrix_sdk::ruma::OwnedUserId;
use uuid::Uuid;

use crate::error::GatewayError;
use crate::manager::ClientManager;

/// What can go wrong listing a user's devices. The adapter in `axon-server`
/// maps these onto `axon-api`'s HTTP-shaped `DeviceListError`.
#[derive(Debug, thiserror::Error)]
pub enum DeviceListError {
    /// No account exists for the given id.
    #[error("no such account: {0}")]
    AccountNotFound(Uuid),
    /// The account is logged out (`deactivated`) or mid-teardown (`deleting`)
    /// — no live client to list devices with.
    #[error("account is not active: {0}")]
    NotActive(Uuid),
    /// The requested (or the account's own stored) user id isn't a
    /// syntactically valid Matrix user id.
    #[error("invalid user id: {0}")]
    InvalidUserId(String),
    /// The homeserver connection itself failed (e.g. `get_or_connect`
    /// couldn't establish or restore the SDK client). The device query proper
    /// (`get_user_devices`) is a local crypto-store read and never lands
    /// here — its failures are [`Store`](Self::Store).
    #[error("upstream error: {0}")]
    Upstream(String),
    /// A local failure: the `axon-store` Postgres read, a corrupt stored
    /// `user_id`, or the SDK's local crypto-store read
    /// (`get_user_devices`/`NoOlmMachine`/a wrapped `CryptoStoreError`)
    /// failing. None of these are homeserver-caused, so they're kept out of
    /// [`Upstream`](Self::Upstream).
    #[error("store error: {0}")]
    Store(String),
}

/// One device of the target user, as the SDK currently reports it.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_id: String,
    pub display_name: Option<String>,
    /// Locally *or* cross-signing trusted — the SDK's combined predicate.
    pub is_verified: bool,
    /// Cross-signed specifically by the device owner's own master key.
    pub is_cross_signed_by_owner: bool,
    /// `"verified" | "black_listed" | "ignored" | "unset"`.
    pub local_trust_state: String,
    /// Encryption algorithms this device advertises, e.g.
    /// `"m.megolm.v1.aes-sha2"`.
    pub algorithms: Vec<String>,
}

/// The resolved target user plus their devices.
#[derive(Debug, Clone)]
pub struct DeviceList {
    pub user_id: String,
    pub devices: Vec<DeviceInfo>,
}

/// Lists a Matrix user's devices. Cheap to clone (handles only). Built by
/// [`SyncEngine::devices`](crate::SyncEngine::devices) and adapted onto the
/// API's device-list port by `axon-server`.
#[derive(Clone)]
pub struct DeviceListEngine {
    store: Store,
    manager: ClientManager,
}

impl DeviceListEngine {
    pub(crate) fn new(store: Store, manager: ClientManager) -> Self {
        Self { store, manager }
    }

    /// List devices for `user_id` (or the account's own user when `None`).
    ///
    /// **Read-only, no per-identity lifecycle lock** — same reasoning as
    /// [`SenderTrustEngine::bundle`](crate::trust::SenderTrustEngine::bundle):
    /// a listing read tolerates a client a concurrent logout/delete is
    /// severing (worst case a `Store`/`Upstream` error). `get_user_devices`
    /// itself is a local crypto-store read with no network round-trip (see
    /// the module docs), so there's no homeserver call to worry about
    /// blocking behind a lock either way — staying lock-free here just keeps
    /// this engine's shape consistent with `SenderTrustEngine::bundle`'s. The
    /// `active` gate is an explicit store read, not
    /// [`ClientManager::get_or_connect`]'s cache-hit fast path, which doesn't
    /// re-check `accounts.state` on a hit.
    pub async fn list(
        &self,
        account_id: Uuid,
        user_id: Option<&str>,
    ) -> Result<DeviceList, DeviceListError> {
        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| DeviceListError::Store(e.to_string()))?
            .ok_or(DeviceListError::AccountNotFound(account_id))?;
        if account.state != AccountState::Active {
            return Err(DeviceListError::NotActive(account_id));
        }

        let target: OwnedUserId = match user_id {
            Some(raw) => raw
                .parse()
                .map_err(|_| DeviceListError::InvalidUserId(raw.to_owned()))?,
            None => account.user_id.parse().map_err(|_| {
                DeviceListError::Store(format!("invalid stored user id: {}", account.user_id))
            })?,
        };

        // `get_or_connect` re-validates existence (`UnknownAccount` → 404) and
        // `active` (`AccountNotActive` → 409) on a cold connect; after the
        // explicit gate above it primarily yields the live client to query.
        let client = self
            .manager
            .get_or_connect(account_id)
            .await
            .map_err(|e| match e {
                GatewayError::UnknownAccount(id) => DeviceListError::AccountNotFound(id),
                GatewayError::AccountNotActive(id) => DeviceListError::NotActive(id),
                other => DeviceListError::Upstream(other.to_string()),
            })?;

        // A local crypto-store read (`NoOlmMachine` or a wrapped
        // `CryptoStoreError`, never a homeserver failure — see the module
        // docs), so its failures are `Store`, not `Upstream`.
        let user_devices = client
            .encryption()
            .get_user_devices(&target)
            .await
            .map_err(|e| DeviceListError::Store(e.to_string()))?;

        let devices = user_devices
            .devices()
            .filter(|d| !d.is_deleted())
            .map(|d| DeviceInfo {
                device_id: d.device_id().to_string(),
                display_name: d.display_name().map(str::to_owned),
                is_verified: d.is_verified(),
                is_cross_signed_by_owner: d.is_cross_signed_by_owner(),
                local_trust_state: local_trust_str(d.local_trust_state()),
                algorithms: d
                    .algorithms()
                    .iter()
                    .map(|a| a.as_str().to_owned())
                    .collect(),
            })
            .collect();

        Ok(DeviceList {
            user_id: target.to_string(),
            devices,
        })
    }
}

/// Maps the SDK's [`LocalTrust`] to the store/DTO snake_case string
/// convention used elsewhere (e.g. `event_sender_trust`'s columns).
fn local_trust_str(state: LocalTrust) -> String {
    match state {
        LocalTrust::Verified => "verified",
        LocalTrust::BlackListed => "black_listed",
        LocalTrust::Ignored => "ignored",
        LocalTrust::Unset => "unset",
    }
    .to_owned()
}
