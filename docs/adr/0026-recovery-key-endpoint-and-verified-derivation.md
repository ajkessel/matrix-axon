# ADR 0026 — Recovery-key key acquisition as a runtime verb, and deriving the `verified` flag

## Context

ADR 0011 settled axon's two E2EE key-acquisition paths: the **recovery-key
bootstrap** (import the megolm key backup + cross-signing keys from a Secure
Storage / 4S recovery key) and the **interactive SAS** path. The recovery-key
path shipped in M3c as a *boot-time, config-driven* call: if
`sync.account.recovery_key` is set, the supervisor calls
`client.encryption().recovery().recover(key)` once during sync startup
(`engine::recovery_key_for`), transient-only and never persisted.

Two gaps remained, and this PR (7a-5) closes them:

1. **No runtime way to recover.** With accounts now added at runtime via
   `POST /v1/accounts/login` (ADR 0022), there was no API to hand a recovery key
   to an account that wasn't provisioned from config. Config-based provisioning is
   also slated for retirement (ADR 0024 *Consequences*), and the recovery-key
   string it carries needs a runtime home before that can happen.

2. **`verified` was stubbed.** ADR 0022 added an `accounts.verified` column —
   whether axon's *own* device is cross-signed, orthogonal to lifecycle `state` —
   but left its derivation "wired up in a later subphase," so it was always
   `false` and the read API surfaced `null`. The spec is emphatic that a stale
   `verified` is worse than none: it must be **re-derived from the SDK's current
   cross-signing state**, not written once. Recovery is exactly the operation that
   *makes* axon's device verified (self-cross-signing with no interactive partner),
   so it is the natural place to make the flag real.

## Decision

### `recover` is the boot-time `recover()` promoted to an on-demand verb

`POST /v1/accounts/{account_id}/recover` takes a 4S `recovery_key` and runs the
**same single SDK call** the config path runs, then reuses the existing
re-decryption sweep:

1. resolve the account, take the **per-identity lock** (the same lock login /
   logout / delete serialize on), and re-read the row under it;
2. require **`active`** — recover needs a live, authenticated client, obtained via
   `ClientManager::get_or_connect` (whose cold-connect gate is already
   active-only). A `deactivated` row is a `409` (`LifecycleError::NotActive` —
   "log in first"), `deleting` a `409` (`BeingDeleted`), an unknown id a `404`;
3. `client.encryption().recovery().recover(recovery_key)` — imports the megolm
   backup decryption key + the cross-signing private keys into the account's
   crypto store;
4. derive + persist `verified` (below) so the row the caller reads back reflects
   the new state;
5. `redecrypt::sweep_pending_utds` — back-fill the stored UTDs the imported keys now
   unlock (keys already in the crypto store don't fire the arrival stream, so an
   explicit sweep is the only thing that retries them).

The `recovery_key` is **consumed once and never persisted** — identical to the
M3c boot path and consistent with the access-token-vs-recovery-key blast-radius
reasoning in ADR 0011. Like the other secret-bearing lifecycle verbs, the route is
**loopback-bound until 7b**.

**Error classification.** The SDK's `RecoveryError` is *not* blanket-mapped to
`400`. Only its `SecretStorage` variant is client-actionable — a wrong/rotated
key, or an account that never set up Secure Backup, fails opening the secret store
— so it becomes a readable **`400`** (`LifecycleError::RecoveryFailed` →
`RecoverError::BadRequest`) carrying a *stable* message (the SDK's own text can
expose secret-storage internals, so it is replaced), never a silent permanent UTD.
Every other variant (an `Sdk`/upstream failure, an unexpected backup-state error)
is **not** the caller's to fix, so it surfaces as a generic **`500`** with the
detail logged server-side rather than mis-blamed on the request as a `400`. (A
`502` would arguably fit a transient homeserver failure, but the SDK's opaque
`Sdk` variant doesn't cleanly separate transient-upstream from internal, so we
stay conservative with `500`.)

**Bounded backfill (step 5).** The sweep runs under the per-identity lock, so a
large UTD backlog or a stalled homeserver could otherwise pin that lock and starve
a concurrent logout/delete. It is therefore capped by `RECOVER_SWEEP_TIMEOUT`
(30s); a timeout is logged, leaves the keys and `verified` already persisted (the
success the caller awaits), and defers the unswept rows to the next supervised
boot sweep.

### `verified` is derived from `get_own_device`, refreshed by a watcher task

**Why not the `verification_state()` subscriber for the derivation.** The SDK
exposes `Encryption::verification_state()`, a subscriber over
`VerificationState { Unknown, Verified, Unverified }` — "the verification state
of our own device." But that value is only updated on a `/keys/query` response
that contains our own device (`update_state_after_keys_query`), so reading it
immediately after `recover()` lags a homeserver round-trip. We therefore derive
`verified` **directly** from `get_own_device().is_cross_signed_by_owner()` — the
exact check the SDK runs internally in `update_verification_state` — which is
accurate the instant the cross-signing keys land. A missing own-device or a
crypto-store error is treated as **not** verified (a stale `true` is worse than a
transient `false`).

