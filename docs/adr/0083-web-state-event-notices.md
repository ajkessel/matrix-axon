# ADR 0083 — Web client: readable membership notices and tiered state-event visibility

## Context

The web timeline rendered every state event as a developer string —
`m.room.member (@alice:example.org): …` — and hid all of them behind a single
`Show state events` checkbox that was **off** by default. The result was that
joins, leaves, invites and display-name changes were invisible to normal users,
and the only way to see them was to opt into a firehose that also carried
power-level, ACL and other room-configuration traffic. Two ordinary Matrix
facts about a room ("who is here" and "who is now called what") were
unreachable without also accepting noise.

Two things make this fixable now:

1. **ADR 0074** landed `prev_content` on `EventDto` across both read paths: the
   timeline read (`crates/axon-store/src/events.rs`, projected out of
   `raw_event->'unsigned'->'prev_content'`) and the live `/v1/ws`
   `timeline.event` frame (`crates/axon-api/src/dto.rs`, from
   `axon_core::LiveEvent`). It explicitly scopes rendering to a per-client
   follow-up; this ADR is the web half of that follow-up.
2. Without `prev_content` a client cannot tell a real join from a display-name
   edit — **issue #31**. Both arrive as `m.room.member` with
   `content.membership: "join"`; only the `displayname` field moves. The TUI's
   `display_body_with_sender` (`clients/tui/src/app/render.rs`) still has this
   bug and reports a rename as "joined the room."

Prior art worth matching rather than reinventing: the TUI already tiers state
events. `should_show_event` (`clients/tui/src/app/timeline.rs`) admits
membership events *regardless* of its `show_state_events` toggle, so only the
web client treated membership as noise.

## Decision

### Three tiers, one radiogroup

`SettingsV1.showStateEvents: boolean` becomes
`SettingsV1.stateEvents: 'hidden' | 'important' | 'all'`, surfaced in
Settings → Timeline as a radiogroup (`Hidden` / `Membership and profile
changes` / `All state events`) built from the same `theme-picker` markup the
theme and timestamp-format pickers already use.

A radiogroup rather than the two independent checkboxes the feature was first
sketched as: two checkboxes admit "all state events but not membership," which
is not a state any user wants and which some code would have to silently
resolve. The tiers are genuinely ordered, so an ordered control is the honest
one.

### `important` is membership only

`stateEventTier` returns `important` for `m.room.member` and `other` for
everything else. This is deliberately the TUI's boundary, not Element's: room
name, topic, avatar, power levels, encryption and ACLs stay in `all`. Keeping
one tier definition across the two clients is worth more than matching a third
client's defaults, and the boundary can move later without reshaping the
setting.

A member event whose profile fields did not actually change yields **no
notice**, and the timeline filter drops it. Matrix re-emits such events
routinely; a row saying nothing is worse than no row.

### The client derives the transition; the server does not

Per ADR 0074 the server ships `content` and `prev_content` verbatim and
computes no diff. `clients/web/src/state-event-notice.ts` holds the whole
interpretation as pure functions — `stateEventTier(event)` and
`stateEventNotice(event, resolveName?)` — shared by the timeline filter
(`pages/RoomPage.tsx`) and the renderer (`components/EventBody.tsx`) so a row
can never be shown that the renderer has nothing to say about.

Transitions rendered, from `prev_content.membership` → `content.membership`
with `state_key` as subject and `sender` as actor:

| Transition | Notice |
| --- | --- |
| `*`→`join` (prev not `join`) | *Alice joined the room* |
| `join`→`join`, `displayname` moved | *Alice changed their display name to Alice B* (or set/removed) |
| `join`→`join`, `avatar_url` moved | *Alice changed their profile picture* |
| `join`→`join`, nothing moved | — (row dropped) |
| `*`→`leave`, `sender == state_key` | *Alice left the room* |
| `invite`→`leave`, `sender == state_key` | *Alice declined the invitation* |
| `invite`→`leave`, `sender != state_key` | *Bob withdrew the invitation to Alice* |
| `ban`→`leave`, `sender != state_key` | *Bob unbanned Alice* |
| `*`→`leave`, `sender != state_key` | *Bob removed Alice from the room* |
| `*`→`ban` / `*`→`invite` | *Bob banned Alice* / *Bob invited Alice* |
| `*`→`knock` | *Alice asked to join the room* |

With no `prev_content` — the first member event for a user, or a row from
before ADR 0074 — the new membership alone drives the notice, which is exactly
what the client could say before. Non-membership state events return `null` and
keep today's raw `type (state_key): body` rendering; prettifying those is out
of scope.

**Name resolution** prefers the event's own `content.displayname`, then
`prev_content.displayname`, then the members store, then `@localpart`. The
event comes first because a user who has left the room is no longer in the
roster, but their own membership event still carries the name they had. For the
same reason a *profile change* is described by who the user **was** — "Alice
changed their display name to Alice B", never "Alice B changed their display
name to Alice B".

### Migration in place, not by version bump

`stateEvents` is read from the stored envelope; when it is missing or invalid,
the legacy `showStateEvents` decides: `true` → `all`, anything else →
`important`. The envelope stays at `version: 1`.

Bumping the version would have been the "clean" reshape, but `parse` resets the
*entire* envelope on a version it does not recognize — trading someone's theme,
pinned rooms and composer height for a timeline-filter rename is not a trade
worth making. The legacy key is simply not written back.

Everyone lands on the new `important` default, including users who had
explicitly turned the old checkbox off: that setting meant "hide the raw
firehose", which is not the question the new middle tier asks.

## Consequences

- Joins, leaves and renames are visible by default for the first time, in
  plain English. This is a visible behavior change for every existing user.
- Issue #31 no longer reproduces on the web client. The TUI's copy of the bug
  is untouched — one silo per PR — and now has a reference implementation to
  port.
- The `all` tier is unchanged in content and still shows raw event strings, so
  nothing that was inspectable before became less so.
- `stateEventNotice` returning `null` is load-bearing in two places (filter and
  renderer); a future tier that shows non-membership state events must give
  them notices or keep the raw fallback.
- Notices are English-only string literals, like the rest of the client. When
  the web client grows localization these are among the strings to extract.
