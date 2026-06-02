# ADR 0017 — Threads deferred to a dedicated post-MVP milestone

## Context

Matrix threads (`m.relates_to` with `rel_type: m.thread`) were the one genuinely
open scope question carried from `tech-spec.md` into `implementation.md`. The
original framing threaded the feature through three milestones: thread-aware
indexing in M4, thread endpoints in M5, and a "view in thread" affordance in M11.

That framing has a cost: it couples a deferrable feature into the critical path
of three separate milestones, and it forces a yes/no on threads before the MVP
read/write/search loop is even proven.

While implementing M4 we made a smaller, load-bearing decision (ADR 0015): the
`events.relates_to` JSONB column captures **every** relation generically,
including `m.thread`, even though nothing indexes or reads threads yet. That
choice is what makes the larger deferral cheap.

## Decision

**Threads are deferred to a single, self-contained, post-MVP Milestone 14** —
not woven through M4/M5/M11.

The deferral is **forward-compatible and backfill-free** because the data is
already being captured:

- `events.relates_to` holds the raw relation object for every decrypted event
  (UTDs pick it up on re-decryption). The thread membership of every
  already-stored event is therefore recoverable from data on disk.
- Adding threads later is purely additive: an index (an expression/partial index
  or a generated column over `relates_to`), store reads, endpoints
  (`GET /v1/rooms/{room_id}/threads` + a thread-scoped timeline), a thread-aware
  send on the existing M6 send path, and a web affordance. No re-sync, no
  re-parse of `raw_event`, no schema migration of existing rows' data.

The alternative we explicitly avoided is **not** capturing `relates_to` in the
MVP. That would have turned a future threads milestone into a data-migration
problem — re-parsing or re-syncing every historical event to recover relations —
which is exactly the expensive kind of deferral this decision sidesteps.

## Consequences

- The MVP (M1–M12) ships without threads; threaded replies still persist as
  ordinary events with their relation preserved, just not surfaced as threads.
- Milestone 14 is low-risk and self-contained: an index + endpoints + UI, no
  re-architecture. Its verification can assert the thread index resolves over
  events stored *before* the milestone, proving the backfill-free claim.
- The "one JSONB column now" write-time cost (ADR 0015) is the price already paid
  to keep this option open; this ADR is the decision that cashes it in as a
  clean future milestone rather than MVP-critical-path work.
- `implementation.md` is updated: the threads open-decision is marked resolved
  and Milestone 14 is added. Per the doc-governance model, the *decision* lives
  here; the spec records the resulting plan.
