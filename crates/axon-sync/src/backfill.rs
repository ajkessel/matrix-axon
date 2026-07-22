//! History backfill engine (M10). See ADR 0043.
//!
//! A continuous, throttled, low-priority background task — one per account,
//! spawned alongside the re-decryption queue in
//! [`run_account`](crate::engine) — that pages each *joined* room's pre-existing
//! history backward through `/messages` and persists every event through the same
//! ingestion path as live sync
//! ([`persist_backfilled_event`](crate::engine::persist_backfilled_event)). So
//! hot columns, crypto siblings, redaction handling, the M8 aggregation indexes,
//! and the M9 search index all apply uniformly, and re-runs are idempotent
//! (`upsert_event` is `ON CONFLICT DO NOTHING`).
//!
//! Paging issues the `/messages` request directly rather than calling
//! [`Room::messages`](matrix_sdk::Room::messages), which additionally writes every
//! fetched event into the SDK's own event cache — a store axon never reads, since
//! Postgres holds the authoritative copy. Doing so duplicated all of history on
//! disk: on one live account, 390k orphaned rows / 1.8 GB of SQLite shadowing a
//! 728 MB `events` table. [`fetch_page`] replicates what `Room::messages` does
//! (send, then decrypt each event via the same [`matrix_sdk::Room::decrypt_event`]
//! the re-decryption queue uses) minus that cache write. See issue #287.
//!
//! It is *not* a one-shot boot sweep: the task lives for the whole supervised run
//! and re-polls `joined_rooms()` when idle, so a newly joined or re-joined room is
//! picked up without a restart. Per-room progress is persisted after every page
//! (`room_backfill`), so backfill resumes where it left off across restarts.
//!
//! A disk-space safety valve ([`BackfillHealth`]) pauses paging when free space on
//! the guarded filesystem (`sync.backfill_disk_guard_path`, default the sync data
//! dir) is low — backfill grows storage unbounded ("to room start"), so it must
//! not fill the disk. Live sync is unaffected; only backfill pauses.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axon_core::SyncConfig;
use axon_store::RoomBackfillState;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::ruma::api::client::message::get_message_events;
use matrix_sdk::ruma::events::{
    AnySyncMessageLikeEvent, AnySyncTimelineEvent, AnyTimelineEvent, SyncMessageLikeEvent,
};
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::Client;
use tokio_util::sync::CancellationToken;

use crate::engine::{persist_backfilled_event, PersistContext};

/// The filesystem the disk-space valve guards: the configured override, else the
/// sync data dir. Shared by the valve (`disk_ok`) and the live `GET /v1/status`
/// free-space read ([`BackfillHealth::free_bytes`]).
pub(crate) fn guard_path(config: &SyncConfig) -> PathBuf {
    config
        .backfill_disk_guard_path
        .clone()
        .unwrap_or_else(|| config.data_dir.clone())
}

/// Consecutive `/messages` failures for a room before backfill falls back to
/// re-paging it from the live timeline end — recovering a room whose persisted
/// resume token has become permanently invalid instead of stalling it forever.
const MAX_TOKEN_RETRIES: u32 = 3;

/// The result of attempting to page one room once.
enum PageOutcome {
    /// Fetched and persisted at least one event — real backward progress.
    Paged,
    /// Nothing to do this sweep: the room is complete or at its depth cap.
    Idle,
    /// A read / pagination / save error; retry the room next sweep.
    Failed,
}

/// Shared, cheaply-cloneable view of the backfill disk-space valve: whether it has
/// paused paging, and (computed live) how much free space is on the guarded
/// filesystem. Held by the engine and read by the API status surface; the paused
/// flag is written by the backfill task.
#[derive(Clone, Default)]
pub struct BackfillHealth {
    inner: Arc<BackfillHealthInner>,
}

#[derive(Default)]
struct BackfillHealthInner {
    paused_low_disk: AtomicBool,
    /// The filesystem whose free space is reported at `GET /v1/status`. `None`
    /// (test/default handles) reports 0 free.
    guard_path: Option<PathBuf>,
}

impl BackfillHealth {
    /// A handle reporting free space on `guard_path` (the valve's guarded
    /// filesystem). `None` — used by tests and the no-backfill default — reports 0.
    pub fn new(guard_path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(BackfillHealthInner {
                paused_low_disk: AtomicBool::new(false),
                guard_path,
            }),
        }
    }

    /// Whether backfill is currently paused because free disk space is low.
    pub fn paused_low_disk(&self) -> bool {
        self.inner.paused_low_disk.load(Ordering::Relaxed)
    }

    /// Free bytes on the guarded filesystem, read **live** so `GET /v1/status` is
    /// accurate even when no backfill sweep has run recently (idle, disabled, or an
    /// account with no rooms). `0` if the path is unset or can't be stat'd.
    pub fn free_bytes(&self) -> u64 {
        self.inner
            .guard_path
            .as_ref()
            .and_then(|p| fs4::statvfs(p).ok())
            .map(|s| s.available_space())
            .unwrap_or(0)
    }

    /// Set the paused flag, returning the previous value so the caller can detect
    /// (and log) a transition.
    fn set_paused(&self, paused: bool) -> bool {
        self.inner.paused_low_disk.swap(paused, Ordering::Relaxed)
    }
}

