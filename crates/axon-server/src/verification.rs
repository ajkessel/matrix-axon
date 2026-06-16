//! Composition-root adapter: binds `axon-sync`'s concrete [`VerificationEngine`]
//! to `axon-api`'s verification port.
//!
//! Same shape as the [`LifecycleAdapter`](crate::lifecycle::LifecycleAdapter):
//! this binary is the one place that knows both crates, so the adapter, the
//! error translation, and the flow-state mapping live here. `axon-api` and
//! `axon-sync` never depend on each other.

use async_trait::async_trait;
use axon_api::{FlowStage, FlowSummary, VerificationService, VerifyError};
use axon_sync::{
    FlowStage as SyncFlowStage, FlowState as SyncFlowState, VerificationEngine,
    VerifyError as SyncVerifyError,
};
use uuid::Uuid;

/// Wraps the sync engine's verification backend so it satisfies the API's
/// `VerificationService` port. The orphan rule requires a local newtype.
pub struct VerificationAdapter(pub VerificationEngine);

/// Map a sync-layer verify error onto the API-layer one (and thus an HTTP
/// status): unknown account/flow → not found, a logged-out / mid-teardown account
/// → conflict, an unknown target device → bad request, a homeserver/SDK failure →
/// upstream, a store failure → a logged internal error.
fn map_err(err: SyncVerifyError) -> VerifyError {
    match err {
        SyncVerifyError::NotFound(id) => VerifyError::NotFound(format!("account {id} not found")),
        SyncVerifyError::FlowNotFound(flow_id) => {
            VerifyError::NotFound(format!("verification flow {flow_id} not found"))
        }
        SyncVerifyError::NotActive(id) => VerifyError::NotActive(format!(
            "account {id} is not active; log in before verifying"
        )),
        SyncVerifyError::BeingDeleted(id) => {
            VerifyError::Conflict(format!("account is being deleted: {id}"))
        }
        SyncVerifyError::UnknownDevice(device_id) => {
            VerifyError::BadRequest(format!("unknown device: {device_id}"))
        }
        SyncVerifyError::WrongStage(msg) => VerifyError::Conflict(msg),
        SyncVerifyError::Upstream(msg) => VerifyError::Upstream(msg),
        SyncVerifyError::Store(msg) => {
            tracing::error!(error = %msg, "store error during verification");
            VerifyError::Internal
        }
    }
}

/// Map a sync-layer flow stage onto the API-layer one.
fn map_stage(stage: SyncFlowStage) -> FlowStage {
    match stage {
        SyncFlowStage::Requested => FlowStage::Requested,
        SyncFlowStage::Ready => FlowStage::Ready,
        SyncFlowStage::KeysExchanged => FlowStage::KeysExchanged,
        SyncFlowStage::Confirmed => FlowStage::Confirmed,
        SyncFlowStage::Done => FlowStage::Done,
        SyncFlowStage::Cancelled => FlowStage::Cancelled,
    }
}

/// Map a sync-layer flow snapshot onto the API-layer summary.
fn map_state(state: SyncFlowState) -> FlowSummary {
    FlowSummary {
        flow_id: state.flow_id,
        target_device_id: state.target_device_id,
        stage: map_stage(state.stage),
        emoji: state.emoji,
        decimals: state.decimals,
        cancel_reason: state.cancel_reason,
    }
}

#[async_trait]
impl VerificationService for VerificationAdapter {
    async fn start(&self, account_id: Uuid, device_id: &str) -> Result<String, VerifyError> {
        self.0.start(account_id, device_id).await.map_err(map_err)
    }

    async fn list(&self, account_id: Uuid) -> Result<Vec<FlowSummary>, VerifyError> {
        self.0
            .list(account_id)
            .await
            .map(|flows| flows.into_iter().map(map_state).collect())
            .map_err(map_err)
    }

    async fn get(&self, account_id: Uuid, flow_id: &str) -> Result<FlowSummary, VerifyError> {
        self.0
            .get(account_id, flow_id)
            .await
            .map(map_state)
            .map_err(map_err)
    }

    async fn confirm(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError> {
        self.0.confirm(account_id, flow_id).await.map_err(map_err)
    }

    async fn cancel(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError> {
        self.0.cancel(account_id, flow_id).await.map_err(map_err)
    }
}
