//! Per-account sync-service health, shared between each account's supervised
//! run loop (which records every state transition) and the API status surface
//! (`GET /v1/status`). Same shape as [`crate::backfill::BackfillHealth`], but
//! keyed per account since each account runs its own [`matrix_sdk_ui::sync_service::SyncService`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use matrix_sdk_ui::sync_service::State;
use uuid::Uuid;

/// A serialization/log-friendly copy of [`State`]: that type isn't `Eq` and
/// carries an `Arc<Error>` on the error variant that this crate's callers (the
/// API status DTO, log fields) have no business holding onto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    Idle,
    Running,
    Offline,
    Terminated,
    Error,
}

impl SyncState {
    pub fn as_str(self) -> &'static str {
        match self {
            SyncState::Idle => "idle",
            SyncState::Running => "running",
            SyncState::Offline => "offline",
            SyncState::Terminated => "terminated",
            SyncState::Error => "error",
        }
    }
}

impl From<&State> for SyncState {
    fn from(state: &State) -> Self {
        match state {
            State::Idle => SyncState::Idle,
            State::Running => SyncState::Running,
            State::Offline => SyncState::Offline,
            State::Terminated => SyncState::Terminated,
            State::Error(_) => SyncState::Error,
        }
    }
}

/// A point-in-time snapshot of one account's sync-service state.
#[derive(Debug, Clone, Copy)]
pub struct AccountSyncStatus {
    pub state: SyncState,
    /// When this account last entered `state`.
    pub since: SystemTime,
}

/// Shared handle: one per engine, holding every supervised account's latest
/// sync-service state. Cheap to clone (an `Arc` internally).
#[derive(Clone, Default)]
pub struct SyncHealth {
    inner: Arc<Mutex<HashMap<Uuid, AccountSyncStatus>>>,
}

impl SyncHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a state transition for one account, overwriting whatever was
    /// there before with a fresh `since`. Called from the account's own
    /// supervised task, so writes for different accounts never race each
    /// other's `since`.
    pub fn set(&self, account_id: Uuid, state: &State) {
        self.inner
            .lock()
            .expect("sync health mutex poisoned")
            .insert(
                account_id,
                AccountSyncStatus {
                    state: SyncState::from(state),
                    since: SystemTime::now(),
                },
            );
    }

    /// Mark an account as errored without an SDK [`State`] in hand — used when
    /// the supervised task fails before the sync service ever reaches its own
    /// state stream (e.g. the initial connect), so `/v1/status` doesn't keep
    /// reporting a stale prior-run state through the restart backoff.
    pub fn set_error(&self, account_id: Uuid) {
        self.inner
            .lock()
            .expect("sync health mutex poisoned")
            .insert(
                account_id,
                AccountSyncStatus {
                    state: SyncState::Error,
                    since: SystemTime::now(),
                },
            );
    }

    /// Drop an account's entry (on logout/delete) so `/v1/status` stops
    /// reporting an account that no longer exists.
    pub fn remove(&self, account_id: Uuid) {
        self.inner
            .lock()
            .expect("sync health mutex poisoned")
            .remove(&account_id);
    }

    /// A snapshot of every currently-known account's status.
    pub fn snapshot(&self) -> Vec<(Uuid, AccountSyncStatus)> {
        self.inner
            .lock()
            .expect("sync health mutex poisoned")
            .iter()
            .map(|(id, status)| (*id, *status))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_records_the_states_own_transition() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();

        health.set(account_id, &State::Offline);

        let snapshot = health.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].0, account_id);
        assert_eq!(snapshot[0].1.state, SyncState::Offline);
    }

    #[test]
    fn set_overwrites_the_prior_state_for_the_same_account() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();

        health.set(account_id, &State::Offline);
        health.set(account_id, &State::Running);

        let snapshot = health.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].1.state, SyncState::Running);
    }

    #[test]
    fn set_error_records_error_without_an_sdk_state() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();

        health.set_error(account_id);

        let snapshot = health.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].1.state, SyncState::Error);
    }

    #[test]
    fn remove_drops_the_entry() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();
        health.set(account_id, &State::Running);

        health.remove(account_id);

        assert!(health.snapshot().is_empty());
    }

    #[test]
    fn remove_of_an_unknown_account_is_a_no_op() {
        let health = SyncHealth::new();

        health.remove(Uuid::new_v4());

        assert!(health.snapshot().is_empty());
    }

    #[test]
    fn snapshot_covers_every_known_account_independently() {
        let health = SyncHealth::new();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();

        health.set(first, &State::Running);
        health.set(second, &State::Terminated);

        let mut snapshot = health.snapshot();
        snapshot.sort_by_key(|(id, _)| *id);
        let mut expected = [first, second];
        expected.sort();

        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].0, expected[0]);
        assert_eq!(snapshot[1].0, expected[1]);
    }
}