/// Runtime knobs for the backfill task, derived from [`SyncConfig`].
#[derive(Clone)]
pub(crate) struct BackfillParams {
    page_size: u32,
    /// Per-room event cap (0 = unbounded / "to room start").
    target_depth: u64,
    throttle: Duration,
    /// Per-request timeout for one `/messages` page.
    page_timeout: Duration,
    idle_poll: Duration,
    min_free_bytes: u64,
    min_free_percent: f64,
    /// Filesystem whose free space gates backfill (the sync data dir).
    guard_path: PathBuf,
}

impl BackfillParams {
    pub(crate) fn from_config(config: &SyncConfig) -> Self {
        Self {
            page_size: config.backfill_page_size.max(1),
            target_depth: config.backfill_target_depth,
            throttle: Duration::from_millis(config.backfill_throttle_ms),
            page_timeout: Duration::from_secs(config.backfill_page_timeout_secs.max(1)),
            idle_poll: Duration::from_secs(config.backfill_idle_poll_secs.max(1)),
            min_free_bytes: config.backfill_min_free_bytes,
            min_free_percent: config.backfill_min_free_percent,
            guard_path: guard_path(config),
        }
    }
}

/// Whether there is enough free disk to keep backfilling, recording the result
/// (and logging pause/resume transitions) on `health`. A stat failure is treated
/// as "ok" (don't wedge backfill on an unreadable path) but logged.
fn disk_ok(params: &BackfillParams, health: &BackfillHealth, account_id: uuid::Uuid) -> bool {
    let (paused, free) = match fs4::statvfs(&params.guard_path) {
        Ok(stats) => {
            let free = stats.available_space();
            let total = stats.total_space();
            let pct = if total > 0 {
                (free as f64 / total as f64) * 100.0
            } else {
                100.0
            };
            let low = free < params.min_free_bytes || pct < params.min_free_percent;
            (low, free)
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                path = %params.guard_path.display(),
                error = %err,
                "backfill disk-space check failed; proceeding"
            );
            // Don't wedge backfill on an unreadable path. `free` here is only for
            // the transition log below; the live `GET /v1/status` free-space read
            // is separate.
            (false, 0)
        }
    };
    let was_paused = health.set_paused(paused);
    if was_paused != paused {
        if paused {
            tracing::warn!(account_id = %account_id, free_bytes = free, "backfill paused: low disk space");
        } else {
            tracing::info!(account_id = %account_id, free_bytes = free, "backfill resuming: disk space recovered");
        }
    }
    !paused
}

/// Run the backfill engine for one account until `cancel` fires.
pub(crate) async fn run(
    client: Client,
    ctx: PersistContext,
    params: BackfillParams,
    health: BackfillHealth,
    cancel: CancellationToken,
) {
    let account_id = ctx.account_id;
    tracing::debug!(account_id = %account_id, "history backfill task started");

    // Per-room count of consecutive `/messages` failures, so a permanently
    // invalid resume token eventually triggers a from-the-start retry
    // (`MAX_TOKEN_RETRIES`) instead of stalling that room forever.
    let mut failures: HashMap<String, u32> = HashMap::new();

    loop {
        if cancel.is_cancelled() {
            return;
        }

        // Load every room's cursor once per sweep (one query) rather than a lookup
        // per room, so a large account doesn't hammer the DB each pass.
        let cursors = match ctx.store.list_room_backfill(account_id).await {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(account_id = %account_id, error = %err, "backfill: list cursors failed");
                HashMap::new()
            }
        };

        let mut progressed = false;
        for room in client.joined_rooms() {
            if cancel.is_cancelled() {
                return;
            }
            let room_id = room.room_id().as_str().to_owned();

            // Skip already-complete rooms without any per-room work — the common
            // steady state on an established account, and what keeps a mostly-done
            // account from re-touching every room each sweep.
            if cursors.get(&room_id).is_some_and(|s| s.complete) {
                continue;
            }
            // Disk guard checked per active room (covers the first too, so there is
            // no redundant pre-loop check): if low, abandon the sweep and fall
            // through to the idle wait below, which re-checks next round.
            if !disk_ok(&params, &health, account_id) {
                break;
            }

            let force_from_start =
                failures.get(&room_id).copied().unwrap_or(0) >= MAX_TOKEN_RETRIES;
            let state = cursors.get(&room_id).cloned();
            match backfill_room_page(&ctx, &params, &room, state, force_from_start, &cancel).await {
                PageOutcome::Paged => {
                    progressed = true;
                    failures.remove(&room_id);
                    // Throttle *only* after real work, so completed/capped rooms
                    // don't add dead time that starves an actively-backfilling room
                    // on an account with many finished rooms.
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(params.throttle) => {}
                    }
                }
                PageOutcome::Idle => {
                    failures.remove(&room_id);
                }
                PageOutcome::Failed => {
                    *failures.entry(room_id).or_default() += 1;
                }
            }
        }

        // Nothing advanced this pass (every room complete/capped, paused, or
        // failing): idle, then re-poll — this is how newly joined/re-joined rooms
        // are picked up without a restart, and it caps the cursor-read cadence for
        // a fully-backfilled account at one sweep per `idle_poll`.
        if !progressed {
            tokio::select! {
                _ = cancel.cancelled() => return,
                _ = tokio::time::sleep(params.idle_poll) => {}
            }
        }
    }
}

