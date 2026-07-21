# ADR 0070 — Server-derived per-room unread counts

## Context

A freshly loaded or reloaded web client can show a durable "unread dot" from
its own locally-persisted read markers (ADR 0048, ADR 0067), but it cannot
show a *numeric* unread count until it has personally observed live message
events in the current session: `RoomDto`'s only activity signal was
`last_activity_ts`, an unfiltered `MAX(origin_ts)` over every event type, and
the web client's own unread counter (`stores/unread.ts`) is live-only,
resetting to zero on every reload. Issue #313 asks for a real, server-derived
count so a fresh session shows the right number immediately.

Real Matrix unread/notification counts are a **sync room-summary field**
(`unread_notifications.notification_count` / `highlight_count`), computed
server-side by the homeserver's push-rule evaluation. This is not an
ephemeral event, so the M18 ephemeral-passthrough path (ADR 0056) — a raw,
lossy, no-replay forward of allowlisted `m.typing`/`m.receipt`-style events —
structurally cannot carry it: there is no ephemeral event to forward, and
ADR 0056's passthrough only carries per-event `content` verbatim, not a
room-summary aggregate.

The scope of this ADR is **server-only**: expose the data through `RoomDto`
and a new live WS frame. Web-client consumption (retiring
`stores/unread.ts`'s live-only counter, `hasRoomUnread`'s reload-resetting
heuristic) is a deliberate follow-up, not covered here.

## Decision

### Source of truth: matrix-sdk's already-computed counts

Axon's `matrix-sdk` dependency (v0.18.0) already computes real Matrix
notification/highlight counts locally, sourced directly from the upstream
homeserver's actual sync response: `Room::unread_notification_counts()`
(backed by `RoomInfo::notification_counts`), updated automatically by
matrix-sdk-base on every sync — including the initial one. Axon does not
reimplement read-position tracking or push-rule filtering over the `events`
table; it captures and persists a value matrix-sdk already computes
correctly. This keeps the homeserver as the single source of truth for
notification semantics, rather than introducing a second, Axon-maintained
read-position model that could drift from the real receipts the existing
`POST …/rooms/{room_id}/read` endpoint (ADR 0067) already writes through to
Synapse.

### Capture mechanism: a watcher task, not a request-time query

A new `watch_unread_counts` task in `crates/axon-sync/src/engine.rs` (same
pattern as `watch_sender_trust`/`watch_verification`): the in-memory dedup
cache is seeded from whatever is already persisted (`Store::room_unread_counts`)
before anything else runs, so a restart's startup sweep only re-upserts and
re-broadcasts rooms whose counts actually changed since the last run — not
unconditionally every joined room. The watcher then subscribes to
`Client::room_info_notable_update_receiver()`, then runs the (now-seeded)
startup sweep over every currently-joined room (`client.joined_rooms()`), so
a notable update that lands mid-sweep is queued on the receiver rather than
missed — replaying it afterward is a harmless no-op once
`capture_unread_counts`'s dedup check sees the sweep already observed the
same value. Sweeps (both this startup one and the periodic backstop below)
run with a small bounded concurrency rather than one Postgres round trip at a
time, and prune stale state for rooms the account has since left — both the
in-memory dedup cache and, via `Store::delete_stale_room_unread_counts`, the
persisted `room_unread_counts` row itself, so a left room's row doesn't sit
in the table forever (previously only `ON DELETE CASCADE` on account
deletion cleaned these up; harmless — `list_rooms` already filters left
rooms — but unbounded growth for a long-lived account that churns through
many rooms).

