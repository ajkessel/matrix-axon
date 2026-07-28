# ADR 0056 — Live-event passthrough: fanning out the ephemeral long tail

## Context

The live-event bus is Axon's only push channel to clients. The sync engine
publishes [`LiveFrame`](../../crates/axon-core/src/live.rs)s onto a single
`tokio::sync::broadcast`, and the `/v1/ws` handler
([`crates/axon-api/src/ws.rs`](../../crates/axon-api/src/ws.rs)) fans them out as
tagged JSON envelopes (`type` + `account_id` + `payload`). Today the enum carries
these value-added kinds:

- `Timeline` — a freshly persisted timeline event (decrypted, with a
  `sender_trust` verdict snapshot).
- `Verification` — a stage in an interactive SAS flow (rendered emoji/decimals).
- `SenderTrustChanged` — an M7c sender device-trust overlay.
- `DeviceState` — an M12 per-device drafts/read-marker change (ADR 0048).

All are *value-added*: Axon decrypts, derives a trust verdict, renders SAS
data, or persists device state the raw event doesn't carry on its own. They
are produced by the engine's persistence handlers (`engine.rs`), which
register only for **persisted** data — timeline events, room state,
room/global account data, and per-device state.

Everything *ephemeral* is dropped on the floor. The homeserver delivers, via the
sync response, a whole class of live signals Axon never observes or forwards:

- **Typing** (`m.typing`) — who is composing in a room.
- **Read receipts** (`m.receipt`: `m.read`, `m.read.private`, `m.fully_read`) —
  read state and "seen by".
- **Presence** (`m.presence`) — online/offline/unavailable, last-active, status.
- **Unread / notification counts** (`unread_notifications` in the sync response).