**Not write-once.** A per-account **verification watcher** child task in
`engine::run_account` keeps the column tracking reality. It mirrors the
re-decryption queue's lifecycle exactly — spawned on a `cancel.child_token()`,
joined/drained before the run returns, so it never leaks across a supervised
restart — and uses the `verification_state()` subscriber purely as a *signal*:
its `subscribe_reset` semantics make the first poll yield the current value
(persisting the initial state), and each later change (login, recover/verify,
cross-signing rotated, trust reset) re-derives via `get_own_device` and
re-persists. `recover` additionally persists synchronously so its own read-back
is correct without waiting for the watcher.

**Serialized writes (no lost update).** Both `recover` and the watcher write
`verified`, and a naïve "derive, then persist" interleaves badly: the watcher
could read pre-import state (`false`), `recover` then imports keys and writes
`true`, and the watcher finally writes its stale `false` — a lost update with no
guaranteed later emission to heal it. So the watcher derives **and** persists
under the **same per-identity lock** the verbs hold (the lock map is owned by
`SyncEngine` and shared with every `AccountLifecycle` and every watcher). Holding
the lock across the derive makes it observe post-`recover` state, so the last
write always reflects current truth.

The watcher's lock wait is **cancellation-aware**, and this is load-bearing for
shutdown, not a nicety. A lifecycle verb holds this same lock while it cancels and
*awaits* the supervised task, which in turn drains (awaits) the watcher. If the
watcher were parked on an un-cancellable `lock.lock()` at that moment it would hold
nothing yet block forever on the lock the verb holds, while the verb blocks on the
watcher — a deadlock broken only by the drain timeout aborting the supervisor, after
which the *separately-spawned* watcher could acquire the freed lock and persist the
dead device's derived value over the verb's reset `false` (the very stale `true` the
reset exists to prevent). So the watcher races the lock acquisition against its
cancellation token and, on cancel, abandons the write entirely.

**Deactivated accounts.** `logout` resets `verified` to `false` as it deactivates
(under the same lock, after the watcher task is reaped): a logged-out device is
dead — its token was invalidated upstream — and reactivation logs in a *fresh,
unverified* device (ADR 0022). So a `deactivated` account reports `false`, and a
re-login's read-back can't return a stale `true` from the previous device. (Were
it not reset, the column would hold the dead device's last value with no watcher
running to correct it.)

**Wire shape.** `AccountDto.verified` stays `Option<bool>` for forward
compatibility (a future genuinely-unknown state, or a nullable column) but is now
always populated from the persisted bool, replacing the always-`null` stub. A
successful `recover` (`200`) guarantees the **keys were imported**, not that
`verified` is `true`: the flag is a derived observation of cross-signing state
(normally `true` after recover, but a partial Secure-Backup could import keys yet
leave the device unverified), so clients should read it rather than assume it.

### What this does *not* do

This is the recovery-key (bootstrap) path only. **Interactive SAS verification**
— the mature, no-recovery-key-on-server path — remains the *last* 7a PR, and
`axon-crypto` stays a stub until then (ADR 0011). Recover imports *keys*, not
*history*: pulling a room's pre-install timeline is M10 backfill, which consumes
exactly these keys (ADR 0018).

## Consequences

**Pros**
- The recovery-key path has a runtime home, unblocking the retirement of
  config-based provisioning (ADR 0024) and giving headless/CI deployments an
  on-demand key-acquisition verb.
- `verified` reports reality: a client can prompt verify-or-recover while the
  device is unverified and see the flag flip after a successful recover, without a
  restart.
- Maximal reuse — recover is the existing SDK call + existing sweep under the
  existing lock; the watcher is a copy of the re-decryption task's shape. No new
  migration (the column already exists), no new crypto surface.

**Cons / risks**
- The watcher adds one more child task per supervised account. It is best-effort
  (a persist failure is logged and skipped) and bounded by the same drain logic as
  the re-decryption queue, so it can't wedge a run.
- `recover` requires an `active` account, so a client must `login` before
  recovering a logged-out one — a deliberate `409`, since there is no live client
  to recover against otherwise.
- Reading `get_own_device` for the synchronous derivation and the
  `verification_state()` subscriber for the watcher signal are two different SDK
  surfaces for one concept; they converge (both ultimately reflect
  `is_cross_signed_by_owner`), but the split is worth remembering.

**When to revisit**
- The interactive-SAS PR will also flip `verified` (after mutual confirm,
  gossiped secrets cross-sign the device); it should reuse `derive_verified` and
  the watcher rather than introducing a second derivation.
- If a genuinely-unknown verification state ever needs distinguishing from
  `false` on the wire, make the column nullable and let `AccountDto.verified`'s
  existing `Option` carry it.
