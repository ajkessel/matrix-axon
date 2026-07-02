# ADR 0043 — History backfill engine (M10 implementation)

## Context

ADR 0018 established *that* history backfill ships in the MVP and its broad
shape: a bounded, resumable engine that pages each room's timeline backward
through the SDK's `/messages` pagination and persists through the same ingestion
path as live sync. It left the concrete build decisions open. `implementation.md`
§10 sequences backfill last of the read-side milestones so every backfilled event
is aggregated (M8) and indexed (M9) incrementally in one pass. This ADR records
the decisions made implementing it (M10).

## Decision

### Continuous background task, not a one-shot boot sweep

Backfill runs for the whole supervised run of an account — spawned in
`run_account` beside the re-decryption queue, under a `cancel.child_token()`,
drained on shutdown — and loops: it pages each incomplete joined room one page
older, then, when all are complete or capped, **idles and re-polls the joined
room list** (`sync.backfill_idle_poll_secs`, default 45). This is how a newly
joined or **re-joined** room is picked up without a restart, which is what makes
"joined rooms only" safe.

The instant-usability window each room shows on first sync is still
`sync.timeline_limit` (ADR 0015); backfill only extends history *behind* it, so
there is no aggressive cold-start phase to bound.

### Joined rooms only

Backfill iterates `client.joined_rooms()`. Rooms the user has left are skipped
(they simply fall out of the polled list); a user can accumulate thousands of
left rooms they don't care about. A re-joined room resumes from its saved token
(the `room_backfill` row survives leave).

### Resume token and exhaustion

Per-room progress lives in a new `room_backfill` table
`(account_id, room_id, oldest_seen_token, complete, events_backfilled)`. The
resume token is the SDK `Messages.end` from the last page; a room is `complete`
when `Messages.end` comes back `NULL` (nothing older upstream). Progress is saved
**after** a page's events are fully persisted, so a mid-page crash re-pages from
the previously saved token — idempotent, because `upsert_event` is
`ON CONFLICT DO NOTHING` and therefore appends no duplicate `search_outbox` rows
either.

A resume token can be permanently rejected by the homeserver (expiry, server
state reset). To avoid a room stalling forever on a bad token, the engine counts
consecutive `/messages` failures per room and, after `MAX_TOKEN_RETRIES`, retries
that room **from the live timeline end** (ignoring the saved token). Re-paging
from the top is idempotent, so the room recovers a valid token and resumes
walking backward; a transient network error just retries the saved token next
sweep.

### Target depth: to room start by default

`sync.backfill_target_depth` caps events per room; `0` (the default) means "to
room start" (unbounded). Unbounded is safe because the engine is continuous and
throttled (`sync.backfill_page_size`, `sync.backfill_throttle_ms`). Hitting a
non-zero cap stops paging a room *without* marking it complete, so raising the cap
later resumes it.

### Same ingestion path, minus the live emit

Back-pagination does **not** dispatch through `add_event_handler`, so the driver
persists each paged `TimelineEvent` itself via a shared `persist_event_core`
(refactored out of the live handler) — hot columns, `upsert_event` (+ the M8/M9
`search_outbox` obligation), and the crypto siblings all apply identically.
Backfilled events **do not** emit a `/v1/ws` frame: replaying deep history through
the live bus would flood subscribers with events mislabeled as just-arrived.
Backfilled history reaches clients through timeline reads (M8) and search (M9),
both driven by `upsert_event`, not the emit.

### Disk-space safety valve

Because backfill grows storage unbounded, it pauses when free space on the
guarded filesystem drops below `sync.backfill_min_free_bytes` (default 2 GiB) or
`sync.backfill_min_free_percent` (default 5%), checked cheaply via `fs4::statvfs`
before each room's page. **Only backfill pauses; live sync is unaffected.** A
shared `BackfillHealth` handle records the paused state and last-observed free
bytes; the API exposes it at `GET /v1/status` (poll-based) so a client can tell
when backfill has paused and resumed.

Backfill's growth lands mostly in the **Postgres `events` table**, whose free
space axon cannot portably measure — so the guarded filesystem is configurable
(`sync.backfill_disk_guard_path`), defaulting to the sync `data_dir`. On the
common single-host / single-volume deploy that default already reflects Postgres's
free space; when Postgres is on a separate host or volume, the operator points the
guard path at the Postgres data volume (or the search index) or monitors that disk
separately. This is a deliberately cheap valve, not a full storage manager.

## Consequences

- Full-history search (the PRD criterion) is reached over wall-clock time as the
  throttled engine grinds each joined room back to its start.
- Re-joins are handled by re-polling, at the cost of a ≤ `idle_poll` latency
  before a newly joined room starts backfilling — acceptable for a low-priority
  builder.
- The disk valve is deliberately cheap: it guards the local filesystem, not the
  Postgres host in a split deployment, and reports via a poll endpoint rather than
  a push. A `backfill.paused/resumed` WebSocket frame was considered but deferred:
  the live bus is account-scoped and the disk signal is engine-global, so it does
  not fit the frame envelope, and multiple accounts' tasks would race to emit it.
- `sync.timeline_limit` is now only a cold-start latency knob (ADR 0015), no
  longer the history bound.
