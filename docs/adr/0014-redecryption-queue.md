# ADR 0014: Re-decryption queue

## Status

Accepted — Milestone 3c.

## Context

A fresh `axon` device is unverified, so it has no megolm keys for messages sent
before it joined. Per ADR 0012, 3b persists those undecryptable events (UTDs)
as `m.room.encrypted` rows with `content = NULL`, retaining the ciphertext and
its megolm `session_id` in `raw_event`. 3c makes those rows recoverable: when
the keys later arrive, find the matching rows, decrypt, and back-fill `content`
plus the real `event_type`.

Three sub-decisions had to be made: **how to find pending rows for an arriving
session**, **what drives the queue**, and **how this fresh device gets any keys
at all** so the queue is actually exercisable end-to-end.

## Decision

### 1. A first-class `megolm_session_id` hot column + partial index

A new forward-only migration adds `events.megolm_session_id TEXT`, populated from
`content.session_id` for UTDs (NULL otherwise), with a partial index:

```sql
CREATE INDEX events_pending_utd_idx
  ON events (account_id, room_id, megolm_session_id)
  WHERE content IS NULL;
```

The arriving-key hot path — "given a `(room_id, session_id)`, find every pending
UTD waiting on it" — is then an index lookup, not a JSONB-expression scan over
`raw_event`. The partial predicate keeps the index covering only undecryptable
rows, which drain over time. This was chosen over (a) a JSONB expression index on
`raw_event->'content'->>'session_id'`, which keeps queries coupled to the raw
envelope shape, and (b) a sibling ciphertext table — that belongs to M4's event
store schema (ADR 0012), which holds session metadata for *every* event to keep
decrypted rows re-verifiable; M3c only needs to locate *undecryptable* rows and
already has the ciphertext in `raw_event`. Treating the session id as the
tech-spec's "megolm session metadata" promoted to a hot column matches how other
hot columns (`sender`, `origin_ts`, `event_type`) are handled.

### 2. Stream-driven, plus a startup sweep

Two drivers feed the queue (`crates/axon-sync/src/redecrypt.rs`):

- **Arrival stream.** `client.encryption().room_keys_received_stream()` yields a
  batch of `RoomKeyInfo { room_id, session_id, .. }` each time keys land. For
  each, we load the pending UTDs for that session and decrypt them via
  `room.decrypt_event(&Raw<OriginalSyncRoomEncryptedEvent>, None)`.
- **Startup sweep.** Keys already in the SDK crypto store from a prior run, or
  imported by `recover()` *before* we subscribe, never fire the stream. So once
  the sync service is up we make one pass over all of the account's pending UTDs.
  The two drivers overlap deliberately — the sweep is the safety net for "keys
  arrived while we weren't listening." Crucially, the sweep first calls
  `backups().download_room_keys_for_room(room_id)` for each room it touches:
  `recover()` imports the backup *decryption key* but **not** the megolm room
  keys themselves (those download lazily), so on a quiet account no later event
  would ever trigger the fetch. Without this, the sweep finds the rows but can't
  decrypt them. The arrival-stream path skips the download — its keys just
  landed in the store.

`decrypt_event` returns a UTD `TimelineEvent` (not an error) when the key still
isn't available, so success is gated on `TimelineEventKind::Decrypted`. Every
per-row failure — malformed envelope, still-missing key, write error — is logged
and skipped; re-decryption is best-effort back-fill and never fails the sync
task. The back-fill `UPDATE … WHERE content IS NULL` guard makes it idempotent
and prevents clobbering a row a live dispatch already decrypted.

The queue runs as a child task of `run_account` on a child `CancellationToken`,
joined on return so it neither outlives nor duplicates across a supervised
restart. It lives in `axon-sync` (it is sync-pipeline back-fill tied to the
per-account `Client` and its supervised lifecycle); `axon-crypto` stays reserved
for the M4/M5 interactive-verification surface.

### 3. Transient `recover()` as the test driver; durable storage deferred

On a fresh unverified device, *no key source ever arrives* for historical UTDs,
so without a bootstrap the queue would have nothing to drain and be untestable
end-to-end. We therefore pull in a **transient-only** account recovery key
(`sync.account.recovery_key`, `#[serde(default)]`): on boot, if set, we call
`client.encryption().recovery().recover(key)` once, which imports the megolm key
backup + cross-signing keys and fires the arrival stream with the full backlog —
the natural, reproducible driver that flips historical UTD rows from NULL to
populated. A wrong or rotated key surfaces as a readable `tracing::error` (not a
silent permanent UTD) and is non-fatal: sync still runs.

Per ADR 0011, this is the *bootstrap/fallback* key-acquisition path. The key is
read from config, held only across the `recover()` call, and **never persisted**
— it is not part of `Credential`, and no recovery-key column exists on
`accounts`. Durable, encrypted-at-rest recovery-key storage and its lifecycle,
along with BFF-proxied interactive verification, remain Milestone 4/5.

## Consequences

- One new nullable column and one partial index on `events`; no backfill of
  existing rows needed (NULL is the correct value for already-decrypted events).
- The queue's correctness depends only on `raw_event` (UTD ciphertext +
  `session_id`) — it does not depend on the M4 sibling ciphertext tables.
- `recovery_key` is a new optional config knob. Until M4 it must be supplied on
  each boot that needs key recovery; once recovery has run and the SDK crypto
  store holds the keys, later boots decrypt from that store and the sweep alone
  suffices.
- Adds `futures-util` to `axon-sync` for `StreamExt::next` on the arrival stream.
- **Permanently-undecryptable rows accumulate.** A row whose key never arrives
  (withheld by the sender, absent from the key backup, or gated by history
  visibility) stays `content = NULL` forever. It therefore lives in the partial
  index `events_pending_utd_idx` indefinitely *and* is re-attempted by the
  startup sweep on every boot — so sweep cost grows with the all-time count of
  doomed UTDs, not the currently-decryptable backlog. The arrival stream still
  back-fills any row the moment its key actually lands, so this is a scaling
  cost, not a correctness gap. Bounding it (per-row attempt tracking / backoff,
  honoring `m.room_key.withheld` to stop retrying, active `request_room_key`
  re-requests, or aging rows out) is deferred future work, tracked in
  [issue #9](https://github.com/jamieforrest/matrix-axon/issues/9).
