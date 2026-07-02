# ADR 0044 — Left-room search semantics and purge-on-leave

## Context

The search index (M9) is not membership-aware: `GET /v1/search` returns hits from
rooms the user has left, because the events synced while a member remain in the
store and index after leaving. Backfill (M10, ADR 0043) makes this more visible by
pulling deep history into joined rooms — and, since backfill is joined-only, a
left room's already-stored content just lingers. We needed to decide what "leaving
a room" means for stored content and for search, without making search silently
resurface rooms a user has walked away from.

## Decision

Two mechanisms, one default-on and reversible, one opt-in and destructive.

### Default: query-time membership filter (non-destructive)

`GET /v1/search` hydrates each index hit through `Store::get_event_if_joined`,
which returns `None` when the local user has **left or been banned** from the
event's room — reusing the exact `NOT EXISTS(m.room.member = leave/ban)` predicate
`list_rooms` already applies (ADR 0037), correlated to the account's own
`user_id`. So left-room hits are dropped exactly like the existing index/DB race
drop, and paging (which advances by the pre-hydration hit count) is unaffected.

This is **reversible**: nothing is deleted, so re-joining a room makes its events
searchable again with no re-backfill. A room with no definitive leave/ban row
still hydrates, so missing membership data never hides a joined room's hits. This
is the default because most users do not expect search results from rooms they
have left.

### Opt-in: destructive purge-on-leave

With `sync.purge_on_leave = true` (default `false`), a leave/ban of the local user
— observed as an `m.room.member` state event on the ordinary sync path — triggers
`Store::purge_room`, which in one atomic statement deletes the room's `events`
(cascading to the crypto siblings), `room_state`, room-scoped `account_data`, and
`room_backfill` row, and appends a **room-scoped search-purge** obligation to
`search_outbox`. The search writer applies it by deleting every document for that
`(account_id, room_id)` — a small extension of the existing account-wide purge
machinery, using Tantivy's `delete_query` (a conjunction of the `account_id` and
`room_id` terms) so it never touches another account's copy of a shared room. The
obligation is engine-neutral and FK-free, so it survives to be applied even if the
indexer is offline, matching the account-purge design.

Purge is destructive: re-joining a purged room re-backfills it from scratch.

## Consequences

- Out of the box, left-room content stays on disk (cheap to keep, instantly
  restored on re-join) but never appears in search.
- Operators who want storage not to grow monotonically enable `purge_on_leave` and
  accept the destructive, non-reversible trade.
- The room-scoped search purge is keyed by `(account_id, room_id)` terms, so it is
  correct for an Axon hosting multiple accounts that share a room.
- The purge fires from the room-state event handler; the triggering `m.room.member`
  event may be (re)persisted by the concurrent timeline handler, leaving at most
  one bodyless membership row — harmless (no searchable body) and cleared by the
  next purge or account deletion.
