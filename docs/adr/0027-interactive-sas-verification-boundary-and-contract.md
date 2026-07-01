# ADR 0027 — Interactive SAS verification: crate boundary and operations contract

## Context

ADR 0011 settled axon's two E2EE key-acquisition paths. The recovery-key
bootstrap landed as a runtime verb in 7a-5 (ADR 0026); this ADR records the
decisions behind the **interactive SAS** path (7a-6): a trusted device the user
already controls verifies axon's fresh device over a Short Authentication String
(emoji / decimal), after which axon is cross-signed and the user's other devices
gossip the cross-signing + key-backup secrets — so the recovery key never has to
live server-side.

Three decisions in that PR are load-bearing and were not adequately captured by a
single note in `AGENTS.md`, so they are recorded here (raised in the PR 6 review):

1. **Which crate owns the SDK-facing verification surface.** The project layout
   (`AGENTS.md`, `docs/mvp/implementation.md`, `docs/mvp/tech-spec.md`) describes
   `axon-crypto` as "the thin verification surface over rust-sdk crypto." The
   implementation instead lives in `axon-sync`, leaving `axon-crypto` a stub. That
   is a deliberate boundary change and needs a durable rationale, not just a
   directory comment.

2. **The shape of the operations contract.** `docs/mvp/implementation.md` (the 7a
   spec) lists the verification operations as "`accept` / `confirm` / `cancel` —
   as HTTP … **and as equivalent `/v1/ws` commands**." The PR ships an HTTP-only
   contract, with no client→server WS commands and no explicit `accept`
   operation. The narrower contract needs explicit approval.

3. **The delivery guarantees of the live surface before auth (7b).** Verification
   progress rides the existing best-effort `/v1/ws` broadcast bus, and the read +
   live surfaces are open (not loopback-bound) until the 7b bearer gate lands.
   Both carry trust-relevant state, so the threat posture must be stated, not
   assumed.

## Decision

### 1. The verification engine lives in `axon-sync`; `axon-crypto` stays a stub

The SDK-facing verification surface — driving the matrix-rust-sdk SAS state
machine, the in-memory flow registry, the per-account incoming-request listener —
lives in `axon-sync` (`crates/axon-sync/src/verification.rs`,
`VerificationEngine`). `axon-crypto` remains a reserved stub.

**Why not `axon-crypto`.** A "thin verification surface" was the original mental
model from before the sync engine grew its supervision machinery. In practice the
verification engine cannot be thin or standalone: it needs

- `ClientManager` — the single owner of per-account `Client`s (build / auth /
  cache / evict), with the active-state connect gate (ADR 0022);
- `IdentityLocks` — the per-identity lifecycle lock (ADR 0026) that serializes
  `start` against login / logout / delete and closes the login activation window;
- the supervised-task lifecycle — drivers must be bound to the **current sync
  run** so they never outlive an evicted client (the supervisor evicts + rebuilds
  on every sync-run failure); and
- it interleaves with `watch_verification`, the watcher that owns the `verified`
  column (ADR 0026).

