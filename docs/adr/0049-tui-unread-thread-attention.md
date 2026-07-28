# ADR 0049 — TUI unread thread attention

## Context

ADR 0032 added thread rendering to `axon-tui`: thread members are hidden from
the main timeline, thread roots show a summary badge, and opening the thread
panel shows the root plus its members. This keeps busy rooms readable, but it
also creates a new attention problem: a thread can receive new messages without
being obvious once the live promoted member scrolls away or when the activity
arrived in another room.

The current Axon API exposes room timelines, live events, room thread summaries,
and thread timelines. It does **not** expose Matrix read receipts, thread-level
read markers, or persisted unread counts. The TUI already maintains room-level
unread counts locally from live events, so a TUI-local attention marker is
consistent with the existing client behavior.

## Decision

Implement unread thread attention in the TUI silo in two stages.

### Stage 1 — in-room thread markers

The TUI records a session-local unread marker when it observes a live
`m.thread` member and the matching thread panel is not open. The marker is
keyed by `(account_id, room_id, thread_root_event_id)` and stores the unread
count plus the latest observed unread member's sender, body, event id, and
timestamp. It also keeps a bounded newest-first preview cache for the picker
view. The picker spends at most three rendered preview lines per unread thread:
if the newest unread member wraps to three lines, no older member preview is
shown; if it wraps to fewer lines, older unread members fill the remaining
preview budget. Older unread members still contribute to the count.

Thread root badges in the main timeline include the unread count and use the
existing unread-count color with bold emphasis:

```text
↳ 7 replies · 2 new · latest: bob: "I think we should wait…"
```

Opening the thread panel for a root is the TUI's local "read" action and clears
that root's unread marker. Merely switching to the room or refreshing its
timeline does not clear thread unread state, because the thread contents remain
hidden behind the root until the panel is opened. Own live thread replies do not
count as unread.

### Stage 2 — unread-thread picker

Add a dedicated unread-thread picker across rooms. This is not a room-list
filter: it lists thread roots, not rooms. `/unreadthreads` and `/ut` open the
picker, and a configurable shortcut (`shortcuts.unread_threads`, default
`alt-t`) opens the same view.

Rows show the room title, unread count, cached root snippet when available, and
up to three wrapped preview lines from the newest unread members. Within a
thread entry, preview lines render oldest-to-newest so the most recent unread
message sits at the bottom of the preview group. Up/Down navigation moves
between unread thread entries, not the preview lines within an entry. The
preview is intentionally bounded so a hot thread cannot turn the picker into an
unbounded transcript. `Enter` selects a row, switches to that room, loads the
current timeline if needed, and opens the corresponding thread panel. `Esc`
closes the picker.

## Consequences

- This is a TUI-only change. It does not alter `crates/`, `openapi/`, or the
  smoke harness.
- The unread-thread state is session-local. It is useful as an attention aid for
  activity observed by this TUI process, but it is not authoritative Matrix
  unread state and is not guaranteed across restarts or devices.
- Room unread behavior remains room-scoped. A selected room can have no room
  unread badge while still showing unread thread markers for roots the user has
  not opened.
- Future server-side support should replace or seed the TUI-local state when
  Axon exposes read receipts, per-thread read markers, or unread counts. A likely
  shape is to enrich `ThreadSummaryDto` or add a related endpoint with unread
  count and read position metadata. Until then, the TUI must not persist or
  present its local markers as cross-device truth.

## Addendum (reinstatement, M12 integration)

This ADR originally landed in a PR whose base branch was deleted after its
parent merged, which silently auto-closed it (see the stacked-PR
branch-deletion rule in `AGENTS.md`); it was renumbered from 0044 (now taken
by left-room search) on reinstatement.

The reinstated implementation narrows one Consequence above: unread-thread
markers are no longer populated *only* by live observation. The M12 room read
marker (ADR 0048) tells the timeline-load path exactly which thread replies
this device has never seen, so replies that arrived while the TUI was down now
count toward their root's badge and the picker on the next room entry (own
messages excepted, matching the live path). The markers themselves remain
session-local and per-device; making them durable and cross-device by syncing
them through an M12 `thread_read_markers` device-state namespace is the
natural follow-up, superseding the "future server-side support" shape above.
