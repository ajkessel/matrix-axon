//! Per-account sync-service health, shared between each account's supervised
//! run loop (which records every state transition) and the API status surface
//! (`GET /v1/status`). Same shape as [`crate::backfill::BackfillHealth`], but
//! keyed per account since each account runs its own [`matrix_sdk_ui::sync_service::SyncService`].
//!
//! This module also derives the coarser account-readiness vocabulary from ADR
//! 0030 (issue #241) — `"connecting"` / `"syncing"` / `"ready"` / `"offline"`,
//! surfaced on `AccountDto.sync_state` and the `account.sync_state` WS frame —
//! from the same per-account state this module already tracks, plus a
//! one-shot `ready` flag set the first time an account's room-list sync
//! reaches a steady state (see [`SyncHealth::mark_ready`]).
//!
//! `ready` is tracked in a set **separate** from the raw `AccountSyncStatus`
//! map, not as a field on it: `mark_ready` can fire before this account has
//! ever recorded a real `SyncService::State` transition (the room-list
//! service's state machine, which `mark_ready` watches, can reach `Running`
//! before the outer `SyncService`'s own subscriber has observed anything —
//! see `engine.rs`'s pre-loop readiness check). A `status`-shaped `ready`
//! field would force `mark_ready` to *invent* a raw entry (e.g. a fabricated
//! `Idle` with a `since` of "just now") for that case, and `snapshot()` feeds
//! `GET /v1/status` — a real operator-facing surface that must only ever
//! report state the SDK actually reported. Keeping the two maps separate lets
//! `mark_ready` update readiness without ever touching `snapshot()`'s view.

use std::collections::{HashMap, HashSet};
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

/// A point-in-time snapshot of one account's sync-service state, as actually
/// reported by the SDK. Never fabricated — every entry corresponds to a real
/// [`SyncHealth::set`] or [`SyncHealth::set_error`] call.
#[derive(Debug, Clone, Copy)]
pub struct AccountSyncStatus {
    pub state: SyncState,
    /// When this account last entered `state`.
    pub since: SystemTime,
}

/// The ADR 0030 readiness vocabulary derived from an account's raw SDK state
/// (`None` if this handle has never recorded one — SDK client not built /
/// session not restored) plus whether it's earned the one-shot `ready` flag
/// (see [`SyncHealth::mark_ready`]):
///
/// - `"offline"` — the sync service is retrying a lost connection.
/// - `"connecting"` — no raw state yet and not ready, or erroring/terminated
///   (indistinguishable from a fresh connect — a supervised restart is about
///   to happen).
/// - `"ready"` — a full sync cycle has completed, regardless of whether the
///   raw `SyncService::State` subscriber has independently observed anything
///   yet (the room-list-service signal `mark_ready` watches can arrive first
///   — see `engine.rs`).
/// - `"syncing"` — a raw `Idle`/`Running` state is on record but not ready.
fn readiness_label(state: Option<SyncState>, ready: bool) -> &'static str {
    match state {
        Some(SyncState::Offline) => "offline",
        Some(SyncState::Error) | Some(SyncState::Terminated) => "connecting",
        Some(SyncState::Idle) | Some(SyncState::Running) => {
            if ready {
                "ready"
            } else {
                "syncing"
            }
        }
        None => {
            if ready {
                "ready"
            } else {
                "connecting"
            }
        }
    }
}

/// Everything one [`SyncHealth`] handle guards under a single lock, so a
/// label transition is always computed from a consistent before/after view of
/// both maps rather than risking a read torn across two separate locks.
#[derive(Default)]
struct Inner {
    status: HashMap<Uuid, AccountSyncStatus>,
    ready: HashSet<Uuid>,
}

impl Inner {
    fn label(&self, account_id: Uuid) -> &'static str {
        readiness_label(
            self.status.get(&account_id).map(|s| s.state),
            self.ready.contains(&account_id),
        )
    }
}

