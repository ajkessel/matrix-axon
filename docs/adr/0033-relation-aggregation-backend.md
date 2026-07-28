# ADR 0033 — Relation aggregation: read-time resolution in the store

## Context

Matrix encodes edits, reactions, threads, and replies as *relation* events.
Axon already persists the whole `m.relates_to` block in the `events.relates_to`
JSONB hot column (ADR 0015), generically, for every event — but until M8 the API
served those relations raw. Every client had to re-aggregate over whatever
timeline window it held, which silently dropped any relation that landed
*outside* that window: a reaction or edit to a message older than the loaded
slice never showed up (GH issue #22, hit by `axon-tui`). M8 moves aggregation
server-side. It also subsumes the old standalone Threads milestone — a thread is
just the `m.thread` case of the same machinery (ADR 0017's deferral cashed in).

This ADR records the **backend** (8a) decisions: the indexes, where resolution
lives, and the rules the resolver enforces. The API surface (8b) is a separate
layer over these store reads.

Two relation shapes coexist and trip people up:

- `rel_type` relations — edits (`m.replace`), reactions (`m.annotation`), thread
  members (`m.thread`) — carry `relates_to.rel_type` + `relates_to.event_id`.
- **replies carry no `rel_type`**; the target nests under
  `relates_to.m.in_reply_to.event_id`.

## Decision

### Computed at read time, not materialized

Aggregation is resolved on read via SQL over indexed `relates_to`, **not**
maintained as tallies on ingest. The indexes make "all relations pointing at X"
an index lookup, which is cheap at the MVP ("Riley") scale; incremental
materialization is a later optimization, not a re-architecture. This keeps the
write path untouched (append-mostly) and keeps the resolution rules in one place
where they're testable — the same philosophy as read-time redaction masking
(ADR 0015): the raw edit/reaction/redaction events stay on disk, and the read
folds them.

### Two indexes, because one relation shape is otherwise unfindable

`20260620120000_relation_aggregation_indexes.sql` adds, both partial and
account-scoped, both retroactive to already-stored rows (additive, backfill-free):

- `events_relation_target_idx` on `(account_id, relates_to->>'rel_type',
  relates_to->>'event_id')` — serves edits/reactions/threads, generalizing the
  thread index sketched in ADR 0017.
- `events_reply_target_idx` on
  `(account_id, relates_to->'m.in_reply_to'->>'event_id')` — replies have no
  `rel_type`, so the first index never sees them; this is the only way "replies
  to X" is an index lookup.

### Aggregation rides the shared timeline projection

The existing `TIMELINE_SELECT` (which already did read-time redaction masking via
a `LEFT JOIN LATERAL`) grew two more LATERALs — latest-valid-edit and a
per-emoji reaction tally — plus an `accounts` join (for the reaction `me` flag).
So `room_timeline`, `get_event`, `event_replies`, and `thread_timeline` all
return the same resolved `TimelineRow` (now with `edited` / `edit_count` /
`latest_edit_ts` / `reactions`). A single projection means one set of rules and
one `FromRow`, and a reply or thread member is itself aggregated (its own edits
and reactions resolve) for free.

### Resolution rules the homeserver doesn't enforce

Aggregating "all relations by target" is necessary but not sufficient — the
resolver enforces validity (ADR 0021 noted edit authorship is unenforced
upstream):

- **Edit authorship + type.** An `m.replace` is honored only when its `sender`
  equals the *original* event's sender (an edit from anyone else is dropped — the
  impersonation guard), it is well-formed (`m.new_content` present), its
  replacement `msgtype` matches the target's, and (MSC2676) it shares the
  target's `event_type`, neither edit nor target is a state event, and the target
  is not itself an `m.replace`. `edit_count` counts only valid edits, and is
  zeroed when the target is redacted (it must not leak a count behind a masked
  row).
- **Relations are room-local.** Every target-based lookup (edit and reaction
  LATERALs, plus `event_edits` / `event_reactions` / `event_replies`) also
  requires the relation to be in the *same room* as its target, so an edit or
  reaction stored in room B can't resolve onto a target in room A.
- **Reactions are emoji-only for now.** The reaction tally is restricted to the
  `m.reaction` event type. Matrix permits any event type to use `m.annotation`
  and distinguishes aggregation by `(event_type, key)`; grouping by `key` alone
  would merge unrelated bot/voting/moderation annotations. Generic
  `(event_type, key)` annotation aggregation is tracked separately (GH issue
  #112).
- **Redaction precedence.** A redacted edit stops contributing (the latest
  *non-redacted* edit wins; if all edits are redacted the body reverts to the
  original). A redacted reaction drops from the tally. **A redacted target wins
  over its edits** — the projection masks a redacted row even if a valid edit
  exists, so a redaction can't be "un-redacted" by a later edit. Redacted
  replies / thread members / thread roots are still counted for *structure*
  (membership, reply count) but presented masked, so a redacted root doesn't make
  its thread vanish.
- **Deterministic latest edit.** Resolved by `origin_ts DESC`, tie-broken by the
  monotonic store `id DESC` (then `event_id`), so the winner is deterministic
  when timestamps collide — never "whichever the query returned." `COUNT(*) OVER
  ()` carries the valid-edit count alongside the `LIMIT 1` winner in one pass.
- **Reaction dedup.** The same `(sender, key)` counts once via `COUNT(DISTINCT
  sender)` within each key group; `me` is `bool_or(sender = <account user_id>)`.
- **Replies vs threads.** `event_replies` matches the nested `m.in_reply_to`
  target **and** requires `rel_type IS NULL`, so a thread member (which also
  nests an `m.in_reply_to` fallback) is not double-counted as a plain reply.

### The room timeline collapses standalone relation events

`room_timeline` excludes the standalone relation events now surfaced on their
target row: every `m.replace` (edited body shown in place), and `m.annotation`
**only when it is an `m.reaction`** (folded into the `reactions` summary). Because
aggregation is restricted to `m.reaction`, a non-`m.reaction` annotation (e.g. a
custom `com.example.approval`) is neither aggregated nor collapsed — it stays
visible as a raw timeline row until generic `(event_type, key)` aggregation lands
(GH issue #112); collapsing it would make it disappear entirely. Replies and
thread members keep their own rows. The forensic edit trail stays on disk and is
exposed separately (the 8b `…/edits` endpoint). `COALESCE(rel_type, '')` keeps
NULL-`rel_type` rows (ordinary messages, replies) visible.

## Consequences

- The full resolved view is served regardless of pagination — issue #22 is fixed
  at the store, not per-client.
- It applies retroactively to rows stored before M8 and automatically to the deep
  history M10 backfills in later — no re-sync, no data migration.
- Two LATERAL subqueries run per timeline row (≤200/page); acceptable at MVP
  scale given the indexes, and the explicit upgrade path is incremental
  materialization without changing the rules.
- M9 search indexes the *latest edited* body because aggregation lands first
  (the resequencing rationale in `implementation.md`).
