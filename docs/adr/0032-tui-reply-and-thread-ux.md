# ADR 0032 — Reply and thread UX in axon-tui

## Context

Matrix defines two distinct relation shapes that together cover the space of
"contextual messages":

- **Replies.** A reply event carries `m.relates_to.m.in_reply_to.event_id`
  pointing at the replied-to event. There is *no* `rel_type`. This shape is
  specified in MSC1767 / the current Matrix spec and is universally supported
  by homeservers and most clients.

- **Threads.** A threaded message carries `m.relates_to` with
  `rel_type: m.thread` and an `event_id` pointing at the thread root. Threads
  were finalized in MSC3440 / Matrix 1.3. They are a superset of replies —
  the spec allows a threaded message to also carry a fallback `m.in_reply_to`
  for older clients, but the canonical relation is the `m.thread` one.

`EventDto` already carries `relates_to: Option<Value>` (the full raw block,
from `events.relates_to` in the store — ADR 0015). The TUI has stub handlers
for `/reply`, `/thread`, and their hotkeys, all gated with "pending API
support" messages. The rendering pipeline (`message_display_lines`) currently
handles edits (collapsed server-side as of M5) and reactions (an emoji bar
below the message), but has no special handling for either relation shape.

M8 (Relation aggregation, `implementation.md`) defines the backend work that
makes cross-window resolution possible: indexed lookups over both relation
shapes and new API endpoints (`GET .../events/{event_id}/replies`,
`GET .../rooms/{room_id}/threads`,
`GET .../rooms/{room_id}/threads/{root_id}/timeline`). **M8b landed in PR #116 (2026-06-21).** All five relation-read endpoints
(`/reactions`, `/replies`, `/edits`, `/threads`, `/threads/{root_id}/timeline`)
are live, and `EventDto` now carries the resolved aggregation fields on the
timeline response. The M3 backend gate is met.

**What "in the loaded slice" means for the TUI.** When a room is selected,
`load_selected_timeline` fetches `TIMELINE_LIMIT` (currently 50) events and
stores the full Vec in `MessagePane.events`. The `scroll` field controls only
which portion of that Vec is visible in the viewport — all 50 events are in
memory regardless of scroll position. "Off-screen" (scrolled out of view) is
therefore *not* the same as "not cached"; a replied-to event is available for
reply-context rendering as long as it is within the 50-event loaded window.
The context-not-loaded fallback only fires for events older than that window
boundary.

Matrix clients have a poor collective track record on reply and thread UX.
Common failure modes: replies with no context when the original is off-screen,
threads that expand inline and bury the main timeline, thread roots that show
no indication a thread exists, and thread views you cannot exit. This ADR
records design decisions for all four implementation phases before code is
written, so the team can iterate on the design cheaply.

---

## Implementation strategy update (2026-06-22)

With M8b now live, the original three-phase sequencing (M1 → M2 → M3 gated on
backend) collapses. M1 and M2 were designed to ship without backend support,
with M3 as a later upgrade pass. Since the API is available from the start,
**M1, M2, and M3 should be implemented together in a single PR.** Concretely:

- The `[reply context not loaded]` placeholder defined in M1 need never ship as
  live behavior — fold the M3 fetch alongside M1's rendering so out-of-window
  replies resolve immediately via `GET …/events/{event_id}/replies`.
- The in-slice-only `↳ N replies` count from M2 should use the server-aggregated
  total from the start, sourced from the enriched `EventDto.reactions`-equivalent
  thread fields rather than a post-hoc upgrade.
- The thread panel's paging via `threads/{root_id}/timeline` (M3) can be wired
  at the same time as the panel itself (M2).

M4 (sending replies/threads) retains its separate gate and is unchanged.

---

## Four milestones

The milestones below reflect the original design. M1+M2+M3 are now implemented
together; see the strategy update above.

### M1 — In-window reply rendering (no backend dependency)

**Gate:** None. The `relates_to` field is already in `EventDto`.

When a message has `m.in_reply_to` in its `relates_to`, the TUI searches the
**full loaded slice** (`selected_raw_events()`, which returns all events in
`MessagePane.events` for the room — not just the visible viewport) for the
replied-to event. If found anywhere in that 50-event cache — whether on-screen
or scrolled out of view — the context is rendered. If the replied-to event is
older than the loaded window, a compact fallback line is shown. No new API
calls in M1.

#### Reply rendering design

**Chosen approach: a dim context line before the reply body, aligned to the
body column.**

```
  10:23:45 alice: Hello world, how are you doing today?
  10:24:12 bob:
           ↩ alice: "Hello world, how are you doing…"
           Actually I disagree
```

- The context line is prefixed with `  ↩ ` (two spaces + return-arrow +
  space), aligning with the body text column.
- The quoted body is truncated at ~60 characters with an ellipsis, displayed
  in a dim/italic style (using `colors.input_hint` or a new palette entry).
- The reply itself renders normally on the line below, with the sender and
  timestamp on the first line and the body indented as usual.
