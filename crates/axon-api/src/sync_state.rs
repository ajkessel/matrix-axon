//! Account sync-readiness port (ADR 0030, issue #241), for `AccountDto.sync_state`
//! and the `account.sync_state` WS frame.
//!
//! Distinct from [`crate::sync_status`] (the raw SDK `SyncService::State`
//! surface behind `GET /v1/status`, for operators): this port reports the
//! coarser four-value vocabulary API clients act on — whether mutations to an
//! account are currently reliable — collapsing the detail `sync_status`
//! exposes. Like `sync_status`, the API depends only on this port; the binary
//! injects an adapter over `axon_sync::SyncHealth`, keeping this crate free of
//! `axon-sync`.

use uuid::Uuid;

/// Port exposing each account's sync-engine readiness (ADR 0030) to the
/// accounts routes and the WS frame emitter.
pub trait SyncStateProvider: Send + Sync {
    /// One of `"connecting"`, `"syncing"`, `"ready"`, `"offline"`. An account
    /// this provider has never heard of (no supervised sync task has ever
    /// recorded a transition — e.g. a `deactivated` account) reports
    /// `"connecting"`.
    fn sync_state(&self, account_id: Uuid) -> &'static str;
}

/// Fallback provider used when the binary injects none (e.g. in API tests, or
/// a build with sync compiled out): every account reports `"connecting"`.
pub(crate) struct NoSyncState;

impl SyncStateProvider for NoSyncState {
    fn sync_state(&self, _account_id: Uuid) -> &'static str {
        "connecting"
    }
}
