# Client Feature Parity

A human-centric, cross-silo tracker for one recurring problem: Axon ships a
server capability, one client adopts it, and there is no single place that
says whether the *other* clients ever caught up. This surfaced concretely in
review of issue #279 (Adam's comment): TUI had no tracking against web once
web existed, web now reads ephemeral indicators (typing, receipts) that TUI
still doesn't, and interactive SAS device verification shipped a full UI in
TUI but never got one in web.

This doc is **not** auto-generated and does not replace `AGENTS.md`'s
"Current state" (server-side landing history) or the ADR log (design
decisions). It answers one question per row: *for a capability the server
already exposes, which clients actually surface it to a user?*

## How to maintain this

- Update the row for a capability in the **same PR** that changes its status
  in any silo (`crates/`, `clients/tui/`, `clients/web/`) — per AGENTS.md's
  `docs/` cross-silo exception, this file can land alongside a change in any
  one silo without violating the one-silo-per-PR rule.
- If you don't know a cell's true status, write "needs confirmation" and say
  why, rather than guess. A wrong "Done" is worse than a known gap — the
  whole point of this doc is to stop parity drift from going unnoticed.
- Add a row as soon as a new server capability is designed (even before it
  lands), so the client-consumer gap is visible from day one instead of
  discovered later.

**Legend:** Done · Gap (server has it, this client doesn't) · Planned
(designed, not landed) · Not started · Deferred (deliberately not building
yet)

## Matrix

| Capability | Server (`/v1/`) | axon-tui | axon-web | iOS (future) | Notes |
|---|---|---|---|---|---|
| Text send / edit / redact / react | Done (M6, ADR 0021) | Done | Done | Not started | |
| Media send (`m.image`/`m.file`) | Done (M15, ADR 0059) | Done | Done | Not started | |
| Media read proxy + LRU cache | Done (M11, ADR 0045) | Done | Done | Not started | |
| Media thumbnail proxy | Done (M17, ADR 0063) | **Gap** — client-side downscale only; doesn't call the server endpoint | Done | Not started | Called out explicitly in AGENTS.md's M17 note |
| Full-text search | Done (M9) | Done (minimal input, per MVP scope) | Done | Not started | |
| Drafts (cross-device) | Done (M12, ADR 0048) | Done | Done | Not started | |
| Read markers (cross-device, Axon-internal) | Done (M12, ADR 0048) — the underlying `device_state` store was dropped by a force-push and restored in PR 226; the TUI read-marker feature itself was separately reverted (`116b3cb`) and re-landed in PR 217 (`clients/tui/src/app/read_markers.rs`) | Done | Done, plus a `thread_read_markers` namespace TUI doesn't have | Not started | Reverse gap: web is ahead here |
| Inbound ephemeral passthrough (typing, receipts) | Done (M18, ADR 0056) | Done (`app/ephemeral.rs`; typing + read receipts shown as a bottom-border status on the message pane) | Done (`stores/ephemeral.ts`) | Not started | Adam's motivating example |
| Outbound read receipts to homeserver | Done (#278, ADR 0067) — `POST .../rooms/{room_id}/read` | Done — second fire-and-forget spawn in `read_markers::spawn_marker_put` | Done (`stores/ephemeral-sender.ts`, fired from `RoomPage`'s read-marker choke point) | Not started | Both piggyback on their client's existing debounced, forward-only read-marker choke point; fire-and-forget |
| Outbound typing notice | Done (M19a, ADR 0068) — `PUT .../rooms/{room_id}/typing` | Done — `app/typing.rs`, driven by `note_draft_activity` + a `flush_due_typing` tick | Done (`stores/ephemeral-sender.ts`, driven by the composer) | Not started | Throttled true, cleared on empty/command/submit/room-switch/idle |
| Interactive SAS device verification | Done (7a-6, ADR 0027/0028) | Done — full emoji-modal flow | **Gap** — `AccountsPage.tsx` only mentions SAS in a placeholder label; no verification flow implemented | Not started | Verified by grep, 2026-07-17 |
| Device-list / picker endpoint | Done (M16, ADR 0060) | **Gap** — no picker UI; verification still requires a blind device id | **Gap** — endpoint appears only in generated `schema.d.ts`; no picker component consumes it | Not started | The exact gap M16's own note anticipated |
| Room membership (leave/forget/invite/kick/ban/unban) | Done (M19b, ADR 0068) — `POST .../rooms/{room_id}/{leave,forget,invite,kick,ban,unban}` | Not started | Not started | Not started | Server-only; client UI (leave-room action, member-list kick/ban) is separate follow-up work |
| Room entry (join/knock/create) | Planned (M19c, ADR 0068) | Not started | Not started | Not started | |
| Room settings (name/topic/avatar/tags) | Planned (M19d, ADR 0068) | Not started | Not started | Not started | |
| Power levels | Planned (M19e, ADR 0068) | Not started | Not started | Not started | |
| Account actions (profile/ignore/directory search) | Planned (M19f, ADR 0068) | Not started | Not started | Not started | |
| Invited-room visibility (see incoming invites, accept/reject) | Not designed — largest remaining Tier-C gap, issue #279 | Not started | Not started | Not started | Recommended as the milestone after M19 |
| Presence (inbound + outbound) | Deferred (ADR 0056) | Not started | Not started | Not started | No ADR planned until the lag question is addressed |
| Unread / notification counts | Not designed — Tier-C, issue #279 | Not started | Not started | Not started | Independent of #241 (account sync-readiness); needs SDK per-room summary plumbing |

## Out of scope for this doc

- Server-only infrastructure with no client-visible surface (search index
  internals, backfill, account lifecycle state machine internals) — those
  live in `AGENTS.md`'s "Current state" section.
- Design decisions and rationale — those live in `docs/adr/`.
