//! Composition-root adapters: bind `axon-sync`'s [`BackfillHealth`] and
//! [`SyncHealth`] to `axon-api`'s status ports (M10), so `GET /v1/status`
//! reports the engine's disk-space health and each account's sync-service
//! state. This binary is the one place that knows both crates; `axon-api` and
//! `axon-sync` never depend on each other.

use axon_api::{
    AccountSyncSnapshot, BackfillStatusProvider, BackfillStatusSnapshot, SyncStateProvider,
    SyncStatusProvider,
};
use axon_sync::{BackfillHealth, SyncHealth};
use uuid::Uuid;

/// Wraps the sync engine's shared [`BackfillHealth`] so it satisfies the API's
/// [`BackfillStatusProvider`] port. The orphan rule requires a local newtype.
pub struct BackfillStatusAdapter(pub BackfillHealth);

impl BackfillStatusProvider for BackfillStatusAdapter {
    fn snapshot(&self) -> BackfillStatusSnapshot {
        BackfillStatusSnapshot {
            paused_low_disk: self.0.paused_low_disk(),
            free_bytes: self.0.free_bytes(),
        }
    }
}

/// Wraps the sync engine's shared [`SyncHealth`] so it satisfies the API's
/// [`SyncStatusProvider`] port. The orphan rule requires a local newtype.
pub struct SyncStatusAdapter(pub SyncHealth);

impl SyncStatusProvider for SyncStatusAdapter {
    fn snapshot(&self) -> Vec<AccountSyncSnapshot> {
        self.0
            .snapshot()
            .into_iter()
            .map(|(account_id, status)| AccountSyncSnapshot {
                account_id,
                state: status.state.as_str(),
                since: status.since,
            })
            .collect()
    }
}

/// Wraps the sync engine's shared [`SyncHealth`] so it satisfies the API's
/// [`SyncStateProvider`] port (ADR 0030, issue #241) — the same handle
/// [`SyncStatusAdapter`] wraps above, since `SyncHealth` derives both the raw
/// `/v1/status` surface and the coarser `AccountDto.sync_state` vocabulary.
/// The orphan rule requires a local newtype.
pub struct SyncStateAdapter(pub SyncHealth);

impl SyncStateProvider for SyncStateAdapter {
    fn sync_state(&self, account_id: Uuid) -> &'static str {
        self.0.sync_state(account_id)
    }
}
