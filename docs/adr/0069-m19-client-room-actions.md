# ADR 0069 — Client rollout: room actions in axon-tui and axon-web

**Status:** Proposed — client-side companion to ADR 0068 (M19). Tracked in
issue #304.

## Context

Until M19, neither Axon client could change room membership: `GET /v1/rooms`
was the only room-membership-adjacent call, and both clients already registered
`/join`/`/leave` as known-but-unsupported commands. ADR 0068 designed the
server-side batch that closes this gap. As of this ADR, **M19b (#298,
membership verbs) and M19c (#307, room entry) are merged** on `main`; M19d
(room settings) and M19e (power levels) are unstarted; **M19f (directory
search, bundled with profile/ignore actions) is still Planned, not merged**.

Issue #304 filed an initial client plan against ADR 0068's design-time
"expected" contracts. A review comment on that issue (jamieforrest) caught
that several of those contracts had already gone stale once M19b/M19c
actually landed — this ADR supersedes the guessed shapes with the contracts
verified against the merged code, and is the settled scope for the client
rollout, not a re-derivation of ADR 0068's server-side design.

Tier C of #279 (invited-room visibility, presence, unread counts) remains
out of scope of M19 and undesigned: `axon-sync`/`axon-store` have no
stripped-state handling today, so an invited room cannot appear in
`list_rooms`. That is a hard precondition for a real invitation inbox and is
not being designed here.

## Decision

Client work proceeds **one silo per PR** (`clients/tui/` or `clients/web/`,
per AGENTS.md), each PR wired to whichever M19 batch has **actually merged** —
not to ADR 0068's design-time guess. Six client features cover the room-action
surface:

1. **Leave / forget** a room (M19b — merged)
2. **Join / knock** a room, including via a `matrix.to`/`matrix:` hyperlink
   (M19c — merged)
3. **Public-room discovery** across homeservers (M19f — **not merged, blocked**)
4. **Invite** (and kick/ban/unban) other users, with cached-data username
   autocomplete (M19b — merged)
5. **Invitation inbox** — see and accept/reject pending invites (Tier-C —
   **not designed, blocked**)
6. **Create a room / start a DM** (M19c — merged; newly committed scope, not
   present in ADR 0068's original inventory of client work)

### Corrected server contracts (supersede ADR 0068's design-time placeholders)

Verified against `crates/axon-api/src/routes/{membership,room_entry}.rs` and
`dto.rs` on `main`:

- **Membership verbs (M19b) return an empty object**, not an event id:
  `leave`/`forget`/`invite`/`kick`/`ban`/`unban` all respond `200 {"data": {}}`.
  The resulting `m.room.member` state event round-trips through ordinary sync;
  clients must not expect an `event_id` on this path. `leave`/`forget` take no
  body; `invite` takes `{user_id}`; `kick`/`ban`/`unban` take
  `{user_id, reason?}`.
- **Room-entry verbs (M19c) return `{room_id}` only** (`RoomEntryResultDto`),
  also with no event id:
  - `POST …/rooms/join` — `{room_id_or_alias, server_names?}`. The
    federation-resolution field is **`server_names`**, not `via` — ADR 0068's
    design text used ruma's internal name; the wire field renames it.
  - `POST …/rooms/knock` — `{room_id_or_alias, reason?, server_names?}`.
  - `POST …/rooms/dm` — `{user_id}`.
  - `POST …/rooms` — `{name?, topic?, invite: string[], is_direct, public,
    preset?, encrypted}`; an empty body creates a private, unencrypted,
    unnamed room. `preset` wire values: `private_chat`/`public_chat`/
    `trusted_private_chat`.

Because these responses carry no event id, both clients confirm a mutation by
**refreshing room/member state** (TUI `refresh_rooms`, web `rooms.refresh()`)
rather than echoing a returned event — the same repair path both already use
for reconnect and for ADR 0037's read-time leave/ban filtering.

### Two client-specific product decisions

- **Web registers an OS-level `matrix:` protocol handler**
  (`navigator.registerProtocolHandler`), in addition to in-app interception of
  `matrix.to`/`matrix:` links in message bodies and a `/join` command —
  feature-detected and gated behind a one-time settings opt-in so it never
  fires an unprompted browser permission dialog.
- **`/part` is a synonym for `/leave`** in both clients (the TUI already
  reserved the name), so command muscle memory carries over between clients.

### Shared building blocks (reused, not duplicated)

- **Matrix URI parsing** maps a `matrix.to`/`matrix:` link to
  `{ target, server_names }`. TUI wraps `ruma`'s existing `MatrixToUri`/
  `MatrixUri`; web hand-writes an equivalent parser (no `ruma` in the JS
  toolchain).
- **Room-target resolution** extends each client's existing resolver (TUI
  `resolve_room_target`, web `resolveRoomTarget`) to accept a parsed matrix
  link alongside id/alias/name.
- **Invite autocomplete draws only from already-cached in-memory data** —
  members of the account's other joined rooms plus recent timeline senders,
  unioned and deduplicated client-side. This adds no new network round-trips
  and no fan-out fetch of unfetched rooms' membership, so it carries no
  meaningful performance cost. It does not cover users the account has never
  shared a room with (that needs a homeserver user-directory search endpoint,
  which M19 does not add); free-form `@user:server` entry remains available as
  the fallback.

### Explicitly blocked, not scoped further here

- **Public-room discovery (Feature 3)** waits on M19f actually merging. M19f
  also bundles `set_display_name`/`set_avatar_url`/`fetch_user_profile_of`/
  `ignore_user`/`unignore_user` in the same server PR, so it will not be a
  small, directory-only change; the client directory PR should be scoped off
  M19f's real merged contract, not a pre-merge guess — repeating that mistake
  is exactly what this ADR is correcting for M19b/M19c.
- **Invitation inbox (Feature 5)** waits on Tier-C invited-room visibility
  landing its own ADR and stripped-state projection. Accept (join) and reject
  (leave) already work once Features 1/2 ship; only the list of pending
  invites is missing.

## Consequences

- **Pro:** four of six features (Leave, Join, Invite, Create/DM) are
  unblocked today and can be scoped as PRs immediately, rather than waiting on
  the full M19 sequence.
- **Pro:** correcting the contracts here, once, prevents every downstream
  client PR from independently re-discovering that membership/entry responses
  carry no event id.
- **Pro:** reusing each client's existing link-parsing, room-resolution, and
  refresh-on-mutation machinery keeps this a thin client layer over already-
  proven patterns rather than new infrastructure.
- **Con / accepted:** Features 3 and 5 stay unscheduled beyond "blocked on X,"
  same as ADR 0068 left Tier C unscheduled — no committed milestone number for
  either yet.
- **Con / accepted:** six client PRs (three silos of work × roughly two
  batches) is more review surface than one combined "room actions" PR per
  client, in exchange for the one-silo-per-PR discipline and independently
  landable, independently revertable slices.

## Suggested PR sequence

**Ready now** (M19b and M19c are merged):
1. TUI leave/forget PR + web leave/`part` PR (Feature 1).
2. TUI invite(+kick/ban/unban) PR + web invite PR, including autocomplete
   (Feature 4).
3. TUI join/knock PR + web join PR (link interception, protocol handler,
   join overlay) (Feature 2).
4. TUI create-room/DM PR + web create-room/DM PR (Feature 6).

**Blocked:**
5. TUI + web directory PRs, after M19f merges (Feature 3).
6. TUI + web invitation-inbox PRs, after Tier-C invited-room visibility ships
   (Feature 5).

Each PR updates the corresponding `docs/client-parity.md` row in the same PR,
per that doc's cross-silo exception to the one-silo-per-PR rule.
