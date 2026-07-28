# ADR 0068 — Matrix C-S parity: batched verb routes (M19)

**Status:** Proposed — targets **Milestone 19** (server-side Matrix capability
gap-filling; design ADR, no code yet).

## Context

Issue #279 inventories every remaining Matrix Client-Server capability an Axon
client cannot reach today. Axon's differentiated value (Tantivy search,
server-side relation aggregation, SSO/OAuth, multiaccount, E2EE decryption,
media proxy) is fully built; what's left is mostly "the homeserver already
does this, Axon just hasn't wired a route to it" — typing, room membership,
room settings, profile actions.

We already rejected a generic passthrough for this class of gap (#130):
letting a client call arbitrary CS-API paths through Axon bypasses state
coherence, E2EE, and the security review each capability deserves. But #279's
own inventory shows almost every remaining gap is the *same shape* as the
mutations M6 already ships: one `matrix-sdk` call, resolved through
`SdkGateway` (`crates/axon-sync/src/gateway.rs`), reusing the consumer-owned
port + composition-root-adapter pattern from ADR 0021 — `axon-api` owns the
trait it needs, `axon-sync` implements the concrete SDK call, `axon-server`
adapts one onto the other. `SdkGateway::room()` (gateway.rs:167) already
resolves `(account_id, room_id)` to a live `Room` via
`ClientManager::get_or_connect` plus an error taxonomy
(`map_sdk_err`/`GatewayError::{Invalid,Forbidden,RoomNotFound}`) that every
new room-scoped verb can reuse verbatim.

ADR 0067 (outbound read receipts, tracked in #278) already proves the
pattern for a fire-and-forget ephemeral send. This ADR is the umbrella for
the rest of #279's Tier A/B inventory: design the shared conventions once,
then stamp them across a handful of grouped PRs instead of running a design
cycle per verb.

Tier C of #279 (invited-room visibility, presence, unread/notification
counts) is explicitly **not** in scope here — each needs real design work
that doesn't fit the batched-verb shape. See "Tier C" below for how each is
sequenced instead.

## Decision

### Shared conventions, stamped once

Every M19 route follows conventions already established by M5/M6/M7b and
restated here so no batch re-derives them:

- Account-nested routes (`/v1/accounts/{account_id}/...`), per the M5a
  convention.
- Success envelope `{ "data": <T> }`; errors `{ "error": { "code",
  "message" } }` (existing `ApiResponse`/`ApiError`).
- The existing bearer-token gate (ADR 0029) and `get_or_connect`'s
  `state == active` check (`403` on a non-active account) apply unchanged —
  no new auth surface.
- Error mapping reuses `map_sdk_err`'s shape: SDK `M_FORBIDDEN` →
  `GatewayError::Forbidden` → `403`; a malformed id/param is validated before
  any SDK call → `GatewayError::Invalid` → `400`; anything else upstream →
  `502`/`503` per the existing convention.
- OpenAPI entries land against the golden file (`UPDATE_OPENAPI=1 cargo test
  -p axon-api --test openapi`) in the same PR as the handler.
- Structured logging includes `account_id`, `room_id` (where applicable),
  and the target user/event id.

### Trait groups and batch boundaries

#279 proposed four trait groups (`TypingSender`/ephemeral, `RoomMembership`,
`RoomSettings`, `AccountActions`) as roughly four PRs. This ADR keeps the
four trait groups but splits two of them, because the codebase already gives
us a sharper line to split on than "same trait" — **resolution path and
risk**, not just trait membership:

