# ADR 0034 — TUI feature roadmap: post-media gap analysis

## Context

With the `tui-media` branch landing authenticated MXC proxy rendering (the
preparatory slice from #70), the TUI enters a phase where the API surface has
significantly outpaced the client implementation. This ADR maps the remaining
gaps — features fully or partially supported by the server that `axon-tui` does
not yet expose — and records the agreed priority order so that future TUI work
has a sequenced roadmap rather than an ad-hoc backlog.

The analysis compared every route and WebSocket frame in
`crates/axon-api/src/routes/` and `crates/axon-api/src/ws.rs` against the
corresponding client code in `clients/tui/src/`.

**Last updated:** 2026-06-22 to reflect work that has landed since the initial
draft (PRs #119, #124/#133).

## Decision

### Completed items

#### ✅ 1. SAS device verification — TUI implementation (was HIGH)

Landed in PR #119 (`feat(tui): SAS emoji device verification + sender-trust
indicators`). The full ADR 0028 UX is implemented: bi-directional SAS flows,
auto-opening modal on `verification.requested`, `y`/`n`/`Esc` confirmation,
and read-on-reconnect via `GET …/verify/{flow_id}`.

#### ✅ 2. Sender trust indicators (was HIGH, paired with #1)

Landed alongside SAS verification in PR #119. Per-message trust glyphs based
on `sender_trust` (values: `verified`, `unverified`, `unknown`,
`violation`) are rendered, and `sender_trust.violation` WebSocket frames
surface a visible alert. The `sender_trust` field lives on `EventDto` (not
`ReactionTally`).

#### ✅ 3. Formatted message sending (not in original draft)

Landed in PRs #124 / #133 (`feat(tui): formatted message sending — markdown,
/html, /rainbow, /spoiler`). Covers:

- **Markdown auto-detect**: plain messages are run through pulldown-cmark; when
  non-paragraph formatting is found (bold, italic, code, lists, etc.) the
  message is sent with `format`/`formatted_body` automatically.
- **`/html <content>`**: send raw HTML as `formatted_body`.
- **`/literal <text>`**: bypass markdown conversion.
- **`/rainbow <text>`**: per-character HSL color cycling via `<font color>` →
  `<span data-mx-color>`.
- **`/spoiler [reason |] <text>`**: wraps in `<span data-mx-spoiler>`.
- **Receive side**: `data-mx-color` and `data-mx-spoiler` span attributes are
  now rendered in the TUI (colored text; `[spoiler]` prefix with dimmed
  content).

### Gap inventory (remaining, in priority order)

#### 1. Reply rendering and sending (MEDIUM)

ADR 0033 (M8) landed server-side relation aggregation and the API layer (8b)
exposes reply counts and parent references in the timeline DTO. However,
`SendMessageRequest` does not yet accept `relates_to`, so the send path cannot
be wired.

TUI stubs remain in place: `/reply` command parses and targets the selected
message (`app.rs:895–910`), but the send step emits a "waits for Axon write
API" status message. Once the server accepts `relates_to` on
`SendMessageRequest`, the remaining TUI work is:

- Render `m.in_reply_to` events with an indented quote block.
- Wire `/reply` to pass the target event ID in the send request.

#### 2. Thread sending (LOW)

Same blocker as replies — `SendMessageRequest` has no `relates_to` field.
TUI stub at `app.rs:935–952`. Defer until after replies are complete.

### Features not blocked on this roadmap

- **Room join / leave / create**: No API endpoints exist yet. Out of scope.
- **Device list / picker** (issue #84): Required for smooth outgoing SAS
  verification but deferred per ADR 0028 decision (1).
- **Media**: Fully landed via `tui-media` (image thumbnails, Sixel/Kitty/
  halfblock auto-detect, wide-screen preview popup, EXIF rotation, encrypted
  MXC proxy).
- **Formatted sending**: Landed (see completed items above).

## Consequences

- Work order: reply send + render (#1) → thread send (#2).
- Both remaining items (#1 and #2) are blocked on `SendMessageRequest`
  gaining a `relates_to` field server-side before TUI work can proceed.
- ADR 0028 remains the authoritative spec for the (now-complete) SAS
  verification UX.
- ADR 0033 remains the authoritative spec for the relation aggregation backend
  that the reply/thread TUI work will consume.