- When the replied-to event is **not** in the loaded 50-event cache, the
  context line reads `↩ [reply context not loaded]` in the dim style — a
  clear signal rather than silent omission. This placeholder is replaced when
  M3 resolves it from the API.

**Rejected alternatives:**
- **Inline block with vertical bar** (Element-style):
  `│ alice: Hello world…` indented inside the reply. This takes an extra line
  and the `│` character is easily confused with multi-line message wrapping.
- **Inline prefix on the reply body line**:
  `[↩ alice: Hello world…] Actually I disagree`. Cramped, hard to read for
  longer original messages.
- **No visual distinction** — replies appear as bare messages with no context.
  This is the failure mode we are trying to avoid.

#### What changes in M1

- Add `reply_relation() -> Option<&str>` method to `EventDto` (analogous to
  `edit_relation` and `reaction_annotation`): returns the `m.in_reply_to`
  target event ID if present and there is no `m.thread` `rel_type` (to avoid
  double-processing a threaded message's fallback reply target).
- `message_display_lines` gains an optional pre-body context line for reply
  events, rendered only when the replied-to event is found in the supplied
  full slice (or the fallback placeholder).
- `message_display_line_count` updated to account for the added line.
- No new API calls, no new app state fields.

---

### M2 — In-window thread rendering (no backend dependency)

**Gate:** None. `m.thread` `rel_type` is already in `relates_to`.

When the loaded timeline slice contains messages that share an `m.thread`
root, the TUI surfaces thread membership without restructuring the timeline
order or requiring any API calls.

#### Thread rendering design

This is the decision that most Matrix clients get wrong. The failure modes
are:

1. Threads expand inline, consuming vertical space proportional to reply count
   and burying the main conversation.
2. Thread roots show no indication a thread exists until the user happens to
   select that message.
3. A thread view exists but traps the user with no clear exit.

**Chosen approach: thread replies are only shown in the thread panel; the
main timeline shows a summary badge on the thread root.**

**Team discussion resolved:** thread replies do *not* appear inline in the
main timeline. They are only accessible via the thread panel (described
below). The rationale: a room with active threads becomes unreadable if every
threaded reply also appears in the main timeline; the badge approach keeps the
main conversation legible while signaling that discussion exists.

**In the main timeline,** a thread root (any event whose `event_id` is the
`m.thread` root for one or more later events in the slice) gets a summary
line below the message:

```
  10:23:45 alice: Let's discuss the deployment plan
           ↳ 3 replies · latest: bob: "I think we should wait…"
```

- The summary line is `  ↳ N replies · latest: @sender: "body…"` (dim style,
  truncated to ~50 chars of body).
- In-slice count only in M2; server-aggregated count in M3.
- Thread replies that are *not* the root are filtered from the main timeline
  (`should_show_event` gains a thread-filter pass).

#### Live update of thread badges

When a new threaded message arrives via the WebSocket fanout, the TUI must
update the thread root's badge — both the reply count and the "latest:"
excerpt — without waiting for the user to switch rooms and back. This is the
same path as reaction deltas today: the WS event lands in the live handler,
the room's stored events are mutated in-place (or the badge is derived from
the stored slice on next render), and the screen redraws on the next tick.

Concretely: a new `m.thread` message received over WS is appended to the
stored slice for its room. Because the badge is computed from the stored slice
at render time (not cached separately), the badge updates automatically on the
next frame. No separate badge-state cache is needed.

**Unread counting.** Thread replies count toward the room's unread marker
the same as any other message. Hiding them from the main timeline does not
mean hiding them from the unread count. A room with 5 unread thread replies
should show 5 on the unread badge.

**Thread panel:** activating the thread hotkey (or `/thread`) while the
cursor is on a thread root or any member of that thread opens a **thread
panel**. The thread panel:

- Replaces the message list area (same panel, same scroll bindings — no
  new layout splits in M2 to keep scope tight).
- Shows the thread root at the top (with a `[thread root]` label), followed
  by the in-slice thread replies in chronological order.
- Displays the room title bar with a `[in thread]` indicator so the user
  always knows their context.
- `Escape` exits the thread panel and returns to the main timeline at the
  same scroll position.
- New thread replies that arrive via WS while the thread panel is open are
  appended to the panel view live (same redraw path as above).
- Replying while in the thread panel (M4) targets the thread, not the room.

Activating the thread hotkey on a non-root thread member (which would only
be reachable within the thread panel itself) navigates within the panel rather
than opening a new panel. There is only one level of thread nesting.

#### What changes in M2

- Add `thread_relation() -> Option<&str>` to `EventDto`: returns the root
  `event_id` for `m.thread` events.
- App state gains `thread_panel: Option<String>` (active thread root event
  ID; `None` when in the main timeline).
- `should_show_event` filters out thread non-root events when
  `thread_panel.is_none()`.
