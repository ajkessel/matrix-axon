# ADR 0011 — E2EE key acquisition and device trust for a headless agent

## Status (updated M4a)

How the two paths actually landed, versus the M4/M5 split this ADR first
anticipated:

- **Recovery-key bootstrap — landed in M3c, not M4.** `recover()` is the
  re-decryption queue's driver, so it shipped with M3c (`engine.rs`,
  `recovery_key_for`). It is consumed once on boot and **never persisted**. The
  M4 review of whether to persist it encrypted at rest is closed in ADR 0015:
  **keep transient-only**.
- **Verification plumbing — moved wholly to M5, not M4.** This ADR floated
  building the SDK verification surface in M4 and wiring the UX in M5. But the
  plumbing cannot be exercised before the `/v1/ws` channel exists (M5), so a
  M4/M5 split bought nothing; the whole interactive path (surface + UX, in
  `axon-crypto`) is M5 work. M4 is the event-store schema (ADR 0015).

The body below is the original decision record. See ADR 0015 for the re-scope.

## Context

`axon` runs as a background device. When it logs in for the first time it appears
to the homeserver and to the user's other devices as a brand-new, **unverified**
device (ADR 0007 covers login/restore; this ADR covers what happens *after* login
for end-to-end-encrypted rooms).

A fresh device cannot read encrypted history and may not receive keys for new
messages, producing "Unable To Decrypt" (UTD) events. Two distinct things are
missing, and they are often conflated:

1. **Megolm room keys** — the per-session symmetric keys that decrypt message
   content. Historical keys predate this device; future keys are shared
   device-to-device by senders.
2. **Device trust (cross-signing)** — whether other devices consider `axon`
   *verified*. Senders that restrict key sharing to verified devices will withhold
   keys from an unverified `axon`, and the user sees "unverified session" prompts.

Matrix's normal answer to (2) is **interactive verification** (SAS — the
emoji/number comparison, or QR scanning). The naive reading is that a "headless"
backend cannot do this. That is wrong, and the distinction matters for the
decision below: *headless* means **`axon` has no UI of its own** — it does **not**
mean axon cannot participate in interactive verification. The matrix-rust-sdk's
verification machinery is fully programmatic (surface a `VerificationRequest`,
`accept()`, read `sas.emoji()` / `sas.decimals()`, `confirm()` / `cancel()`), so a
backend-for-frontend can **proxy** the flow to its own client.

## Decision

`axon` will acquire E2EE capability by **two complementary paths**, with
interactive verification as the primary mature path and recovery-key bootstrap as
the no-client fallback. **Both are deferred** out of subphase 3a; a fresh `axon`
device shows UTDs in the meantime (the expected condition the 3c re-decryption
queue and M4 crypto layer resolve).

### Primary (mature) path — BFF-proxied interactive verification (M4/M5+)

`axon` exposes the verification flow through its own API so the user verifies the
`axon` session from the **axon client**, exactly as they would verify any other
device:

- axon relays incoming/outgoing `VerificationRequest`s and **streams the SAS
  emoji/decimals over the existing WebSocket** (the `/v1/ws` channel introduced in
  M5);
- the user compares against an already-trusted client (e.g. Element) and confirms;
- axon calls `confirm()` to complete cross-signing the device.

**Security kicker — no recovery key on the server.** Once `axon` is interactively
verified and trusted, the user's other devices **automatically gossip** the
cross-signing secrets *and* the key-backup decryption key to it over encrypted
to-device messages ("secret sharing"). So `axon` ends up with full history *and*
future keys **without the recovery key ever touching the server**. This is the
canonical Matrix trust model and the right long-term fit for a BFF.

This path depends on the M5 client API + WebSocket layer (there must be a client to
verify *from*), so it cannot land before then.

### Bootstrap / fallback path — recovery key (M4)

Before any client exists (M4, and for headless/CI/no-client deployments), `axon`
bootstraps from the account's **recovery key** (Secure Storage / "4S"). The recovery
key is a single secret that unlocks *both* things axon needs:

- the **Megolm key-backup decryption key** → restore historical room keys; and
- the **cross-signing private keys** → `axon` signs its own device and becomes
  verified, so future keys flow automatically.

In matrix-rust-sdk this is essentially one call —
`client.encryption().recovery().recover(recovery_key)`. Supplied per account as
`sync.account.recovery_key` (optional), stored encrypted with the same pgcrypto
mechanism as the access token (ADR 0008).

## Consequences

**Pros**
- Interactive path matches the canonical Matrix trust model and keeps the
  crown-jewel recovery key **off the server** (secrets gossiped after trust).
- Recovery-key path needs no client, so decryption can be proven in M4 before the
  front-end exists, and covers headless/CI deployments.
- Both paths reuse the SDK; the interactive path reuses the M5 WS channel.

**Cons / risks**
- **Interactive path depends on M5** (client + WS) and on building a small
  verification state machine over the API — more moving parts than `recover()`.
- **Recovery-key path puts the crown-jewel secret at rest.** It decrypts *all*
  historical plaintext — a larger blast radius than the access token. Prefer
  holding it transiently (use to recover, then drop) over persisting it where
  feasible; review explicitly in M4.
- **Recovery-key path requires Secure Backup to be enabled** on the account; if it
  was never set up there is nothing to recover from.
- **Keys never backed up stay UTD** until they arrive via to-device sharing (the 3c
  re-decryption queue catches those).
- A wrong/rotated recovery key, or a cancelled/mismatched SAS exchange, must surface
  as a readable error, not a silent permanent UTD state.

**When to revisit**
- **M4:** implement `recover()` on connect; add `sync.account.recovery_key`
  (encrypted at rest; consider transient-only handling); verify the cross-signing
  self-signing path end-to-end against a real homeserver.
- **M4/M5:** design the verification API surface (request/accept, emoji stream over
  `/v1/ws`, confirm/cancel) and the SDK verification-event plumbing; QR-code
  verification as a follow-up.
- Revisit if we need accounts without Secure Backup, or `axon` to originate
  verification of *other* devices.

## Alternatives considered

- **Interactive SAS, but assume it's impossible for a headless device.** Rejected as
  a false premise: the SDK verification API is programmatic, so a BFF can proxy the
  flow to its own client. (The Element "start verification on the other device" flow
  hangs against `axon` *today* only because we have not built that plumbing yet, not
  because it is fundamentally unavailable.)
- **Recovery-key only, forever.** Restores capability but permanently keeps the
  crown-jewel secret server-side and skips proper interactive cross-signing. Kept as
  bootstrap/fallback, not the end state.
- **Run unverified and rely solely on key backup.** Restores history but leaves
  `axon` unverified, so strict senders withhold *new* keys and the user keeps seeing
  "unverified session" warnings.
- **Dehydrated devices (MSC3814).** A server-side device whose keys are restored on
  pickup. Potentially a clean fit for a headless agent, but more moving parts than
  the two paths above and not needed for the MVP. Noted as a future direction.
