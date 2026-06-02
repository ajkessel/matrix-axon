# ADR 0018 — History backfill, folded into the search milestone

## Context

axon's archive is populated by the sync engine, which only ever sees events
*going forward*: live timeline events, plus the shallow per-room window set by
`sync.timeline_limit` on a room's first sync (ADR 0015). Nothing reaches back for
a room's pre-existing history. ADR 0015 named a "full history-backfill engine" as
"later work" but assigned it to no milestone.

That omission collides with the PRD:

- **Success criterion #2 — "Full-history search… p95 < 200ms."** The search index
  is populated on event ingestion, so it contains exactly what has been ingested.
  On a fresh install that is *not* a room's full upstream history, so
  "full-history search" is not literally achievable from sync alone.
- **The 100–200k-event working-set target** only materializes if something
  deliberately pulls that history down.

A subtlety that makes backfill a distinct piece of work: `recover()` (ADR
0011/0014) imports the *keys* to decrypt old messages, but not the *messages* —
those must still be fetched by paging each room's `/messages` endpoint. So
backfill is its own engine, not a side effect of key recovery.

## Decision

**History backfill is folded into the search milestone (Milestone 9), which is
split into two parts:**

- **9a — search ingestion & indexing** (the Tantivy index + `/v1/search`).
- **9b — history backfill** (the engine below).

This keeps backfill **in-MVP** and co-located with the criterion it serves:
"full-history search" needs both the index (9a) and the history (9b), so the two
belong in one milestone — the alpha should be able to search a room's full
history, not just the slice ingested since install. The split mirrors the M4a/M4b
pattern. (Threads are Milestone 13.)

Engine shape (9b):

- A bounded, **resumable** engine that pages backward through each room's
  timeline via the SDK's room pagination, decrypts with already-imported keys,
  and persists through the **same ingestion path as live sync** — so hot columns,
  crypto siblings, redaction handling, and the 9a index apply uniformly and
  re-runs are idempotent (`ON CONFLICT DO NOTHING`).
- Per-room backfill state (e.g. a `room_backfill` table keyed by
  `(account_id, room_id)` recording the oldest token reached and a `complete`
  flag) so progress survives restarts.
- Background and throttled, so it never starves live sync; configurable target
  depth.
- It retires the `sync.timeline_limit` bump as the "bounded substitute" for real
  backfill (ADR 0015).

## Consequences

- Backfill ships **inside the MVP**, so the alpha satisfies the "full-history
  search" criterion rather than deferring it; no PRD wording needs softening.
- Ordering within M9 is natural: 9a builds the index, 9b feeds it the rest of
  history through the same ingestion path — so 9b indexing is automatic, not a
  second integration.
- Because backfill reuses the live ingestion path, no new persistence,
  decryption, or indexing code is needed — it is a driver feeding existing
  machinery, which is what keeps 9b a tractable half-milestone rather than its
  own.
- `sync.timeline_limit` becomes a cold-start latency knob rather than the de
  facto history bound.
