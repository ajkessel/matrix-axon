# ADR 0018 — History backfill as a dedicated milestone

## Context

axon's archive is populated by the sync engine, which only ever sees events
*going forward*: live timeline events, plus the shallow per-room window set by
`sync.timeline_limit` on a room's first sync (ADR 0015). Nothing reaches back for
a room's pre-existing history. ADR 0015 named a "full history-backfill engine" as
"later work" but assigned it to no milestone.

That omission collides with the PRD:

- **Success criterion #2 — "Full-history search… p95 < 200ms."** The M9 search
  index is populated on event ingestion, so it contains exactly what sync has
  ingested. On a fresh install that is *not* a room's full upstream history, so
  "full-history search" is not literally achievable.
- **The 100–200k-event working-set target** only materializes if something
  deliberately pulls that history down.

A subtlety that makes this its own piece of work: `recover()` (ADR 0011/0014)
imports the *keys* to decrypt old messages, but not the *messages* themselves —
those must still be fetched by paging each room's `/messages` endpoint. So
backfill is a distinct engine, not a side effect of key recovery.

## Decision

**History backfill is a dedicated milestone (13), sequenced before threads
(now 14).** It is ordered ahead of threads because it closes a *PRD success
criterion* gap, whereas threads are an additive feature deferral (ADR 0017).

Shape:

- A bounded, **resumable** engine that pages backward through each room's
  timeline via the SDK's room pagination, decrypts with already-imported keys,
  and persists through the **same ingestion path as live sync** — so hot columns,
  crypto siblings, redaction handling, and search indexing apply uniformly and
  re-runs are idempotent (`ON CONFLICT DO NOTHING`).
- Per-room backfill state (e.g. a `room_backfill` table keyed by
  `(account_id, room_id)` recording the oldest token reached and a `complete`
  flag) so progress survives restarts and the engine knows where to resume.
- Background and throttled, so it never starves live sync; configurable target
  depth.
- It retires the `sync.timeline_limit` bump as the "bounded substitute" for real
  backfill (ADR 0015).

## Status / open question

This ADR records that the engine **needs a milestone and where it sits**. One
question is deliberately left for the human, because it changes the MVP's scope,
not just its ordering:

- **Is backfill in-MVP or post-MVP?** As placed (Milestone 13, after the M12
  self-hosting docs), the MVP alpha ships with search over *ingested* history and
  literal full-history search arrives just after. That contradicts a literal
  reading of PRD criterion #2. The alternative is to pull backfill ahead of the
  web alpha / docs (renumbering the tail) so the MVP itself satisfies the
  criterion, at the cost of a later alpha. **Default for now: post-MVP (13).** If
  full-history search is a hard MVP gate, promote it and soften nothing; if "the
  alpha searches what it has ingested, full history follows" is acceptable, keep
  it here and soften the PRD wording.

## Consequences

- Threads move from Milestone 13 to **Milestone 14** (ADR 0017 and
  `implementation.md` updated accordingly).
- Once built, the archive and search index can cover a room's full upstream
  history, not just the post-install slice; `sync.timeline_limit` becomes a
  cold-start latency knob rather than the de facto history bound.
- Because backfill reuses the live ingestion path, no new persistence,
  decryption, or indexing code is needed — it is a driver that feeds existing
  machinery, which is what keeps it a single milestone.