A client (`axon-tui` is the immediate one) cannot show a typing indicator, a
read-receipt marker, or a presence dot, because the data never leaves the sync
engine. The naive fix — a bespoke `LiveFrame` variant per signal — means each new
ephemeral touches four crates (core enum, sync producer, api wire tag, client
consumer), which is the same per-endpoint tax that
[issue #130](https://github.com/matrix-axon/matrix-axon/issues/130) identifies on
the *request* side.

This ADR is the **inbound dual of #130**. #130 proposes a generic *request*
passthrough (client → homeserver, request/response) so clients aren't blocked on
Axon shipping a wrapper for every CS-API call. A request passthrough cannot solve
this problem: typing/receipts/presence arrive **unsolicited** via sync, not in
response to a client call. The push direction needs its own escape hatch. Taken
together the two ADRs form a symmetric pair — generic requests out, generic live
events in — layered under the value-added typed routes and frames.

## Decision

### A generic `Ephemeral` frame for the pure-overlay long tail

Add one new variant to `LiveFrame`:

```rust
LiveFrame::Ephemeral(EphemeralFrame)

struct EphemeralFrame {
    account_id: Uuid,
    room_id: Option<String>,   // None for account-scoped signals (presence)
    event_type: String,        // "m.typing", "m.receipt", "m.presence", …
    content: serde_json::Value, // the raw event content, unmodified
}
```

It serializes onto the wire as `type: "ephemeral.passthrough"` with the raw
`content` in `payload`, alongside the existing typed encoders in `encode_frame`
(`ws.rs`). The sync engine subscribes to ephemeral EDUs (and the
`unread_notifications` / presence sections of the sync response) and forwards any
event of an allowed type, **unmodified**, onto the bus.

The frame is deliberately wire-neutral and thin: Axon adds no value to these
events, so it forwards them verbatim rather than reshaping them into a typed DTO.
This is the inbound analogue of #130's "raw method/path/body" option, and the
trade-off lands differently here (see below) — in this direction the thin proxy
is unambiguously the right call.

### The typed frames stay first-class — generalization is additive

`Timeline`, `Verification`, and `SenderTrustChanged` are **not** folded into the
generic frame. They carry Axon-derived value (decrypted content, the
`sender_trust` snapshot, rendered SAS emoji) that a raw passthrough would strip.
The split mirrors #130's own distinction: bespoke handlers for anything Axon adds
value to, one generic escape hatch for the long tail. A signal graduates from
`Ephemeral` to a typed frame the day Axon starts deriving something from it — not
before.

### Why the inbound passthrough is *safer* than #130's outbound one

#130's hardest open questions largely **do not apply** in this direction, which
is why this can land ahead of (and independently of) the request passthrough:

- **No state coherence risk.** #130's central worry is writes routing around
  Axon's store and diverging from tracked state. Ephemeral events mutate
  **nothing** in Axon's store — they are transient overlays with no persisted
  representation. The entire divergence class evaporates.
- **No E2EE risk.** Typing, receipts, and presence are never encrypted, so #130's
  crypto-bypass concern (encrypted sends must not skip the crypto layer) has no
  analogue here. There is nothing to denylist for safety.
- **Read-only blast radius.** A client receiving these frames learns nothing it
  couldn't learn by reading the room; it cannot *act* through this channel. The
  security surface is observation, not mutation.

### Allowlist, not denylist, of forwarded event types

Forwarded types are an explicit **allowlist** (initially `m.typing`,
`m.receipt`), config-extensible. Rationale:

- **Presence is opt-in.** `m.presence` is federation-wide and high-volume; a
  busy account can see presence churn for users across many servers. Defaulting
  it on would flood the single broadcast channel. It stays off the default
  allowlist until the lag question (below) is settled.
- An allowlist fails closed: a future EDU type Axon hasn't reasoned about isn't
  forwarded by accident.

### Lag domain and coalescing

All frames share one `broadcast` channel and therefore one lag signal: a slow
consumer that lags gets a `Lagged` skip across *every* frame kind. High-frequency
ephemerals (presence especially, typing to a lesser degree) could induce timeline
consumers to lag and miss real events. Mitigations, in order of preference:

- **Coalesce typing.** `m.typing` is already whole-list-replace (the full set of
  typers per room), so only the latest per room matters — older typing frames for
  the same room can be dropped before send.
- **Reserve a separate channel / lag domain for ephemerals** if coalescing proves
  insufficient, so an ephemeral flood cannot cost a client a timeline event.
  Deferred until measured; the single channel ships first.

### No replay — ephemerals are miss-tolerant

Unlike timeline events (re-readable via the read API) and verification (re-readable
via `GET …/verify/{flow_id}` so a reconnecting client recovers a missed frame,
ADR 0027), a reconnecting client simply **misses** in-flight ephemerals and waits
for the next one. They are self-expiring (typing times out server-side ~30s;
presence and receipts are superseded by the next update), so there is no replay
contract and no re-read endpoint. This is a deliberate simplification: the client
holds the live overlay in memory and lets it expire, the same way it must already
expire a typing indicator that stops arriving rather than being explicitly
cancelled.

## Consequences

- **`axon-tui` unblocks two features at once.** Read receipts ("seen by", unread
  markers) and typing indicators become possible with no further backend frame
  work — the first concrete consumers, and the recommended lead implementation.
- **The long tail is free.** Any future ephemeral on the allowlist reaches clients
  without a four-crate change; client feature work decouples from Axon's frame
  roadmap, exactly as #130 decouples it from the route roadmap.
- **Two silos.** Backend (core enum + sync producer + api wire tag) and client
  (TUI consumer) are separate PRs per the one-silo-per-PR rule; backend lands
  first with a wire-contract test, the TUI consumer second.
- **Presence is explicitly deferred** behind the lag-domain decision; the frame
  shape already accommodates its wire shape (`room_id: None`). **Implementation
  correction:** enabling it later is *not* purely a config change as originally
  assumed here — presence is account-scoped and matrix-sdk dispatches it via a
  structurally different handler kind than the room-scoped ephemeral events
  this ADR's implementation registers for, so forwarding presence needs a
  second handler registration (real code) in addition to the config edit and
  the lag-domain decision. The backend PR logs a boot warning if `m.presence`
  is added to the allowlist anyway, so the gap fails loudly rather than
  silently.
- **Pairs with #130.** Adopting both yields a symmetric escape hatch — arbitrary
  requests out, arbitrary live events in — with the typed routes/frames as the
  value-added layer over each. This ADR can be accepted and shipped without #130;
  the cross-reference is architectural, not a dependency.
