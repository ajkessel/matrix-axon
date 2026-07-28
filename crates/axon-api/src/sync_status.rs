//! Sync-service status port, for `GET /v1/status`.
//!
//! Reports each account's live sync-service state (connected/idle/offline/erroring)
//! so a client — or an operator watching `/v1/status` — can tell sync is actually
//! running rather than silently wedged. Like [`crate::backfill`], the API depends
//! only on this port; the binary injects an adapter over `axon_sync::SyncHealth`,
//! keeping this crate free of `axon-sync`.

use std::time::SystemTime;

use uuid::Uuid;

/// A point-in-time snapshot of one account's sync-service state.
#[derive(Debug, Clone)]
pub struct AccountSyncSnapshot {
    pub account_id: Uuid,
    /// One of `"idle"`, `"running"`, `"offline"`, `"terminated"`, `"error"`.
    pub state: &'static str,
    /// When the account last entered `state`.
    pub since: SystemTime,
}

/// Port exposing every supervised account's sync-service state to `GET /v1/status`.
pub trait SyncStatusProvider: Send + Sync {
    /// A snapshot of every account the binary currently knows about (cheap;
    /// reads a shared mutex-guarded map).
    fn snapshot(&self) -> Vec<AccountSyncSnapshot>;
}

/// Fallback provider used when the binary injects none (e.g. in API tests, or a
/// build with sync compiled out): reports no accounts.
pub(crate) struct NoSyncStatus;

impl SyncStatusProvider for NoSyncStatus {
    fn snapshot(&self) -> Vec<AccountSyncSnapshot> {
        Vec::new()
    }
}
