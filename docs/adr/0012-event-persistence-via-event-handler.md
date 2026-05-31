# ADR 0012: Event persistence via `add_event_handler`

## Status

Accepted — Milestone 3b.

## Context

Milestone 3b adds an `events` table to Postgres and wires the sync engine to
populate it. Two approaches were considered for receiving events from
matrix-rust-sdk:

**Option A: `Client::add_event_handler`**
Register a single handler for `AnySyncTimelineEvent` on the per-account
`Client` before starting the `SyncService`. The SDK dispatches every timeline
event (including E2EE events, after automatic Megolm decryption) to registered
handlers, passing the typed event, the `Room`, and a `RawEvent` context
argument (the full event JSON as seen post-decryption).

**Option B: Per-room `Room::timeline()` + `Timeline::subscribe()`**
After sync starts, obtain the `RoomListService` from `SyncService`, subscribe
to room-list updates, and for each active room open a `Timeline` subscription
to receive `TimelineItem` events.

## Decision

Option A (`add_event_handler`).

Reasons:
- **Simpler supervision.** One handler registered once per account; no
  per-room task management or fan-out logic needed.
- **No missed events on first sync.** Registering the handler *before*
  `SyncService::start()` ensures the SDK delivers every event, including those
  that arrive during the initial sync before rooms appear in the room list.
- **All event types for free.** `AnySyncTimelineEvent` covers message-like
  and state events without explicitly enumerating types; new Matrix event types
  from the homeserver are automatically persisted.
- **E2EE transparent.** The SDK decrypts Megolm payloads before dispatching,
  so `persist_timeline_event` always sees plaintext content (or
  `m.room.encrypted` for UTDs, which are persisted with `content = NULL` and
  upgraded by the M3c re-decryption queue).

Option B is the right foundation for *live-streaming* timeline updates to API
clients (Milestone 5), where per-room pagination state and real-time push
semantics matter. It is overkill for the batch-write use case here.

## Consequences

- `raw_event` in the `events` table stores the **full event envelope** as the
  SDK dispatched it (type, sender, `content`, unsigned, …), *not* the ciphertext
  specifically. What it contains depends on the decryption path:
  - **Live-decrypted message** → plaintext envelope. The SDK decrypts Megolm
    internally before dispatch: when a sender encrypts a message, their client
    distributes the Megolm session key to every device then in the room
    (including axon) via an `m.room_key` to-device event, so axon holds the key
    by the time the event arrives and the handler sees plaintext.
  - **UTD** (historical message, or any session axon never received a key for)
    → the `m.room.encrypted` envelope, i.e. ciphertext + `session_id`.
  The companion `content` column is the *extracted, decrypted* payload: the
  event's `content` field for decrypted events, `NULL` for UTDs.
- UTD events are persisted with `content = NULL` and `event_type =
  m.room.encrypted`; their ciphertext and `session_id` are already in
  `raw_event`. The M3c re-decryption queue reads the ciphertext straight from
  `raw_event` and back-fills `content` as keys arrive — no separate ciphertext
  store is needed for re-decryption.
- The one thing **not** retained in Postgres is the ciphertext of events the SDK
  decrypted *inline* (live messages): for those, `raw_event` holds plaintext and
  the original ciphertext lives only in the SDK's SQLite store (per ADR 0007).
  Nothing in the MVP needs that ciphertext-at-rest; if a future audit/forensics
  requirement ever does, it can be captured then. We do not speculatively add a
  sibling ciphertext table now.
- The handler is registered per `run_account` call (i.e., on every supervised
  restart). `SyncService::builder` takes ownership of the `Client`, which
  carries the registered handlers; when the service is stopped and dropped the
  handlers are cleaned up with it.
