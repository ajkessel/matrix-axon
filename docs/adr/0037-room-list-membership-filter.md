# ADR 0037 — Room list excludes rooms the local user has left

## Context

`GET /v1/rooms` is backed by `Store::list_rooms` (`axon-store/src/rooms.rs`),
which derives the room set by aggregating the append-only `events` table: any
`(account_id, room_id)` pair that has stored events becomes a row in the list.
There is no `rooms` table and no membership check (ADR 0019, ADR 0033 describe
the surrounding read model).

That derivation has no notion of *leaving*. When the local user leaves (or is
kicked/banned from) a room, the leave is an ordinary `m.room.member` event: it is
ingested into `events` and projected into `room_state` like any other state
event. But because the room still has events on disk, it keeps appearing in the
list — even though every other Matrix client has dropped it, because those
clients filter on membership. This surfaced as stale temporary rooms left behind
by SAS-verification troubleshooting (GH issue #122) but applies to any left or
banned room.

The fix had two candidate shapes (from the issue):

1. **Filter at read time** in `list_rooms` against the `room_state` membership
   projection — non-destructive, the raw events stay on disk.
2. **Purge on leave** — a sync-engine handler that deletes the room's `events`
   and `room_state` rows when the local user leaves.

## Decision

### Filter at read time, in the query (option 1)

`list_rooms` gains a correlated `NOT EXISTS` against `room_state` that drops a
room when the local user's current `m.room.member` membership is `leave` or
`ban`:

```sql
WHERE NOT EXISTS (
    SELECT 1 FROM room_state rs
    WHERE rs.account_id = a.account_id AND rs.room_id = a.room_id
      AND rs.event_type = 'm.room.member' AND rs.state_key = ac.user_id
      AND rs.content->>'membership' IN ('leave', 'ban')
)
```

`state_key = ac.user_id` ties the membership to the *local* account (the
`accounts` join already supplies `user_id` for the `account_user_id` field), so
another member leaving never hides the room. `room_state` already holds the
current value of each `(type, state_key)` tuple (ADR 0016) and is kept current by
the existing state-event handler (`persist_room_state_event` in
`axon-sync/src/engine.rs`) — the leave event updates the local user's member row
to `membership: "leave"` as a side effect of normal sync, so no new write path or
leave-specific handler is needed. This is the same read-time-resolution
philosophy the store already uses for redaction masking and relation aggregation
(ADR 0033): the raw events stay on disk and the read folds them.

### Exclude `leave`/`ban`, rather than require `join`/`invite`

The issue sketched the inverse predicate — keep only rooms whose membership is
`IN ('join', 'invite')`. We deliberately invert it: **hide only on a definitive
`leave`/`ban` signal**, keep everything else. The reason is robustness against a
*missing* member row. `room_state` is populated by sync handlers and is not
guaranteed to carry a local-user `m.room.member` row for every room that has
events (e.g. rows ingested before the room-state projection existed, or via
paths that wrote `events` without a corresponding membership upsert). An
inclusion predicate would silently drop those legitimately-joined rooms from the
list — a worse regression than the bug being fixed. The exclusion predicate
changes the list only for rooms with an explicit leave/ban, which is exactly the
reported case, and leaves `join`/`invite`/`knock`/membership-unknown rooms
visible as before.

### Not option 2 (purge on leave)

Deleting a room's history on leave is destructive and hard to reverse: re-joining
a room would lose its locally-stored timeline, search index entries, and key
material, and it interacts badly with the redecryption queue and account-deletion
teardown (ADR 0024). It maps to Matrix `forget` semantics, which is a distinct,
explicit user action — not the implicit consequence of a leave. If a `forget`
endpoint is wanted later it can be built deliberately; the read-time filter does
not foreclose it and is the conservative first step the issue recommends.

## Consequences

- Left/banned rooms disappear from `GET /v1/rooms` with no data loss; their
  events and state remain on disk and a re-join makes the room reappear (its
  membership row flips back to `join`).
- A room with no local-user `m.room.member` row in `room_state` still appears —
  current behavior is preserved for rooms that lack membership data.
- The filter is a correlated `NOT EXISTS` keyed on the `room_state` primary key
  `(account_id, room_id, event_type, state_key)`, so it is a single index lookup
  per room, not a scan.
- The room *timeline* endpoint is unchanged: a left room's history is still
  directly readable by id. Only its presence in the derived list is affected.
- No migration: the predicate reads existing `room_state` rows and applies
  retroactively to already-left rooms.