/// Decrypt one paged event into the [`TimelineEvent`] the ingestion path expects.
///
/// Mirrors the SDK's own `Room::try_decrypt_event`, which `Room::messages` applies
/// to every fetched event, with two deliberate differences: no push context (axon
/// never reads `push_actions`, so computing them is pure waste — the re-decryption
/// queue passes `None` for the same reason), and no event-cache write.
///
/// An event whose keys haven't arrived yet comes back as a UTD `TimelineEvent`
/// rather than an error, exactly as on the live path; the re-decryption queue
/// back-fills it once keys arrive.
async fn decrypt_paged_event(room: &matrix_sdk::Room, raw: Raw<AnyTimelineEvent>) -> TimelineEvent {
    if needs_decryption(&raw) {
        if let Ok(event) = room.decrypt_event(raw.cast_ref_unchecked(), None).await {
            return event;
        }
    }
    TimelineEvent::from_plaintext(raw.cast())
}

/// Whether a paged event is an original `m.room.encrypted` message-like event, and
/// so the one shape [`matrix_sdk::Room::decrypt_event`] accepts.
///
/// Split out from [`decrypt_paged_event`] because it is the classification the
/// bypass has to get right — everything else in the decrypt path is delegated to
/// the SDK. Anything else (plaintext, state, redacted) is passed through
/// undecrypted, matching the SDK's own `try_decrypt_event`.
fn needs_decryption(raw: &Raw<AnyTimelineEvent>) -> bool {
    matches!(
        raw.deserialize_as::<AnySyncTimelineEvent>(),
        Ok(AnySyncTimelineEvent::MessageLike(
            AnySyncMessageLikeEvent::RoomEncrypted(SyncMessageLikeEvent::Original(_))
        ))
    )
}

/// Fetch and decrypt one `/messages` page, returning the events and the resume
/// token for the next page (`None` once the room's history is exhausted).
///
/// This is `Room::messages` minus its event-cache write — see the module docs and
/// issue #287. Everything else matches: the same client send path (so retry and
/// rate-limit handling are unchanged) and the same per-event decryption.
async fn fetch_page(
    room: &matrix_sdk::Room,
    request: get_message_events::v3::Request,
) -> Result<(Vec<TimelineEvent>, Option<String>), matrix_sdk::Error> {
    let response = room.client().send(request).await?;

    let mut events = Vec::with_capacity(response.chunk.len());
    for raw in response.chunk {
        events.push(decrypt_paged_event(room, raw).await);
    }

    // `state` is ignored: the paged timeline events are what backfill persists, and
    // room state arrives through live sync.
    Ok((events, response.end))
}

