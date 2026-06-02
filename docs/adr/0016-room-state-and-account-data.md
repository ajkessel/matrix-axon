# ADR 0016 — Room state and account data projections

## Context

M4a (ADR 0015) matured the `events` log. The other half of M4's store work
(`docs/mvp/implementation.md` §4) is the two *current-value* projections an
application read needs without folding over the event log:

- **Room state** — the latest value of each `(type, state_key)` state tuple per
  room: name, topic, avatar, canonical alias, join rules, and the membership
  list.
- **Account data** — both global (account-wide: push rules, `m.direct`, ignored
  users) and per-room (`m.fully_read` markers, `m.tag`), one current value per
  type per scope.

This ADR records the M4b decisions the spec does not pin down.

## Decision

### Projections, not logs

`room_state` and `account_data` hold the *resolved current value*, upserted in
place as syncs arrive — a read is a point lookup, not a fold over history. The
raw state events still land in `events` (state events are part of the Matrix
timeline), so nothing is lost; these tables are the derived current-value
projection a room-summary or read-marker read consults — maintained by hand on
each upsert, not a Postgres `MATERIALIZED VIEW`.

- `room_state` PK = the Matrix state identity `(account_id, room_id, event_type,
  state_key)`. `state_key` is `''` for singletons (`m.room.name`) and the target
  user id for `m.room.member`, exactly as Matrix defines it.
- `account_data` PK = `(account_id, room_id, event_type)`.

Both carry an `updated_at` maintained by the shared `trigger_set_updated_at()`
trigger introduced for `accounts`, so a reader can rely on it without the
application setting it on every write.

### Global account data uses a `''` room_id sentinel (not NULL)

Global account data has no room. The plan described uniqueness as
`(account_id, COALESCE(room_id,''), event_type)`. We implement that with
`room_id TEXT NOT NULL DEFAULT ''` rather than a nullable column + expression
index:

- A real Matrix room id always starts with `!`, so `''` is an unambiguous
  "global" sentinel.
- The natural PK then carries uniqueness directly, and `ON CONFLICT
  (account_id, room_id, event_type)` targets it cleanly.
- A **nullable** `room_id` would be the trap: under SQL semantics `NULL` is
  distinct from `NULL` in a unique index, so two global rows for one type would
  both be allowed and the upsert would silently duplicate instead of overwrite.

The store API speaks `room_id: Option<&str>` (None = global) and maps to/from
`''` at the boundary, so callers never see the sentinel.

### Freshness guard on room state; last-write-wins on account data

State events carry `origin_server_ts`. `upsert_room_state` guards the update
(`ON CONFLICT … DO UPDATE … WHERE EXCLUDED.origin_ts >= room_state.origin_ts`)
so an older, replayed state event can never clobber newer resolved state,
regardless of arrival order. Account-data events carry **no** timestamp, so
`upsert_account_data` is plain last-write-wins — the most recent sync is
authoritative.

### Three SDK handlers, reusing one context

Simplified Sliding Sync dispatches state and account-data events through the same
`add_event_handler` mechanism as timeline events (`call_sync_response_handlers`).
We register three handlers in `engine.rs`, all sharing the existing
`PersistContext`:

- `persist_state_event(AnySyncStateEvent, Room, RawEvent, Ctx)` → `room_state`.
- `persist_room_account_data(AnyRoomAccountDataEvent, Room, RawEvent, Ctx)` →
  `account_data` (room scope).
- `persist_global_account_data(AnyGlobalAccountDataEvent, RawEvent, Ctx)` →
  `account_data` (global scope).

The `Any*` enums have `SyncEvent::TYPE = None`, so each handler catches **all**
events of its kind. Identity fields (event_id/sender/origin_ts) come from the
typed event; `type`/`state_key`/`content` are read from the raw JSON so the exact
content (including unknown fields) is preserved — the same split
`persist_timeline_event` uses.

**The global handler deliberately omits the `Room` argument.** The SDK's `Room`
context extractor returns `None` for an event with no room and the handler is
then *skipped with a logged error* — so a global-account-data handler that asked
for a `Room` would never run. Room and global account data therefore need
separate handlers despite sharing a table.

Writes follow the established best-effort posture: a failure is logged and never
fails the sync task. State events are never encrypted, so unlike the M4a sibling
writes there is no UTD/decryption asymmetry here.

## Consequences

- Room summaries and read markers are O(1) point lookups; no timeline replay.
- A handler only fires for an event its `Any*` enum can deserialize — a wholly
  unknown *state* event type would be skipped (same limitation as the M4a
  timeline handler). The standard state types we care about all deserialize.
- `room_state.content` is nullable (redacted state events); `account_data.content`
  is NOT NULL (account-data events always carry content — a contentless one is
  skipped).
- No HTTP surface yet (that's M5); verified by `--ignored` store tests in
  `crates/axon-store/tests/state.rs`.
