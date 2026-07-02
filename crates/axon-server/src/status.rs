//! Composition-root adapter: binds `axon-sync`'s [`BackfillHealth`] to
//! `axon-api`'s backfill-status port (M10), so `GET /v1/status` reports the
//! engine's disk-space health. This binary is the one place that knows both
//! crates; `axon-api` and `axon-sync` never depend on each other.

use axon_api::{BackfillStatusProvider, BackfillStatusSnapshot};
use axon_sync::BackfillHealth;

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
