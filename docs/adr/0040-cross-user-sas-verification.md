# ADR 0040 — Cross-user SAS verification

## Status

Accepted. **Supersedes the "Self-verification only" decision of ADR 0027 §2**, and
absorbs the abandoned ADR 0035 (incoming room-based verification), whose
implementation was cut from PR 119 for blast radius.

## Context

ADR 0027 (§2) scoped interactive SAS verification to **self-verification only**: axon
verifies the user's *own* devices, and the incoming to-device listener cancels anything
that is not `is_self_verification()`. Verifying *another* user was explicitly out of
scope.

We now want **cross-user verification** in both directions:

- **Sender** — the user runs `/verify @bob:hs` and axon initiates a verification of
  another user's identity.
- **Receiver** — another user (e.g. from Element Web) initiates a verification of the
  axon user, and axon can complete it.

An implementation landed once on branch `tui-sas-verification` (commit `7e005f5`,
recorded by the now-superseded ADR 0035) and was abandoned from PR 119. The SAS *driver*
logic there was generic and correct; what made it unmergeable was its **transport**: it
blanket-accepted *every* pending room invitation and explicitly subscribed *all*
~300–400 DM rooms into the sliding-sync window. That produced a ~700 KB sync request body
that caused the homeserver to return **zero** timeline events to *every* room — a
whole-account regression. This ADR keeps the driver model and replaces the transport.

### Two load-bearing facts

1. **Cross-user verification is room-based.** Per the Matrix spec (MSC2241), verifying
   another user travels as an `m.room.message` event with
   `msgtype: m.key.verification.request` in a shared DM room — *not* as a to-device
   message. The existing to-device handler `on_incoming_request`
   (`crates/axon-sync/src/verification.rs`) and its `is_self_verification()` guard are
   therefore the wrong lever; a cross-user request never arrives there. Receiving
   cross-user verification requires a **room-event** handler.

2. **The trust outcome is already wired.** `crates/axon-sync/src/trust.rs` already reads
   `get_user_identity(sender).is_verified()` and renders it as the per-message
   `✓ / ⚠ / ?` sender-trust glyph (ADR 0031). When a cross-user SAS completes, the SDK
   signs the peer's master key with our user-signing key and `identity.is_verified()`
   flips to `true` on its own. **No trust-persistence work is required** — verifying a
   user updates the glyph for free.

## Decision

### 1. Add cross-user verification; keep the self-verification fast path

Cross-user is added *alongside* self-verification, not by relaxing the to-device guard.

- The to-device handler `on_incoming_request` keeps its `is_self_verification()` guard:
  self-verification stays a clean, room-free path.
- A new room-event handler `on_incoming_room_request` handles `m.room.message` /
  `m.key.verification.request`. For a cross-user sender (`ev.sender != our user_id`) the
  SDK stores the request normally and `get_verification_request()` returns `Some`; the
  handler accepts it through the existing `drive_request`. A self-verification-by-room
  fallback is also supported for clients (e.g. Element) that send self-verification as a
  room event — the SDK's `event_sent_from_us` guard drops those, so the handler responds
  with an outgoing to-device request to the sending device (the workaround documented in
  the superseded ADR 0035). The two handlers share one `VerificationListenerCtx`, with a
  `handled_room_events` set for room-event dedup.
- The sender path (`VerificationEngine::start`) is generalized to accept a target
  *user* as well as a target *device*. A user target initiates via the SDK user-identity
  API (`get_user_identity(user)` → `request_verification_with_methods(sas_only())`),
  which creates/uses a DM room and sends the room event. The SAS-only method
  advertisement (ADR 0027) is unchanged — QR stays out of scope.

### 2. Transport: scoped, on-demand room subscription

The blast radius that sank the prior attempt was subscribing the *whole* DM list. We
subscribe **only the single room backing an active verification**, and tear it down when
the flow ends.

- When axon initiates a cross-user flow, or accepts a fresh verification DM invite, the
  verification layer asks the sync engine to **join (if invited) and
  `subscribe_to_rooms` for exactly that one room** so its timeline events are delivered
  regardless of the room's rank in the selective sliding-sync window.
- When the flow reaches a terminal state (done / cancelled / expired), the engine
  **unsubscribes** that room (and may leave a DM it joined solely for verification).
- The set of explicitly-subscribed rooms is therefore bounded by the number of
  *concurrently active* verifications (normally 0–1), never the DM count. The rejected
  "subscribe all DMs" approach is not reintroduced.

For an *incoming* request we cannot read event content before joining the room. The
engine joins only invites that present as a verification DM (a small / freshly-created
direct room) and **auto-leaves on a short timeout** if no
`m.key.verification.request` event materializes, so an unsolicited invite cannot leave us
parked in an arbitrary room.

### 3. Flow model carries the peer user id

`FlowEntry` / `FlowState` (axon-sync), `FlowSummary` and the `start` port (axon-api), the
`VerificationFrame` bus type (axon-core) and its WS payload (axon-api), and the HTTP
`StartVerifyRequest` / `FlowDto` all gain a `target_user_id` / `user_id` field. The peer
user id already exists at the source (`ev.sender`, and the SDK request object) and was
simply discarded; threading it through lets a client show *who* it is verifying and lets
`POST .../verify` name a user target. The registry key stays `(account_id, flow_id)` —
the flow/transaction id is still unique per account.

### 4. Responder authorization: accept by default, client-gated

Incoming cross-user requests are accepted from **any** user by default. The user still
compares the emoji and presses confirm, so an unsolicited request can only *open a modal*
that the user can decline — it cannot complete a verification on its own. There is no
server-side allowlist.

Users who do not want unsolicited inbound requests suppress them **at the client**. The
TUI gains a config flag (`verification.accept_incoming_user_requests`, default `true`);
when `false`, an incoming *cross-user* `verification.requested` frame is not surfaced and
the client immediately cancels the flow via the existing `cancel` verb. Self-verification
(own-device) requests are never gated by this flag. The server keeps registering and
emitting the `requested` frame regardless — the client is the gate. We revisit a
server-side policy only if abuse surfaces.

## Consequences

- ADR 0027 §2's "self-verification only" is superseded; the rest of ADR 0027 (crate
  boundary, HTTP-only operations contract, SAS-only methods, read-on-reconnect, pre-7b
  threat posture) still holds. ADR 0035 is superseded by this ADR.
- Cross-user verification is delivered in phased, one-silo-per-PR changes: the wire model
  (axon-core + axon-api), the engine (axon-sync room transport + flow), and the TUI
  (`/verify @user`, user-aware modal, the inbound-acceptance config flag).
- Trust display needs no change — a completed cross-user verification flips the existing
  sender-trust glyph automatically (fact 2 above).
- The same pre-7b threat posture as ADR 0027 §3 applies: until the bearer gate lands the
  trust-bearing verbs are loopback-bound and the read/live surface is open.
