# ADR 0015 — Event store schema: hot columns, crypto siblings, timeline read

## Context

Milestone 3 persisted events as a single flat `events` table — enough to prove
sync + re-decryption, but missing what a timeline read and a future search index
need: the relational hot columns (`redacts`, `relates_to`, `decrypted_body_text`),
a paginated read path, redaction handling, and a durable record of *how* each
encrypted event was decrypted so a decrypted row stays re-verifiable against
Matrix's signatures.

`docs/mvp/implementation.md` §4 ("Event store schema") specifies this. This ADR
records the decisions made implementing it (M4a) that the spec does not pin down.

## Decision

### Hot columns on `events`

Added `redacts TEXT`, `relates_to JSONB`, `decrypted_body_text TEXT`, plus an
`(account_id, room_id)` index and a partial `(account_id, redacts) WHERE redacts
IS NOT NULL` index. `relates_to` is captured generically (it already holds
`m.thread` relations) but **not** thread-indexed — threads are deferred (a
scope-gated open decision in the spec), so the column is forward-compatible
without committing to thread features.

### Crypto provenance lives in sibling tables, keyed by `(account_id, event_id)`

Three siblings, each FK→`events(account_id, event_id)` ON DELETE CASCADE:

- `event_ciphertext` — the original `m.room.encrypted` envelope.
- `event_megolm_session` — the megolm session metadata that decrypted it.
- `event_sender_device_keys` — the sending device's identity keys + verification
  state at decrypt time (a snapshot; it can change later as devices are verified).

### EncryptionInfo finding (the load-bearing investigation)

The matrix-rust-sdk 0.17 event handler accepts `Option<EncryptionInfo>` as an
extractor, and `TimelineEventKind::Decrypted` carries `Arc<EncryptionInfo>`. So
the megolm-session and sender-device-key siblings are populated **for every
successfully-decrypted event** — on the live dispatch path (`persist_timeline_event`)
and again on the re-decryption path (`redecrypt_one`), since a UTD has no
`EncryptionInfo` at first persist.

**The original ciphertext is *not* surfaced for events the SDK decrypts before
dispatch** — the handler only ever sees the plaintext. The ciphertext is
available only when an event arrives as a UTD (where it is the `raw_event`). So
`event_ciphertext` is written **from the UTD path only**. This is a deliberate,
documented limitation: live-decrypted events have their crypto provenance (megolm
+ device siblings) but no separate ciphertext row. It matches the existing M3c
behavior where re-decryption reads UTD ciphertext straight from `raw_event`.

### Timeline read with cursor pagination and read-time redaction masking

`Store::room_timeline(account_id, room_id, before, limit)` returns rows newest
first, ordered by `(origin_ts DESC, id DESC)`. The cursor carries `(origin_ts,
id)`; the monotonic `BIGSERIAL` `id` is the tiebreaker so pages never overlap or
skip when events share an `origin_ts`. Redaction is applied **at read time**: a
`LEFT JOIN LATERAL` (LIMIT 1, so a doubly-redacted target still yields one row)
finds any `m.room.redaction` pointing at a row; when present, `content` and
`decrypted_body_text` are masked to `NULL` and the redaction's event id is set on
`redaction_event_id`. The stored row and its ciphertext sibling are never mutated
— masking is purely a projection.

**Field naming (deviation from spec).** `docs/mvp/implementation.md` §4 and
`tech-spec.md` name this field `redacted_because` and define it as carrying the
redaction event ID. We renamed it to **`redaction_event_id`**: the value is only
the id, and `redacted_because` collides with ruma's `unsigned.redacted_because`,
which carries the *whole* redaction event — so reusing that name for an id-only
field is a trap. The spec author signed off on the rename; the frozen specs stay
as-is (the deviation is recorded here, per the doc-governance model). The M5 HTTP
layer should surface the field as `redaction_event_id` accordingly.

### Account-scoped uniqueness (deviation from literal spec)

The spec lists a unique `(event_id)`. We keep the existing unique
`(account_id, event_id)` instead: every account-scoped table carries
`account_id` by project convention, and a single Axon hosting multiple accounts
could legitimately see the same event id in two accounts' views.

### Timeline depth

The SDK's sliding-sync list defaults to a per-room timeline limit of **1**
(latest event only) — the root cause of M3's "latest-event-only" archive. We
raise it via `SyncServiceBuilder::with_room_list_timeline_limit`, driven by a new
`sync.timeline_limit` config (default 20). This is a bounded substitute for a
full history-backfill engine, which remains later work.

### Recovery-key at-rest review (closes the ADR 0011 M4 item)

ADR 0011 asked us to review in M4 whether to persist the recovery key encrypted
at rest. Decision: **keep it transient-only** (consumed once on boot, never
stored — no `accounts` column), the posture ADR 0011 already preferred. The
mature, no-server-secret path is interactive verification in M5.

## Consequences

- A decrypted timeline row is re-verifiable via its megolm/device siblings;
  redactions are honored without destroying the underlying ciphertext.
- Crypto siblings are best-effort writes (logged, never fatal to sync), so a
  sibling write failure can leave provenance gaps — acceptable for a derived,
  re-derivable table.
- Live-decrypted events have no `event_ciphertext` row (see finding above). If we
  ever need ciphertext for *all* events we'd have to capture it pre-decryption,
  which the SDK's handler API does not currently expose.
- `sync.timeline_limit` only deepens *new* syncs; it is not retroactive history
  backfill.

## M4 milestone re-scope (verification → M5)

The spec assigned E2EE work to M4 in two pieces. Reality diverged, recorded here
and in ADR 0011: the **recovery-key bootstrap landed early in M3c** (it was the
re-decryption queue's driver), and the **verification plumbing moved wholly to
M5** — it cannot be exercised before the `/v1/ws` channel exists, so splitting it
M4/M5 bought nothing. M4 is therefore the event-store schema (this ADR); the
`axon-crypto` verification surface is built in M5. See ADR 0011.
