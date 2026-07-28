//! Composition-root adapter: binds `axon-sync`'s concrete [`SenderTrustEngine`]
//! to `axon-api`'s sender-trust port (M7c).
//!
//! Same shape as the [`VerificationAdapter`](crate::verification::VerificationAdapter):
//! this binary is the one place that knows both crates, so the adapter, the error
//! translation, and the bundle mapping live here. `axon-api` and `axon-sync`
//! never depend on each other.

use async_trait::async_trait;
use axon_api::{CurrentTrust, SenderTrustService, TrustBundle, TrustError, TrustSnapshot};
use axon_sync::{
    CurrentTrust as SyncCurrentTrust, SenderTrustEngine, TrustBundle as SyncTrustBundle,
    TrustError as SyncTrustError, TrustSnapshot as SyncTrustSnapshot,
};
use uuid::Uuid;

/// Wraps the sync engine's sender-trust backend so it satisfies the API's
/// `SenderTrustService` port. The orphan rule requires a local newtype.
pub struct TrustAdapter(pub SenderTrustEngine);

/// Map a sync-layer trust error onto the API-layer one (and thus an HTTP status):
/// unknown account/event → not found, a logged-out / mid-teardown account →
/// conflict, a homeserver/SDK failure → upstream, a store failure → a logged
/// internal error.
fn map_err(err: SyncTrustError) -> TrustError {
    match err {
        SyncTrustError::AccountNotFound(id) => {
            TrustError::NotFound(format!("account {id} not found"))
        }
        SyncTrustError::EventNotFound(event_id) => {
            TrustError::NotFound(format!("event {event_id} not found"))
        }
        SyncTrustError::NotActive(id) => TrustError::NotActive(format!(
            "account {id} is not active; log in before reading trust"
        )),
        SyncTrustError::Upstream(msg) => TrustError::Upstream(msg),
        SyncTrustError::Store(msg) => {
            tracing::error!(error = %msg, "store error assembling trust bundle");
            TrustError::Internal
        }
    }
}

fn map_snapshot(s: SyncTrustSnapshot) -> TrustSnapshot {
    TrustSnapshot {
        sender_trust: s.sender_trust,
        verification_state: s.verification_state,
        device_id: s.device_id,
        curve25519_key: s.curve25519_key,
        ed25519_key: s.ed25519_key,
        session_id: s.session_id,
        forwarded: s.forwarded,
        forwarder_user_id: s.forwarder_user_id,
        forwarder_device_id: s.forwarder_device_id,
    }
}

fn map_current(c: SyncCurrentTrust) -> CurrentTrust {
    CurrentTrust {
        device_known: c.device_known,
        device_cross_signed: c.device_cross_signed,
        identity_known: c.identity_known,
        identity_verified: c.identity_verified,
        verification_violation: c.verification_violation,
        previously_verified: c.previously_verified,
        master_key: c.master_key,
    }
}

fn map_bundle(b: SyncTrustBundle) -> TrustBundle {
    TrustBundle {
        sender: b.sender,
        snapshot: b.snapshot.map(map_snapshot),
        current: map_current(b.current),
    }
}

#[async_trait]
impl SenderTrustService for TrustAdapter {
    async fn bundle(&self, account_id: Uuid, event_id: &str) -> Result<TrustBundle, TrustError> {
        self.0
            .bundle(account_id, event_id)
            .await
            .map(map_bundle)
            .map_err(map_err)
    }
}