/// Shared handle: one per engine, holding every supervised account's latest
/// sync-service state. Cheap to clone (an `Arc` internally).
#[derive(Clone, Default)]
pub struct SyncHealth {
    inner: Arc<Mutex<Inner>>,
}

impl SyncHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a state transition for one account, overwriting whatever was
    /// there before with a fresh `since`. Called from the account's own
    /// supervised task, so writes for different accounts never race each
    /// other's `since`. Does not touch `ready` — an in-run transition (e.g. a
    /// brief `Offline` blip and recovery) doesn't clear earned readiness; see
    /// [`Self::set_error`] for the one path that does.
    ///
    /// Returns the account's new [`readiness_label`] if this call changed it
    /// from what it was before (or from the no-entry-yet default), or `None`
    /// if the derived label is unchanged — so a caller can push an
    /// `account.sync_state` frame only on an actual transition.
    pub fn set(&self, account_id: Uuid, state: &State) -> Option<&'static str> {
        let mut inner = self.inner.lock().expect("sync health mutex poisoned");
        let previous = inner.label(account_id);
        inner.status.insert(
            account_id,
            AccountSyncStatus {
                state: SyncState::from(state),
                since: SystemTime::now(),
            },
        );
        let new = inner.label(account_id);
        (new != previous).then_some(new)
    }

    /// Mark an account as errored without an SDK [`State`] in hand — used when
    /// the supervised task fails before the sync service ever reaches its own
    /// state stream (e.g. the initial connect), so `/v1/status` doesn't keep
    /// reporting a stale prior-run state through the restart backoff. Also
    /// clears `ready`: the next run starts a fresh sync service, so readiness
    /// has to be re-earned (unlike an in-run `Offline` blip, which doesn't
    /// touch it — see [`Self::set`]).
    ///
    /// Returns the new readiness label if it changed, same as [`Self::set`].
    pub fn set_error(&self, account_id: Uuid) -> Option<&'static str> {
        let mut inner = self.inner.lock().expect("sync health mutex poisoned");
        let previous = inner.label(account_id);
        inner.status.insert(
            account_id,
            AccountSyncStatus {
                state: SyncState::Error,
                since: SystemTime::now(),
            },
        );
        inner.ready.remove(&account_id);
        let new = inner.label(account_id);
        (new != previous).then_some(new)
    }

    /// Mark an account's *current* supervised run as having completed a full
    /// sync cycle — mutations to it are now reliable (ADR 0030). One-shot per
    /// run: cleared only by [`Self::set_error`] ahead of the next restart.
    /// Called once, from the room-list-service state watch, the first time
    /// its state reaches a steady `Running` (see `engine.rs`) — which can
    /// happen before this account has ever recorded a raw [`AccountSyncStatus`]
    /// (the two subscribers race independently), so this deliberately never
    /// touches `status`/`snapshot()`: only [`Self::set`]/[`Self::set_error`]
    /// do, since those alone reflect what the SDK actually reported.
    ///
    /// Returns the new readiness label if it changed, same as [`Self::set`].
    pub fn mark_ready(&self, account_id: Uuid) -> Option<&'static str> {
        let mut inner = self.inner.lock().expect("sync health mutex poisoned");
        let previous = inner.label(account_id);
        inner.ready.insert(account_id);
        let new = inner.label(account_id);
        (new != previous).then_some(new)
    }

    /// The ADR 0030 readiness label for one account: `"connecting"` for an
    /// account this handle has never heard of (no supervised task has ever
    /// recorded a transition — e.g. a `deactivated` account, or one whose
    /// task hasn't started yet).
    pub fn sync_state(&self, account_id: Uuid) -> &'static str {
        self.inner
            .lock()
            .expect("sync health mutex poisoned")
            .label(account_id)
    }

    /// Drop an account's entry (on logout/delete) so `/v1/status` stops
    /// reporting an account that no longer exists.
    pub fn remove(&self, account_id: Uuid) {
        let mut inner = self.inner.lock().expect("sync health mutex poisoned");
        inner.status.remove(&account_id);
        inner.ready.remove(&account_id);
    }

    /// A snapshot of every currently-known account's *raw* SDK-reported
    /// status, for `GET /v1/status`. Never includes an account whose only
    /// signal is [`Self::mark_ready`] — see the module docs.
    pub fn snapshot(&self) -> Vec<(Uuid, AccountSyncStatus)> {
        self.inner
            .lock()
            .expect("sync health mutex poisoned")
            .status
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

    #[test]
    fn unknown_account_reports_connecting() {
        let health = SyncHealth::new();
        assert_eq!(health.sync_state(Uuid::new_v4()), "connecting");
    }

    #[test]
    fn running_before_first_sync_cycle_is_syncing() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();

        let transition = health.set(account_id, &State::Running);

        assert_eq!(transition, Some("syncing"));
        assert_eq!(health.sync_state(account_id), "syncing");
    }

    #[test]
    fn mark_ready_flips_syncing_to_ready() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();
        health.set(account_id, &State::Running);

        let transition = health.mark_ready(account_id);

        assert_eq!(transition, Some("ready"));
        assert_eq!(health.sync_state(account_id), "ready");
    }

    #[test]
    fn mark_ready_is_idempotent() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();
        health.set(account_id, &State::Running);
        health.mark_ready(account_id);

        let transition = health.mark_ready(account_id);

        assert_eq!(transition, None);
        assert_eq!(health.sync_state(account_id), "ready");
    }

    #[test]
    fn offline_blip_does_not_clear_readiness() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();
        health.set(account_id, &State::Running);
        health.mark_ready(account_id);

        let to_offline = health.set(account_id, &State::Offline);
        assert_eq!(to_offline, Some("offline"));

        let back_online = health.set(account_id, &State::Idle);
        assert_eq!(back_online, Some("ready"));
        assert_eq!(health.sync_state(account_id), "ready");
    }

    #[test]
    fn set_error_clears_readiness_for_the_next_run() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();
        health.set(account_id, &State::Running);
        health.mark_ready(account_id);

        let transition = health.set_error(account_id);
        assert_eq!(transition, Some("connecting"));
        assert_eq!(health.sync_state(account_id), "connecting");

        // The restarted run starts syncing (not ready) again.
        let restarted = health.set(account_id, &State::Running);
        assert_eq!(restarted, Some("syncing"));
    }

    #[test]
    fn error_and_terminated_both_report_connecting() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();

        health.set(account_id, &State::Terminated);
        assert_eq!(health.sync_state(account_id), "connecting");
    }

    #[test]
    fn repeated_set_with_no_label_change_returns_none() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();
        health.set(account_id, &State::Running);

        let transition = health.set(account_id, &State::Idle);

        assert_eq!(transition, None);
    }

    /// The regression this module's split (`status` map vs `ready` set)
    /// exists to prevent: `mark_ready` firing before this account has ever
    /// recorded a raw `SyncService::State` (the room-list-service race in
    /// `engine.rs` — its subscriber can observe `Running` before the outer
    /// `SyncService` subscriber observes anything at all) must not fabricate
    /// an entry in `snapshot()`, since that feeds the operator-facing
    /// `GET /v1/status` surface.
    #[test]
    fn mark_ready_before_any_raw_state_does_not_fabricate_one() {
        let health = SyncHealth::new();
        let account_id = Uuid::new_v4();

        let transition = health.mark_ready(account_id);

        assert_eq!(transition, Some("ready"));
        assert_eq!(health.sync_state(account_id), "ready");
        // No raw `SyncService::State` was ever reported for this account, so
        // /v1/status's view of it must still be empty.
        assert!(health.snapshot().is_empty());
    }
}
