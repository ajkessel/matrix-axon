# ADR 0029 — Client↔axon bearer-token auth

## Context

Until now the `/v1/` API had no application-level auth. The read and mutation
routes were open; the secret-bearing / destructive lifecycle verbs (login,
logout, recover, verify/confirm/cancel, delete) were merely **loopback-restricted**
by a per-route guard (`require_loopback`). That made the M7a-without-7b state
explicitly *not remotely deployable* — there was no token a remote client could
present, so the only safe posture was localhost-only.

M7b (the work formerly numbered M8; `docs/mvp/implementation.md` §7b) closes
this: a bearer-token gate in front of the whole `/v1/` surface — HTTP **and** the
WebSocket, including the lifecycle verbs. Tokens are minted out-of-band by an
`axon token` CLI (the bootstrap path), stored hashed, and carried by clients
thereafter. Once it lands, the loopback restriction lifts and the gate applies
uniformly. The spec adds a forward-compat constraint: a future OAuth 2.0 + PKCE
issuer must be able to replace the CLI mint path **without** changing the
on-the-wire `Authorization` header or any consumer code.

## Decision

**Tokens are global to the instance, not account-scoped.** The `tokens` table is
`(id, label, hash, created_at, last_used_at, revoked_at)` — deliberately no
`account_id`. One human owns all their Matrix accounts; per-account
authorization is an explicit non-goal. A token authenticates a *client* to axon,
distinct from the per-account Matrix access token (which authenticates axon to a
homeserver).

**Hash, don't encrypt.** A bearer token is a high-entropy random secret, so a
single SHA-256 (base64) is the right primitive — the GitHub-PAT model — not the
recoverable `pgp_sym_encrypt` used for the access token (ADR 0008), and not a
password KDF like argon2 (those defend *low*-entropy secrets and are far too slow
to run on every request). There is nothing to brute-force in a 256-bit random
token, and verification must be cheap because it is on the hot path of every
request. The raw token (`axon_<base64url(32 bytes)>`) is shown once at mint and
never recoverable; only its hash is stored, `UNIQUE` (which doubles as the lookup
index). Verification is one `UPDATE … WHERE hash = $1 AND revoked_at IS NULL
RETURNING id` — match and `last_used_at` touch in a single round-trip.
Revocation is a tombstone (`revoked_at`), not a delete, so the audit row
survives.

**Verification sits behind a `TokenVerifier` port.** The trait (`verify(token) ->
bool`) is the seam the spec asks for: the `tokens` table and the CLI mint path
are an implementation detail behind the shipped `StoreTokenVerifier`, so an OAuth
issuer can swap in without touching the middleware or any route. It is held in
`AppState` as `Arc<dyn TokenVerifier>`, mirroring the existing `MessageSender` /
`AccountLifecycle` / `VerificationService` ports.

**One layer over `/v1`, plus a separate WebSocket check.** The HTTP routes are
built into a sub-router carrying a single `require_bearer` layer
(`from_fn_with_state`), rather than a per-route attachment — so no route can be
added without the gate, and the per-route `require_loopback` guards (and the
`loopback` module) are removed. `/healthz` stays outside the gate (a monitor must
reach it without a token). The WebSocket can't ride the HTTP layer — a browser
can't set an `Authorization` header on a socket — so `ws_handler` authenticates
itself at upgrade time, accepting the token from the `Authorization` header
(non-browser clients like the TUI) **or** a `bearer.<token>` entry in
`Sec-WebSocket-Protocol` (browsers), and rejecting with `401` before upgrading.
The token-bearing subprotocol is **never echoed** in the `101` response — that
would put the secret in response headers, where proxies and access logs may
capture it — so axum selects no subprotocol and the handshake completes without
one. The scheme name in the `Authorization` header is matched case-insensitively
(RFC 7235); the token is verbatim.

**Revocation must reach live sockets.** Revocation happens out-of-process (the
`axon token revoke` CLI writes the DB; the running server gets no push signal), so
an established socket would otherwise keep streaming frames to a revoked client
forever. The fix is **bounded periodic revalidation**: each live socket re-checks
its token on an interval (default 30s; tests shorten it) and closes when it stops
verifying. This honors the tech spec's "bearer tokens … revocable individually."

**Plain HTTP is refused off-loopback.** Axon terminates no TLS and the `/v1/` API
carries credentials, while the tech spec requires client↔Axon TLS. So the server
**refuses to bind a non-loopback address** over plain HTTP unless
`server.allow_insecure_bind` is explicitly set — the safe deployment is loopback
behind a TLS-terminating reverse proxy (or a private mesh VPN). Removing the
loopback guard does not, on its own, make off-box cleartext access safe; this gate
keeps that explicit and auditable.

**The `401` is in the contract, with an RFC 6750 challenge.** The global
`security` requirement is paired with a reusable `401` response (the
`ErrorResponse` envelope) injected into every operation, so the source-of-truth
OpenAPI document describes both the requirement and its failure shape. The gate's
`401`s also carry a `WWW-Authenticate` challenge (RFC 6750 §3): a missing or
malformed credential gets the bare `Bearer`, and a present-but-rejected token
(unknown or revoked) gets `Bearer error="invalid_token"` (§3.1), so a
standards-aware client can tell "send a token" apart from "your token is bad."
The same challenges apply to the `/v1/ws` upgrade rejection (it is plain HTTP
until the `101`). The challenge is attached **at the gate**, not on every
`ApiError::unauthorized` — a `401` raised *inside* a handler (e.g. `login`, when
the **Matrix** homeserver rejects the supplied Matrix credentials) is a different
failure and must not advertise the client↔axon bearer scheme.

**CLI bootstrap.** `axon token issue --label <name>` mints and prints a token
once; `axon token list` shows id/label/created/last-used/status (never a secret);
`axon token revoke <id>` tombstones one. These need only the database, so the
binary gained a clap subcommand layer: no subcommand runs the server exactly as
before; `token` connects the store and skips the sync engine and HTTP listener.

## Consequences

- The loopback guard is gone; the bearer gate is its stronger, route-agnostic
  replacement, and the lifecycle verbs are now reachable from off-box **with** a
  valid token (and rejected without one). M13's VPN guidance becomes
  defense-in-depth over the whole authenticated surface, as the spec intends —
  not a stand-in for app auth.
- `last_used_at` is written on every authenticated request. That is one extra
  `UPDATE` per request by design (the spec wants last-used tracking); if it ever
  shows up on the hot path it can be throttled, but it is a single indexed write.
- The OpenAPI document now declares an HTTP-bearer security scheme with a global
  `security` requirement, so generated clients know every `/v1/` operation needs
  the header.
- `axon-tui` does not yet send the token; wiring the client (HTTP header + the WS
  subprotocol/Authorization) is separate client work, tracked outside this PR.
- Forward compatibility is real at the wire and the seam: OAuth replaces the
  `TokenVerifier` impl and the mint path; the `Authorization: Bearer` contract,
  the middleware, and every route stay unchanged.
- `store_key` rotation remains deferred (ADR 0008) and is unrelated — tokens are
  hashed, not encrypted, so they are not under the `store_key` at all.