All of that already lives in `axon-sync`. Re-homing it into `axon-crypto` would
either duplicate the connection/lock/supervision machinery or make `axon-crypto`
depend on `axon-sync` — inverting the intended layering. The cost of leaving it in
`axon-sync` is that `axon-crypto` stays empty; the alternative costs a worse
dependency graph for no isolation benefit (the crypto is the SDK's, not ours).

**The boundary that replaces the "axon-crypto surface."** Verification follows the
same consumer-owned-port / composition-root-adapter pattern as the message
gateway (ADR 0021) and the lifecycle port (ADR 0022): `axon-api` defines the
`VerificationService` port (trait + HTTP-shaped `VerifyError` + `FlowSummary`) and
stays free of `matrix-sdk`; `axon-sync` exposes the concrete `VerificationEngine`
implementing no foreign trait; `axon-server` owns the adapter newtype that maps
`axon_sync::VerifyError → axon_api::VerifyError`. `axon-api` and `axon-sync` never
depend on each other. The port *is* the boundary the directory comment used to
ascribe to `axon-crypto`.

### 2. The operations contract is HTTP-only; `accept` is automatic

The verification operations are exposed **as HTTP verbs only**:

- `POST …/verify {device_id}` — start a flow, returns `flow_id` (loopback-bound).
- `POST …/verify/{flow_id}/confirm` — confirm the SAS matches (loopback-bound).
- `POST …/verify/{flow_id}/cancel` — cancel the flow (loopback-bound).
- `GET …/verify` and `GET …/verify/{flow_id}` — read replayable per-flow state.

There are **no client→server `/v1/ws` commands**; the socket stays server→client
send-only, carrying `verification.{requested,sas,done,cancelled}` frames. This
supersedes the "and as equivalent `/v1/ws` commands" wording in the 7a spec.

There is also **no `accept` operation**. SAS has a protocol-level *accept* step,
but it requires no human decision (only `confirm`, the emoji comparison, does), so
the driver performs it automatically: a peer-initiated request is accepted as soon
as it is observed (SAS-only methods — see below), and a flow we started begins SAS
as soon as the peer is ready. Exposing `accept` to the client would add a verb
with no decision behind it. This supersedes the spec's listing of `accept` as a
client operation.

**Why HTTP-only.** The WS bus is fan-out infrastructure (ADR 0020) with no
inbound command framing, sequence numbers, or per-client addressing. Adding a
reliable bidirectional command channel is real protocol design that belongs with
the 7b auth work (the client identity a command is attributed to is exactly what
7b establishes), not bolted onto a pre-auth fan-out socket. The HTTP verbs already
cover every operation a client needs; a WS command surface would be a second way
to do the same thing. Deferred, not dropped.

**SAS-only method advertisement.** Every request axon starts
(`request_verification_with_methods`) and every incoming request it accepts
(`accept_with_methods`) advertises **`SasV1` only**. matrix-rust-sdk's default
method set has the `qrcode` feature on, so the SDK default would let a peer pick
QR — which this driver does not implement and would immediately cancel. QR is
explicitly deferred (ADR 0011).

**Self-verification only.** *(Superseded by ADR 0040 — cross-user verification.)*
The incoming-request listener rejects (cancels) any
request that is not `is_self_verification()`. M7a verifies axon's *own* device
against another of the user's trusted devices; actively verifying another user's
identity is out of scope. This also keeps the `(account_id, flow_id)` registry key
unambiguous — only the user's own devices initiate, so a transaction id cannot
collide across senders.

### 3. Pre-7b threat posture of the live + read surfaces (accepted)

Until the 7b bearer gate lands, the secret-/trust-bearing verbs (`verify`,
`confirm`, `cancel`) are loopback-bound (`127.0.0.1`), like the other lifecycle
verbs. The **read** routes (`GET …/verify`, `GET …/verify/{flow_id}`) and
`/v1/ws` are **open**, consistent with the rest of the pre-auth read API.

Two consequences are **accepted for the pre-7b window**, not fixed here:

- **Pre-auth disclosure.** An open read/live surface lets any peer that can reach
  axon observe an active verification exchange (the target device id and, once
  available, the SAS emoji/decimals). This is the same posture as every other
  pre-7b read route, and the SAS values are not themselves a credential (knowing
  the emoji does not let an attacker complete a verification — confirmation
  happens on the loopback-bound verbs and over the encrypted Matrix to-device
  channel). The M7a-without-7b state is already documented as *not remotely
  deployable* (`implementation.md`, lifecycle endpoints); this disclosure is
  covered by that same constraint. 7b closes it by gating the whole `/v1/` surface.

- **Lossy delivery of trust-bearing frames.** Verification frames share the
  best-effort broadcast bus with timeline traffic (ADR 0020); on
  `RecvError::Lagged` a client silently skips frames, with no gap/sequence signal.
  The mitigation is the **read-on-reconnect contract**, which the design already
  requires: a client must **never** assume the frames it received are complete —
  after any reconnect (and as the authoritative source at any time) it re-reads
  `GET …/verify/{flow_id}`, which re-derives the current stage and SAS values from
  the live SDK object, and a terminal outcome (including a missed `cancelled`) is
  retained for a 5-minute grace TTL. A first-class gap signal or a reliable
  verification channel is deferred to the 7b/WS-command work in (2); the read API
  is the durable source of truth in the interim.

## Consequences

- `axon-crypto` stays an empty reserved crate. The earlier "thin verification
  surface over rust-sdk crypto" directory comments are updated to point here; if a
  genuinely SDK-isolated crypto surface is ever wanted, this ADR is the thing to
  supersede.
- The 7a spec's verification "operations" wording (WS commands; `accept`) is
  superseded by decision (2); `docs/mvp/implementation.md` is annotated to point
  here rather than rewritten, so the original intent stays legible.
- Clients must implement the read-on-reconnect contract; they cannot treat the WS
  stream as a reliable verification transcript. This is the same lesson as ADR
  0020 (sync is never back-pressured by a client), applied to a trust-bearing
  frame.
- A bidirectional WS command surface and a verification-delivery gap/replay
  contract are open follow-ups, naturally sequenced with 7b (client auth).
