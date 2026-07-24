# ADR 0074 — Expose state-event `state_key`/`prev_content` for client-side transition rendering

## Context

Issue #31: a user's display-name change was rendered by the TUI as "joined the
room." Matrix display-name and avatar changes are `m.room.member` state
events whose `content.membership` is unchanged (still `"join"`); only
`content.displayname`/`content.avatar_url` moved. The TUI and web client 
currently treat any `m.room.member` event with `membership: "join"` as a join,
so the rendering is wrong for the common case of someone editing their profile.

An Axon client cannot fix this from the wire shape it had: distinguishing a real
join from a no-op-membership profile edit requires comparing the event's
`content` against the state it replaced, and `EventDto` exposed neither the
previous state (`unsigned.prev_content`, standard Matrix wire field on state
events per the Client-Server spec) nor, at the time the issue was filed,
`state_key` (needed to know *which* room member a member event is about).
`state_key` was added to `EventDto` in a prior change; this ADR covers adding
`prev_content` and, more importantly, sets a shared reference for Axon client
developers on what to *do* with the two together — this is a recurring shape
across several Matrix state event types, not just membership.

Axon already stores everything needed: `raw_event` (the full synced/backfilled
event JSON, including `unsigned`) has been persisted on every event row since
the store's ingestion path was built. This is a read-path exposure change
only, not new data capture.

## Decision

### Wire shape

`EventDto.prev_content: Option<Value>` — the event's `unsigned.prev_content`,
verbatim, whenever the source event carried one. `null` for message-like
events (which have no state to replace) and for state events with no prior
state (e.g. the first `m.room.member` event for a user, room creation).
Available on both read paths: `GET …/rooms/{room_id}/timeline` (sourced from
`events.raw_event->'unsigned'->'prev_content'`, same extraction pattern
already used for `state_key`) and the live `/v1/ws` `timeline.event` frame
(captured at ingestion time in `axon-sync`, since a live frame is the
raw pre-aggregation event and there is no later read to re-derive it from).

No new column, no schema migration: `prev_content` already lives inside the
persisted `raw_event` JSONB; this only changes what the API layer projects
out of it.

### Client responsibility, not a server-side diff

The server does not compute or ship a diff (e.g. `{ field: "displayname",
from: "Alice", to: "Alice B" }`). `content` and `prev_content` are both
handed to the client verbatim, matching the raw Matrix event shape, for two
reasons:

1. Different clients want different granularity — the TUI likely wants one
   collapsed sentence per event, the web client may want to bold just the
   changed field. A server-side diff would bake in one client's UX.
2. Interpreting a transition is state-event-type-specific (see the table
   below): there is no single generic diff algorithm across
   `m.room.member`, `m.room.power_levels`, `m.room.pinned_events`, etc. that
   wouldn't be more complex than just comparing the two field-by-field on the
   client, which already has the full `content`/`prev_content` pair in hand.

### Roadmap for client authors: state event types worth diffing

`state_key` + `content`/`prev_content` together are useful well beyond
membership rename detection. This is not a commitment to implement all of
these in any particular client — it's the reference list so a client author
building this feature knows what exists and doesn't have to rediscover it
per event type. Ordered roughly by how commonly mainstream Matrix clients
(Element et al.) render a distinct timeline notice for it:

| Event type | What `prev_content` vs `content` tells you | Example rendering |
| --- | --- | --- |
| `m.room.member` | Full membership transition, not just the new value: `invite→join` (accepted), `leave/none→join` (joined/rejoined), `join→leave` where `sender == state_key` (left) vs `sender != state_key` (kicked), `*→ban` (banned), `invite→leave` (invite rejected) vs `invite→leave` with `sender != state_key` (invite revoked), `knock→join/invite/leave` (knock resolved). Also `displayname`/`avatar_url` diffs when `membership` is unchanged — the issue #31 case. | "Alice joined" / "Bob kicked Alice" / "Alice changed their display name to Alice B" |
| `m.room.power_levels` | The event only ever carries the *full* new `users`/`events` maps. Diffing `users` key-by-key against `prev_content.users` is the only way to know *who* was promoted/demoted without independently tracking prior state client-side; diffing `events`/`state_default`/etc. shows which action's required level changed. | "Bob made Alice a moderator" |
| `m.room.join_rules` | Old vs new join rule (`invite`/`public`/`knock`/`restricted`). | "Alice changed who can join this room to anyone" |
| `m.room.history_visibility` | Old vs new visibility. | "Bob made history visible to anyone" |
| `m.room.guest_access` | Old vs new guest access. | "Guest access was disabled" |
| `m.room.name` / `m.room.topic` / `m.room.avatar` / `m.room.canonical_alias` | The prior value, so a client can render "changed the topic from *X* to *Y*" instead of only showing the new value. | "Alice changed the room name from Foo to Bar" |
| `m.room.pinned_events` | Diffing the `pinned` id arrays identifies which specific event was pinned/unpinned; without `prev_content` a client only has the new full list and would need to have cached the old list itself to say anything more specific than "pinned messages changed." | "Alice pinned a message" |
| `m.room.server_acl` | Diffing `allow`/`deny`/`allow_ip_literals` shows which servers were added to or removed from the ACL — moderation-relevant, often silently applied by clients today. | "Bob blocked example.org from this room" |
| `m.room.encryption` | `prev_content` absent + `content` present = encryption was just turned on (one-way; Matrix does not support disabling room encryption). | "Alice turned on encryption" |
| `m.space.child` | Comparing `via`/`suggested`/presence of `content` vs `prev_content` distinguishes a room being added to, removed from, or re-flagged (suggested) within a space's hierarchy. | n/a (usually silent, but useful for a space-admin view) |

Membership transitions and power-level diffing are called out as the two
highest-value cases: they're what issue #31 is literally about, and what
mainstream clients already render as timeline notices, so a TUI/web client
gap here is the most visible relative to comparable clients.

### Scope

This ADR covers the **API layer only** (`axon-core`, `axon-sync`,
`axon-store`, `axon-api`) — landing `prev_content` on `EventDto` alongside
the `state_key` field added earlier for issue #31. It does not implement
rendering in any client. Per this repo's one-silo-per-PR convention,
TUI/web consumption of `prev_content` (starting with the membership-rename
case that motivated the issue) is a separate follow-up PR per client.

## Consequences

- `EventDto.prev_content` is `null` on every event today until a client
  starts reading it — purely additive, no behavior change for existing
  clients that ignore the field.
- Clients gain the data needed to render correct Matrix-standard state-event
  transitions instead of the current TUI behavior of collapsing every
  `m.room.member` event with `membership: "join"` into "joined the room."
- No new storage or migration: `prev_content` was already inside
  `raw_event`; this is a projection change plus one addition to the live-sync
  in-memory frame (`LiveEvent.prev_content`).
- The interpretation table above is reference documentation, not an API
  contract — Axon does not promise every listed transition is rendered by any
  given client, only that the raw data to do so is now available uniformly
  across the timeline-read and live-WS paths.
- A client that wants a *diff-free* rendering (just "member state changed")
  can continue to ignore `prev_content` entirely; nothing about this change
  requires updating existing rendering logic.
