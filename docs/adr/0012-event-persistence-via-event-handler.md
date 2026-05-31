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

- `raw_content` in the `events` table stores the *decrypted* event JSON for
  E2EE rooms (not the ciphertext). This is possible because the SDK decrypts
  Megolm payloads internally before dispatching to handlers: when a sender
  encrypts a message, their client distributes the Megolm session key to every
  device currently in the room (including axon) via an `m.room_key` to-device
  event, so axon already holds the key by the time the encrypted event arrives.
  The SDK decrypts silently and gives handlers the plaintext. Historical messages
  (sent before axon's device existed) are the exception — no `m.room_key` was
  ever sent to axon for those sessions, so they arrive as UTDs (see next bullet).
  Ciphertext and Megolm session metadata live in the SDK's SQLite store (per ADR
  0007). M4 will add sibling tables for the original ciphertext if needed.
- UTD events are persisted with `content = NULL` and `event_type =
  m.room.encrypted`. The M3c re-decryption queue will back-fill `content` as
  keys arrive.
- The handler is registered per `run_account` call (i.e., on every supervised
  restart). `SyncService::builder` takes ownership of the `Client`, which
  carries the registered handlers; when the service is stopped and dropped the
  handlers are cleaned up with it.
