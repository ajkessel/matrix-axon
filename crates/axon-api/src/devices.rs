//! The device-list port the `/v1/accounts/{id}/devices` handler depends on
//! (M16, ADR 0060).
//!
//! Like [`SenderTrustService`](crate::trust::SenderTrustService), this crate
//! defines the capability it *needs* — listing a Matrix user's devices —
//! rather than depending on whatever provides it. The real implementation
//! lives in `axon-sync` (it needs a live `Client` to query the SDK), adapted
//! onto this port by `axon-server`, so this crate stays free of `axon-sync`
//! and `matrix-sdk`.
//!
//! Unlike the sender-trust bundle there is no durable snapshot half: a device
//! list is inherently point-in-time evidence, read live on every call.

use async_trait::async_trait;
use uuid::Uuid;

/// What can go wrong listing a user's devices. Small and HTTP-shaped, like
/// [`TrustError`](crate::trust::TrustError); the adapter collapses its richer
/// backend error into one of these (see the `From` impl in
/// [`response`](crate::response)).
#[derive(Debug)]
pub enum DeviceListError {
    /// No such account. → `404`.
    NotFound(String),
    /// The account is logged out (`deactivated`) or mid-teardown (`deleting`),
    /// so it has no live client to list devices with. → `409`.
    NotActive(String),
    /// The `user_id` query param isn't a syntactically valid Matrix user id.
    /// → `400`.
    BadRequest(String),
    /// The upstream homeserver / SDK failed the device query. → `502`.
    Upstream(String),
    /// An internal failure (e.g. the store). The detail is logged, not
    /// returned. → `500`.
    Internal,
}

/// One device of the target user.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_id: String,
    pub display_name: Option<String>,
    pub is_verified: bool,
    pub is_cross_signed_by_owner: bool,
    pub local_trust_state: String,
    pub algorithms: Vec<String>,
}

/// The resolved target user plus their devices.
#[derive(Debug, Clone)]
pub struct DeviceList {
    pub user_id: String,
    pub devices: Vec<DeviceInfo>,
}

/// Lists a Matrix user's devices. Implemented outside this crate; held in
/// [`AppState`](crate::AppState) as `Arc<dyn DeviceListService>`.
#[async_trait]
pub trait DeviceListService: Send + Sync {
    /// List devices for `user_id` (the account's own user when `None`). An
    /// unknown account is [`NotFound`](DeviceListError::NotFound); a
    /// logged-out / mid-teardown account is
    /// [`NotActive`](DeviceListError::NotActive).
    async fn list(
        &self,
        account_id: Uuid,
        user_id: Option<&str>,
    ) -> Result<DeviceList, DeviceListError>;
}