/// Page one room one step older and persist the results. `force_from_start`
/// ignores the persisted resume token and re-pages from the live timeline end —
/// the recovery path for a room whose token has become permanently invalid.
async fn backfill_room_page(
    ctx: &PersistContext,
    params: &BackfillParams,
    room: &matrix_sdk::Room,
    state: Option<RoomBackfillState>,
    force_from_start: bool,
    cancel: &CancellationToken,
) -> PageOutcome {
    let account_id = ctx.account_id;
    let room_id = room.room_id().as_str().to_owned();

    let (from_token, complete, count) = match state {
        Some(s) => (s.oldest_seen_token, s.complete, s.events_backfilled),
        None => (None, false, 0),
    };
    if complete {
        return PageOutcome::Idle;
    }
    if params.target_depth != 0 && count as u64 >= params.target_depth {
        return PageOutcome::Idle;
    }

    let mut request = get_message_events::v3::Request::backward(room.room_id().to_owned());
    request.limit = params.page_size.into();
    // `force_from_start` re-pages from the timeline end (idempotent, via
    // `ON CONFLICT DO NOTHING`) to recover a stuck room; otherwise resume from the
    // saved token.
    request.from = if force_from_start { None } else { from_token };

    // The `/messages` call crosses the homeserver boundary, so bound it: race it
    // against a timeout *and* the cancellation token. Without this, a hung request
    // would keep the backfill task parked inside the await, and shutdown / logout /
    // delete — which cancel the token and then await this task's handle — would
    // stall until the request eventually resolved.
    let (events, end) = tokio::select! {
        biased;
        _ = cancel.cancelled() => return PageOutcome::Failed,
        result = tokio::time::timeout(params.page_timeout, fetch_page(room, request)) => match result {
            Ok(Ok(m)) => m,
            Ok(Err(err)) => {
                // Transient (network, rate limit) or a bad token; the caller counts
                // failures and eventually retries this room from the start.
                tracing::warn!(account_id = %account_id, room_id = %room_id, error = %err, "backfill: /messages page failed");
                return PageOutcome::Failed;
            }
            Err(_elapsed) => {
                tracing::warn!(account_id = %account_id, room_id = %room_id, timeout_secs = params.page_timeout.as_secs(), "backfill: /messages page timed out");
                return PageOutcome::Failed;
            }
        },
    };

    for tev in &events {
        if cancel.is_cancelled() {
            return PageOutcome::Failed;
        }
        persist_backfilled_event(ctx, room, tev).await;
    }

    let fetched = events.len();
    let complete = end.is_none();
    if let Err(err) = ctx
        .store
        .save_room_backfill(
            account_id,
            &room_id,
            end.as_deref(),
            complete,
            fetched as i64,
        )
        .await
    {
        // The events landed but the cursor didn't advance; report a failure so the
        // sweep doesn't count this as progress (it will re-page the same window
        // next sweep — idempotent for `events`).
        tracing::warn!(account_id = %account_id, room_id = %room_id, error = %err, "backfill: save cursor failed");
        return PageOutcome::Failed;
    }

    tracing::debug!(
        account_id = %account_id,
        room_id = %room_id,
        fetched,
        complete,
        "backfilled a page"
    );

    if fetched > 0 {
        PageOutcome::Paged
    } else {
        PageOutcome::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DECRYPTED_MESSAGE: &str =
        include_str!("../../../tests/fixtures/decryption/decrypted_message_event.json");
    const UTD_ENCRYPTED: &str =
        include_str!("../../../tests/fixtures/decryption/utd_encrypted_event.json");

    fn raw(json: &str) -> Raw<AnyTimelineEvent> {
        serde_json::from_str(json).expect("fixture")
    }

    #[test]
    fn encrypted_event_is_routed_to_decryption() {
        assert!(needs_decryption(&raw(UTD_ENCRYPTED)));
    }

    #[test]
    fn plaintext_event_is_passed_through() {
        // The common case on an unencrypted room: no olm round-trip, straight to
        // `TimelineEvent::from_plaintext`.
        assert!(!needs_decryption(&raw(DECRYPTED_MESSAGE)));
    }

    #[test]
    fn redacted_encrypted_event_is_passed_through() {
        // A redacted `m.room.encrypted` has no ciphertext left to decrypt, so it
        // must take the plaintext arm rather than a doomed `decrypt_event` call.
        let redacted = json!({
            "type": "m.room.encrypted",
            "sender": "@alice:example.org",
            "event_id": "$redacted1:example.org",
            "origin_server_ts": 1_700_000_000_000_u64,
            "content": {},
            "unsigned": {
                "redacted_because": {
                    "type": "m.room.redaction",
                    "sender": "@alice:example.org",
                    "event_id": "$redaction1:example.org",
                    "origin_server_ts": 1_700_000_000_001_u64,
                    "content": {}
                }
            }
        });
        let raw: Raw<AnyTimelineEvent> = serde_json::from_value(redacted).expect("raw");
        assert!(!needs_decryption(&raw));
    }

    #[test]
    fn state_event_is_passed_through() {
        // Backfill pages state events too; only message-like encrypted events are
        // decryptable, so a state event must never reach `decrypt_event`.
        let raw: Raw<AnyTimelineEvent> = serde_json::from_value(json!({
            "type": "m.room.topic",
            "sender": "@alice:example.org",
            "event_id": "$topic1:example.org",
            "origin_server_ts": 1_700_000_000_000_u64,
            "state_key": "",
            "content": { "topic": "hello" }
        }))
        .expect("raw");
        assert!(!needs_decryption(&raw));
    }

    #[test]
    fn malformed_event_is_passed_through() {
        // Undeserializable JSON must not be mistaken for an encrypted event; it
        // falls through to `from_plaintext`, where the ingestion path logs and
        // skips it.
        let raw: Raw<AnyTimelineEvent> =
            serde_json::from_value(json!({ "nonsense": true })).expect("raw");
        assert!(!needs_decryption(&raw));
    }
}
