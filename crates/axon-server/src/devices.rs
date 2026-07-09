//! Composition-root adapter: binds `axon-sync`'s concrete [`DeviceListEngine`]
//! to `axon-api`'s device-list port (M16, ADR 0060).
//!
//! Same shape as [`TrustAdapter`](crate::trust::TrustAdapter): this binary is
//! the one place that knows both crates, so the adapter, error translation,
//! and mapping live here. `axon-api` and `axon-sync` never depend on each
//! other.

use async_trait::async_trait;
use axon_api::{DeviceInfo, DeviceList, DeviceListError, DeviceListService};
use axon_sync::{
    DeviceInfo as SyncDeviceInfo, DeviceList as SyncDeviceList, DeviceListEngine,
    DeviceListError as SyncDeviceListError,
};
use uuid::Uuid;

/// Wraps the sync engine's device-list backend so it satisfies the API's
/// `DeviceListService` port. The orphan rule requires a local newtype.
pub struct DeviceAdapter(pub DeviceListEngine);

/// Map a sync-layer device-list error onto the API-layer one (and thus an
/// HTTP status): unknown account → not found, a logged-out / mid-teardown
/// account → conflict, an invalid `user_id` → bad request, a homeserver/SDK
/// failure → upstream, a store failure → a logged internal error.
fn map_err(err: SyncDeviceListError) -> DeviceListError {
    match err {
        SyncDeviceListError::AccountNotFound(id) => {
            DeviceListError::NotFound(format!("account {id} not found"))
        }
        SyncDeviceListError::NotActive(id) => DeviceListError::NotActive(format!(
            "account {id} is not active; log in before listing devices"
        )),
        SyncDeviceListError::InvalidUserId(user_id) => {
            DeviceListError::BadRequest(format!("invalid user id: {user_id}"))
        }
        SyncDeviceListError::Upstream(msg) => DeviceListError::Upstream(msg),
        SyncDeviceListError::Store(msg) => {
            tracing::error!(error = %msg, "store error listing devices");
            DeviceListError::Internal
        }
    }
}

fn map_device(d: SyncDeviceInfo) -> DeviceInfo {
    DeviceInfo {
        device_id: d.device_id,
        display_name: d.display_name,
        is_verified: d.is_verified,
        is_cross_signed_by_owner: d.is_cross_signed_by_owner,
        local_trust_state: d.local_trust_state,
        algorithms: d.algorithms,
    }
}

fn map_list(l: SyncDeviceList) -> DeviceList {
    DeviceList {
        user_id: l.user_id,
        devices: l.devices.into_iter().map(map_device).collect(),
    }
}

#[async_trait]
impl DeviceListService for DeviceAdapter {
    async fn list(
        &self,
        account_id: Uuid,
        user_id: Option<&str>,
    ) -> Result<DeviceList, DeviceListError> {
        self.0
            .list(account_id, user_id)
            .await
            .map(map_list)
            .map_err(map_err)
    }
}