- `message_display_lines` appends the `↳` summary badge to thread roots,
  counting in-slice thread members. Computed from the stored slice at render
  time — no separate cache.
- A `thread_display_lines` path renders the thread panel view (root + members
  in chronological order).
- `Escape` from thread panel clears `thread_panel` and restores the main
  timeline view.
- Unread counts are not modified — thread replies already count as regular
  events through the existing path.
- `popup_shortcuts_lines` updated to reflect the thread panel context (per
  project convention — see memory).

---

### M3 — Cross-window reply and thread resolution (gate met — PR #116)

**Gate: met.** M8b landed in PR #116 (2026-06-21). All endpoints are live:
- `GET /v1/accounts/{account_id}/events/{event_id}/replies`
- `GET /v1/accounts/{account_id}/rooms/{room_id}/threads`
- `GET /v1/accounts/{account_id}/rooms/{room_id}/threads/{root_id}/timeline`
- `EventDto` carries resolved aggregation fields (see AGENTS.md M8b notes).

Per the strategy update above, M3 is implemented alongside M1 and M2 rather
than as a follow-on pass. The same visual shapes apply; what changes is that
the API is wired from the start rather than added later:

- The `[reply context not loaded]` placeholder from M1 is replaced by a live
  fetch of the replied-to event when it is not in the current slice.
- The `↳ N replies` count on thread roots reflects the server-aggregated
  total, not the in-slice count.
- The thread panel (M2) can page through the full thread timeline via the
  `threads/{root_id}/timeline` cursor endpoint, not just what is in the room
  slice.
- `GET .../rooms/{room_id}/threads` can populate a thread list, enabling a
  future "show all threads in room" view (out of scope for this milestone).

Because M8b is now live and M1+M2+M3 are implemented together, no runtime
capability gate is needed for the initial implementation. A degradation path
(falling back to in-slice-only behavior on `404`) remains advisable for
forward-compatibility with test environments running older server builds.

---

### M4 — Sending replies and threads (gated on send-path `relates_to` support)

**Gate: the M6 send endpoint must accept `m.relates_to` in the request body
and forward it in the Matrix event.** This is currently stubbed; `app.rs`
holds the event ID in a debug message but never sends it.

Once the send gate is met, the existing stub methods
(`start_reply_to_selected_message`, `start_thread_from_selected_message`) can
be fully wired. The UX:

#### Compose-area reply indicator

When the user invokes `/reply` or the reply hotkey on a selected message, the
compose area transitions to reply mode:

```
  ╭─ Replying to alice: "Hello world, how are you doing…" ─[Esc to cancel]─╮
  │ _                                                                        │
  ╰──────────────────────────────────────────────────────────────────────────╯
```

- A single-line header above the input box identifies the target and allows
  cancellation via `Escape`.
- The header is styled with the dim reply color from M1.
- `send_message` attaches `m.relates_to: { m.in_reply_to: { event_id: … } }`
  to the outgoing payload.
- Cancelling (Escape or sending) clears reply mode and returns to normal
  compose.

#### Compose-area thread indicator

Thread is analogous:

```
  ╭─ Replying in thread: alice: "Let's discuss the deploy…" ─[Esc to cancel]─╮
  │ _                                                                          │
  ╰────────────────────────────────────────────────────────────────────────────╯
```

- `send_message` attaches `m.relates_to: { rel_type: m.thread, event_id: <root_id>, … }`.
- When invoked from inside the thread panel (M2/M3), the root is the panel's
  active thread root.
- When invoked from the main timeline on a thread root, the root is that
  event's ID.
- When invoked on a non-root thread member (visible in the thread panel),
  the root is propagated from the thread context, not the selected message.

#### App state changes for M4

- `pending_reply: Option<String>` (event ID) and
  `pending_thread: Option<String>` (root event ID) in app state.
- They are mutually exclusive; setting one clears the other.
- `send_message` reads these to populate `relates_to` in the request body.
- The compose box renders the header line when either is set.
- `Escape` clears both (already the cancel-compose behavior; this is
  additive).

---

## Consequences

- M1+M2+M3 are implemented together in a single PR (see strategy update
  above). The API is live as of PR #116, so there is no intermediate "in-slice
  only" shipped state.
- The 50-event cache means most reply context would have been resolvable even
  without M3; cross-window fetch is the fallback for genuinely old events.
- Thread replies are hidden from the main timeline, which is a deliberate
  tradeoff: cleaner main timeline at the cost of replies being one panel-open
  away. The badge and live WS updates keep the user informed without flooding
  the room view.
- A soft degradation path (fall back to in-slice behavior on `404`) is
  advisable for compatibility with older server builds in test environments,
  but is not required for production since M8b is live.
- M4 requires the M6 send path to accept `relates_to`. That change is
  straightforward on the server (pass-through field) but must be coordinated
  — the client stub should not be wired until it is confirmed.
- The `EventDto` aggregation fields for M3 (reply counts, thread summaries,
  etc.) are documented in AGENTS.md under the M8b notes.