The watcher reacts to **every** notable update regardless of its `reasons`
bitflag. `RoomInfoNotableUpdateReasons` has no dedicated "notification count
changed" bit — the closest candidates (`RECENCY_STAMP`, `LATEST_EVENT`,
`READ_RECEIPT`) are not documented as exhaustive triggers for a count change,
and the bitflags include a `NONE` sentinel described upstream as a temporary
hack. Filtering on specific reasons would be a bet on an implementation
detail; reacting to every update and dedup-ing on the actual value diff
(`capture_unread_counts`'s in-memory `HashMap<OwnedRoomId, (u64, u64)>`) is
correct regardless of how upstream's reason bits evolve.

A lagged broadcast receiver is not specially recovered: the watcher always
re-derives the *current* value from `Room::unread_notification_counts()`
rather than diffing the missed notification, so a dropped update for a room
self-heals the next time anything about that room changes. A periodic
re-sweep (`UNREAD_COUNTS_RESWEEP`, every 5 minutes) is a backstop against a
room going quiet immediately after a lag.

### Storage: one row per `(account_id, room_id)`, looked up by primary key

`room_unread_counts(account_id, room_id, notification_count, highlight_count,
updated_at)`, upserted via `Store::upsert_room_unread_counts`. `list_rooms`
reads it via two more correlated sub-selects, in the same style as the four
existing display-field sub-selects (`name`/`topic`/`avatar_url`/
`canonical_alias`) — a single-row PK lookup, not an aggregate.

This directly **supersedes one sentence of ADR 0055**: that ADR deferred
member/unread counts from the Tier 1 `RoomDto` projection as "the expensive
case," reasoning that list latency should not scale with the priciest field.
That concern was about an aggregate query computed at read time (e.g.
`COUNT(*)` over events since a read marker); a stored scalar looked up by
primary key has the same cost profile as the fields ADR 0055 already put in
Tier 1. The rest of ADR 0055 (the Tier 1/Tier 2 split, `is_direct`/
`room_type`/`tags` reasoning) is unaffected.

### Wire delivery

- `RoomDto.notification_count` / `RoomDto.highlight_count` (`i64`, always
  present, `0` until the watcher has captured a value) — what a fresh
  `GET /v1/rooms` load returns.
- `unread_counts.changed` on `/v1/ws` (`UnreadCountsFrame` →
  `UnreadCountsFramePayload`) — live updates to an already-connected client,
  following the `SenderTrustFrame`/`sender_trust.violation` pattern exactly.
  Not built on ADR 0056's ephemeral passthrough (see Context).

## Documented limitation: encrypted rooms

`Room::unread_notification_counts()`'s own doc comment states these values
"might be incorrect for encrypted rooms, since the server doesn't know which
events are relevant standalone messages or not."

Concretely: the homeserver always correctly excludes the account's own
events (the sender is always plaintext, even in an encrypted room), so **own
messages/reactions/redactions/edits never increment the count** —
unconditionally, matching issue #313's acceptance criteria.

Inside an **encrypted room**, other users' reactions/edits/redactions travel
as opaque `m.room.encrypted` at the transport type. In an unencrypted room,
Synapse's default push rules (`.m.rule.reaction`, `.m.rule.message` with
`dont_notify` for edits, etc.) inspect the plaintext event type/content and
suppress notifications for these kinds. Encrypted, the homeserver cannot
apply those content-based rules and falls back to `.m.rule.encrypted`, whose
default action is to notify for most events from other users. So in an
encrypted room, other users' reactions/edits/redactions plausibly **do**
increment `notification_count` — a real, spec-driven divergence from the
issue's default assumption, not an Axon bug.

Per issue #313's own "unless intentionally different and documented" clause,
this is surfaced rather than "fixed" — reimplementing content-aware push-rule
evaluation over encrypted payloads is a materially larger, separate feature,
and would mean Axon second-guessing the homeserver's own notification
semantics. The caveat is documented in three places: `RoomSummary`'s and
`RoomDto`'s doc comments (the latter lifted into the OpenAPI schema
description by utoipa), `UnreadCountsFrame`'s doc comment, and this ADR.

## Consequences

- A fresh or reloaded client can show a real unread count immediately,
  without waiting to observe a live event in-session — the literal
  acceptance criterion in issue #313.
- Axon gains no new read-position source of truth: the homeserver remains
  authoritative, and the existing read-receipt round trip (ADR 0067) is what
  clears the count, for free.
- Encrypted-room counts inherit a known, documented over-counting risk for
  other users' reactions/edits/redactions. A future ADR could revisit this
  if it proves disruptive in practice (e.g. by having Axon apply push rules
  itself post-decryption), but that is explicitly out of scope here.
- Web-client consumption (rendering these fields, retiring the live-only
  counter) is deferred to a follow-up PR.