- **M19a — typing notice.** Piggybacks on the already-designed read-receipt
  relay (#278/ADR 0067) rather than earning its own ADR: same fire-and-forget
  ephemeral-outbound semantics ADR 0067 already specified, same
  `Gateway::room()` resolution, landing as a sibling method in that work.
- **M19b — existing-room membership** (`leave`, `forget`, `invite`, `kick`,
  `ban`, `unban`). All resolve via `SdkGateway::room()` exactly like
  `send_message`/`redact` today. `leave`'s downstream handling already
  exists — the ADR 0037 membership filter and ADR 0044's opt-in
  `purge_on_leave` both key off the `m.room.member` leave event that sync
  already persists via `persist_state_event`, so this batch adds no new
  store logic, only the outbound call.
- **M19c — room entry** (`join_room_by_id_or_alias`, `knock`, `create_room`/
  `create_dm`). Split from M19b because these skip `room()` entirely — there
  is no `Room` handle yet, so they resolve via `ClientManager::get_or_connect`
  directly — and carry materially more test surface: does the M10 backfill
  poller's next `joined_rooms()` re-poll pick up the new room, does
  `create_dm`'s `mark_as_dm` round-trip, does `enable_encryption` land before
  any message can leak. That's enough extra verification burden to want its
  own review pass rather than riding with M19b.
- **M19d — room settings** (`name`, `topic`, `avatar`, `tags`). Genuinely
  uniform single-field state-event writes; stamp the pattern per-field
  (`PUT .../name`, `.../topic`, `.../avatar`, `.../tags`) rather than one
  combined settings blob — Matrix itself models these as independent state
  events with independent power-level requirements, and a combined PUT
  invites "was this field omitted or explicitly cleared?" ambiguity that
  separate routes don't have.
- **M19e — power levels.** Deliberately **not** bundled into M19d. This is
  the one verb in the whole inventory where a client mistake is not just a
  bad write but a potential permanent lockout: dropping your own effective
  power level below what's needed to send another `m.room.power_levels`
  event succeeds at the protocol level and leaves no way to self-correct.
  `map_sdk_err`'s `403` on `M_FORBIDDEN` doesn't help here — the call
  *succeeds*, it just strands the caller. M19e's PR must decide an explicit
  guardrail (e.g. reject a change that would drop the caller's own resolved
  PL below `state_default`/`events_default` unless the request sets an
  explicit acknowledgment flag) before shipping — this is exactly the
  boundary-robustness discipline AGENTS.md already asks for, and it doesn't
  belong stamped identically alongside three routes with no such failure
  mode.
- **M19f — account actions** (`set_display_name`, `set_avatar_url`,
  `fetch_user_profile_of`, `ignore_user`/`unignore_user`,
  `public_rooms_filtered`). Kept as one PR for the same reason #279's Tier B
  was one PR — no `Room` handle, everything resolves via `get_or_connect` —
  but the ADR should note `public_rooms_filtered` (directory search) is a paginated
  **read** proxy, not a mutation like the other four methods sharing this
  trait; it needs its own request/response shape (offset/limit or a cursor,
  not `{ "data": { "event_id" } }`) even though it's grouped here for the
  same account-scoped-no-Room reason.

Read receipts (#278/ADR 0067) are unaffected by this ADR — already designed,
tracked, and unblocked; M19a only adds the typing sibling.

### Tier C — not batched, sequenced separately

- **Invited-room visibility.** The largest single remaining gap (no
  stripped-state handling anywhere in `axon-sync`/`axon-store` today, and
  `list_rooms` is built from persisted events an invited room never has). It
  is also the precondition for M19b's `invite` verb being useful in the
  reverse direction. Recommended as the **next** milestone after M19 lands,
  with its own ADR — real schema/projection/WS design work, not a stamped
  verb.
- **Presence.** Stays deferred exactly as ADR 0056 already recorded (both
  directions gated on an actual lag measurement that hasn't happened, and
  inbound presence needs a second SDK handler registration —
  `HandlerKind::Presence`, no `Room` argument — that `forward_ephemeral_event`
  structurally can't receive). No new ADR is warranted until that trigger
  condition is met; writing a design doc for something explicitly not being
  built yet is pure overhead.
- **Unread / notification counts.** #279's own text suggests this "possibly
  fold[s] in the #241 sync-state gap's plumbing" — that's incorrect and this
  ADR corrects it rather than carrying the mix-up forward. #241/ADR 0030 is
  an **account-level** sync-readiness signal (has initial sync finished);
  unread/notification counts are a **room-level** figure the SDK derives
  from the sliding-sync room-list summary (`notification_count`/
  `highlight_count`), an entirely independent mechanism from both #241 and
  the M18 ephemeral bus (issue #233 item 1 already made the same "not a true
  EDU" point about M18). Tracked as its own future design item, with no
  dependency on #241.

## Consequences

- **Pro:** ~17 gaps close via 6 PRs instead of 17 separate design cycles,
  each still getting a typed route, OpenAPI entry, and its own tests.
- **Pro:** every batch reuses `SdkGateway`'s existing room-resolution and
  error-mapping helpers verbatim — no new crate-boundary decisions.
- **Pro:** splitting by resolution-path/risk (M19b/c, M19d/e) instead of
  strictly by #279's four trait names means the highest-risk verb (power
  levels) gets isolated review instead of hiding inside a "stamp the
  pattern" batch.
- **Con / accepted:** six PRs instead of four is more review overhead than
  #279 originally proposed, in exchange for risk-appropriate isolation of
  power levels and new-room entry.
- **Con / accepted:** Tier C stays fully unscheduled beyond "invited-room
  visibility is next" — no committed milestone number yet for presence or
  unread counts.
- **Con / accepted:** client consumers (TUI, web) are separate per-silo
  follow-up PRs, not part of M19 — see `docs/client-parity.md` for tracking
  that follow-up so it doesn't silently lag the way SAS verification and the
  M16 device picker already have.

## Suggested PR sequence

1. **M19a — typing notice.** Sibling method alongside #278/ADR 0067's
   `ReadReceiptSender`; no new ADR.
2. **M19b — existing-room membership.** `leave`/`forget`/`invite`/`kick`/
   `ban`/`unban` via `SdkGateway::room()`.
3. **M19c — room entry.** `join`/`knock`/`create_room`/`create_dm` via
   `ClientManager::get_or_connect`; verify M10 backfill pickup and DM/
   encryption flags.
4. **M19d — room settings.** `name`/`topic`/`avatar`/`tags`, one route per
   field.
5. **M19e — power levels.** Alone, with an explicit self-lockout guardrail
   decided in the PR.
6. **M19f — account actions.** Profile read/write, ignore list, directory
   search (the last one shaped as a read proxy, not a mutation).

Each PR stays server-silo only, per the one-silo-per-PR rule; client
consumers follow as separate TUI/web PRs.
