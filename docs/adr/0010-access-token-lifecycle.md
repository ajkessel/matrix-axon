# ADR 0010 — Access-token lifecycle: long-lived tokens, re-auth on revocation

## Context

Axon restores each account's session from a stored access token (ADR 0007, 0008).
That raises the lifecycle question: do these tokens expire, and how do we recover
when one stops working?

Matrix access tokens on Synapse and Dendrite are **effectively permanent** by
default — they do not expire on a schedule. A token becomes invalid only if the
session is logged out elsewhere, an admin purges it, or the homeserver is
configured with a session lifetime. Matrix also defines short-lived tokens with a
refresh mechanism (MSC2918), but support is not universal.

## Decision

**Treat the access token as long-lived for M3.** We store it once and reuse it
across restarts without proactive refresh.

**Recover from revocation by re-authenticating.** When the SDK reports
`M_UNKNOWN_TOKEN` (HTTP 401) — the token was invalidated — the correct recovery is
to re-login from the provisioned credential (if one is configured), update the
`accounts` row with the new token + device ID, and resume sync. This is handled
within the per-account supervised restart loop (ADR 0007), so a revoked token
manifests as a task failure that the supervisor retries, not a process crash.

**Defer MSC2918 refresh-token support to M4+.** We do not request refresh tokens
or implement proactive refresh yet. `SessionTokens.refresh_token` is stored as
`None`. This is a documented known gap, not an accident.

## Consequences

**Pros**
- Simple: no refresh scheduler, no token-expiry bookkeeping in M3.
- Resilient to the common revocation cases via the existing restart loop.

**Cons / risks**
- An account provisioned with a **pre-supplied `access_token`** (no password) that
  later gets revoked **cannot self-recover** — there is no credential to re-login
  with. The supervisor will back off and **retry indefinitely** (the 60s backoff
  cap is the *interval between retries*, not a total timeout — there is no maximum
  retry count); the operator must supply a fresh token. This is acceptable for M3
  but must be surfaced clearly.
- Homeservers configured with short session lifetimes will see periodic forced
  re-auth; without MSC2918 we cannot refresh seamlessly.

**When to revisit**
- M4+: implement MSC2918 — request refresh tokens at login, store and rotate them,
  refresh proactively before expiry. This also changes ADR 0008's stored shape.
- Add explicit detection/alerting for the "token-only account, revoked, no
  credential" dead-end rather than silent infinite retry.

## Alternatives considered

- **Implement MSC2918 now.** Rejected for M3: not universally supported, and the
  long-lived-token assumption covers the default Synapse/Dendrite case. Scope it
  to M4 once core sync + archive are proven.
- **Crash the process on `M_UNKNOWN_TOKEN`.** Rejected: one account's revoked
  token should not take down a multi-account process; the restart loop isolates it.
