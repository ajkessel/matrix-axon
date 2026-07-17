# ADR 0061 — Web client live WebSocket layer (M-W6)

## Context

M-W6 (ADR 0046 roadmap) turns the read-only browser client into a live one: one
authenticated `/v1/ws` socket, a frame router, reconnect with backoff, a
connection-state indicator, gap-fill on reconnect, live-fed unread, and
cross-device drafts + read markers — with the first Playwright lane as the exit
proof (two tabs see each other's messages live). It is a single **web silo**;
the server side of `/v1/ws` already exists (ADR 0020 fan-out, ADR 0029 auth, ADR
0048 device state, ADR 0056 ephemeral passthrough).

The wire contract is settled and consumed nowhere yet on the web:

- **Envelope.** Every frame is `{ "type", "account_id", "payload" }`
  (`crates/axon-api/src/ws.rs`). The `type` tag is namespaced —
  `timeline.event`, `verification.{requested,sas,done,cancelled}`,
  `sender_trust.violation`, `device_state.changed`, `ephemeral.passthrough`.
- **Auth.** No `Authorization` header is possible on a browser socket, so the
  token rides in `Sec-WebSocket-Protocol` as `bearer.<token>` (ADR 0029).
- **Delivery is lossy.** A single global `broadcast` bus with no per-socket
  filtering: a slow consumer is `Lagged` and *skips* frames; a reconnecting
  client *misses* in-flight frames. Clients self-filter by `account_id` and
  re-read on reconnect. There is no resume cursor (open question #1, ADR 0046).

Two facts about the current tree shape this plan:

- **The #238 fix is only half-landed.** `crates/axon-api/src/ws.rs` now echoes
  the benign `axon` subprotocol *when the client offers it* (commit `3ee4c541`),
  but the browser helper `wsAuthProtocols` (`clients/web/src/api/ws.ts:36`)
  still offers only `bearer.<token>`. A real browser therefore still gets no
  negotiated subprotocol and Chrome still fails the handshake. The client half
  is a prerequisite of everything below, not a separate deliverable — it is inert
  until a socket consumes it.
- **The consumers are already wired, waiting for the feed.** `createUnreadStore`
  (`stores/unread.ts`) exposes `recordEvent`/`markSeen` with a doc-comment
  naming "the M-W6 WS layer's feed point"; `createTimelineStore` exposes
  `loadLatest()` and reconciles by event id already (for local echo). M-W6 adds
  the transport that drives them, not new store surface.

## Decision

### One socket per instance, owned by a `LiveConnection` service

The bus is global and frames carry `account_id`, so a single socket serves every
account — mirroring the TUI and ADR 0020. A new `createLiveConnection` service
joins the `AppServices` graph (`services.ts`), constructed after auth like the
API client. It owns the `WebSocket`, the reconnect timer, the connection-state
signal, and the frame router; components never touch a socket directly. Tests
build it over a fake socket, the same way the graph is built over msw today.

### Complete the `axon` subprotocol offer (first commit)

`wsAuthProtocols(token)` returns `['axon', \`bearer.${token}\`]`. The benign
`axon` entry is offered first so the server has something to echo in the 101;
the token-bearing entry is never echoed (server-enforced, ADR 0029/#238). Its
test updates to assert the two-entry order, and the stale "KNOWN CAVEAT for
M-W6 … Resolve before M-W6" block at the top of `ws.ts` is replaced with the
resolved contract. This lands first because the socket cannot open in Chrome
without it.

### A central frame router, dispatch by `type`, unknown tags ignored

The connection decodes the envelope once and dispatches on the `type` tag to
registered handlers, so new frame kinds never touch transport or reconnect code:

- `timeline.event` → append to the open room's timeline (reconcile by event id,
  reusing the local-echo dedupe path) **or**, for any other room, count it via
  `unread.recordEvent`. The open room both appends and stays seen.
- `device_state.changed` → apply to the device-state store (drafts + read
  markers), suppressing frames whose `device_id` is our own (ADR 0048).
- `verification.*` / `sender_trust.*` → routed to registered handlers now, but
  the SAS UI and trust display land in **M-W9**; M-W6 ships the seam (and the
  reconnect re-read below), not the ceremony.
- `ephemeral.passthrough` → route live `m.typing` and `m.receipt` frames to an
  in-memory overlay store. Presence stays deferred until the server forwards it
  from production and the product semantics are defined.

### Reconnect: exponential backoff 1 s → 30 s, matching the TUI

On unexpected close, reconnect after a delay that doubles from 1 s to a 30 s cap
and resets to 1 s on a successful open, matching the TUI's policy. The socket
re-authenticates with the *current* token each attempt (revocation is
out-of-process; a revoked token yields a close and the client stops retrying and
surfaces a re-auth prompt rather than spinning). A deliberate logout/teardown
cancels the timer instead of reconnecting.

### Connection state is a first-class signal in the shell

A `connection: ReadonlySignal<'connecting' | 'live' | 'reconnecting' | 'offline'>`
drives a small indicator in the app shell. `offline` is the terminal
(auth-failed / torn-down) state; `reconnecting` covers the backoff window. This
is the user-visible half of the exit criterion — a tab must *show* that it is
live.

### Gap-fill on reconnect: refetch the open room's head, reconcile by id

Because the bus has no resume cursor, a reconnect refetches only the **open**
room's timeline head (`timeline.loadLatest()`) and reconciles by event id — the
dedupe path local echo already uses — so a gap during a drop is healed for the
room the user is looking at. This is the client-side gap-fill ADR 0046 open
question #1 commits to; a server-side `since` cursor is explicitly **not** built
here (it would benefit the TUI too and is a separate server silo if ever taken
up). Rooms not open rely on the next live frame plus the unread count; their full
history is available on demand via existing pagination. Reconnect also re-reads
the two other replay-sensitive surfaces: any in-flight **verification** flow
(re-readable via `GET …/verify/{flow_id}`, ADR 0027 — the seam M-W9 builds on)
and **device state** (a GET-before-PUT refresh of drafts/read markers, ADR 0048).

### Drafts and read markers over device state (ADR 0048)

Two `device_state` namespaces, LWW-merged server-side:

- **Read markers** carry the client's read position per room; combined with the
  client-derived unread store they mark a room seen across a user's devices.
- **Drafts** persist the composer per room.

The web client mints a **device UUID once and persists it in `localStorage`**
(implicit registration on first PUT, ADR 0048 — no registration endpoint). The
store GETs the merged view at startup and on reconnect, PUTs debounced on change,
and applies `device_state.changed` frames with own-`device_id` echo suppression.
Read markers are the mechanism behind "read-on-reconnect refresh"; drafts reuse
the same transport, so they ship together.

### First Playwright lane

A `two-tabs-see-each-other-live` spec is the exit proof and the first e2e lane:
tab A sends (M-W7, already merged), tab B — subscribed via this layer — renders
it live without reload; a forced socket drop shows `reconnecting` then heals via
gap-fill. The lane follows the M-W1 workflow convention (`workflow_dispatch` if
it needs a GitHub-hosted browser runner) so it does not gate every push.

### Implementation sequence (commit stack, one web-silo PR or short stack)

1. **`axon` subprotocol offer** — `wsAuthProtocols` + test + retire the caveat.
2. **`LiveConnection` service + frame router** — decode, dispatch, `connection`
   signal; `timeline.event` → timeline/unread; unknown tags dropped. Fake-socket
   unit tests.
3. **Reconnect + backoff + gap-fill** — 1 s→30 s, open-room head refetch and
   reconcile, verification + device-state re-read on reconnect.
4. **Connection-state indicator** in the shell.
5. **Device-state store** — device UUID, drafts + read-marker namespaces,
   debounced PUT, `device_state.changed` handling with echo suppression.
6. **Playwright lane** + CI workflow.

## Consequences

- **The #238 fix becomes real.** Only after commit 1 does a Chrome browser
  actually complete the handshake; the server-side `3ee4c541` was necessary but
  not sufficient on its own.
- **Live read parity, and M-W7 sends light up.** M-W7 merged ahead of M-W6, so
  its sends currently reach peers only via their own REST reads; this layer is
  what makes two tabs see each other live — the milestone's exit criterion — and
  retroactively completes the M-W7 experience.
- **The router decouples client features from the frame roadmap.** New frame
  kinds (the M-W9 verification/trust frames, or any future `ephemeral`) attach a
  handler without touching transport, reconnect, or auth — the inbound analogue
  of ADR 0056's decoupling goal.
- **Loss is tolerated by design, not eliminated.** Gap-fill covers the open room;
  non-open rooms can still miss a frame and rely on unread + on-demand
  pagination. Accepted for MVP; the server-side resume cursor remains the lever
  if this proves too lossy in practice.
- **Verification and trust UI stay in M-W9.** M-W6 ships the frame seam and the
  reconnect re-read so M-W9 is pure UI; no SAS ceremony logic lands here.
- **Typing / receipts are live overlays.** `ephemeral.passthrough` now feeds
  room typing indicators and public read receipts in memory only. Presence
  remains out of scope.
- **One new persisted client identifier.** The `localStorage` device UUID is the
  first piece of durable client identity in the web app; it is account-neutral
  (one install = one UUID across accounts, ADR 0048) and carries only opaque
  drafts/read-markers behind the existing token trust boundary.
