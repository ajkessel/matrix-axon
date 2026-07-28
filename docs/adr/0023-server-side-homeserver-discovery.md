# ADR 0023 — Server-side homeserver discovery for login

## Context

Logging in as `@adam:bostoncoop.net` requires knowing where that user's
homeserver actually lives. The Matrix client-server spec ("Server Discovery",
[well-known URIs](https://spec.matrix.org/v1.18/client-server-api/#well-known-uris),
an [RFC 8615](https://datatracker.ietf.org/doc/html/rfc8615) well-known URI)
resolves this from the user ID's *server name*: fetch
`https://<server name>/.well-known/matrix/client` and use
`m.homeserver.base_url`, falling back to the server name itself when no document
is published. The server name and the homeserver are routinely different hosts —
`bostoncoop.net` publishes `https://matrix.bostoncoop.net` — and a domain may
delegate anywhere (`@adam:bostoncoop.net` could be hosted at an unrelated
domain). It is a DNS-like indirection, not a homeserver call.

`POST /v1/accounts/login` (M7a PR 2) required the caller to supply
`homeserver_url`, so the first TUI login implementation did this discovery
itself, client-side. Review flagged the tension: the architecture says clients
talk only to axon, and client-side discovery means (a) every client reimplements
the same resolution, (b) client egress includes arbitrary user-named hosts, and
(c) — decisive at the time — the account natural key was
`(user_id, homeserver_url)` everywhere (identity lookup, per-identity verb
locks, upsert), so two clients that resolved the same user differently (one via
well-known, one via the fallback) would mint **duplicate accounts** for one
identity.

Amendment (2026-06-14): runtime lifecycle lookup and locking now use `user_id`
as the identity and treat the URL as its connection endpoint. This closes the
remaining config-vs-discovery alias case while retaining the existing database
constraint for compatibility with stores that already contain duplicate rows.

## Decision

Discovery moves into axon; clients send credentials only.

- **`homeserver_url` becomes optional** on `POST /v1/accounts/login`. When
  omitted, the lifecycle backend (`axon-sync`'s `discovery` module) resolves the
  canonical base URL from the MXID's server name before anything else — ahead of
  the per-identity lock, which is keyed by the resolved URL. When supplied, it is
  the escape hatch for homeservers without (or with broken) well-known, and the
  only way to reach a plain-HTTP local dev homeserver (`http://localhost:8008`).
- **Validation, by path:** a *discovered* candidate is accepted only if it uses
  HTTPS (plain HTTP is allowed only when the MXID server name itself is loopback,
  so a public well-known cannot redirect credentials into axon's own loopback)
  *and* answers `GET /_matrix/client/versions` with a non-empty version list —
  discovery is *guessing* (well-known vs. fallback), so the probe confirms which
  guess is a real homeserver. An *explicit* URL is normalized and scheme-checked
  but **not probed**: a trailing slash is trimmed (so it keys identically to a
  discovered URL — the `(user_id, homeserver_url)` natural key would otherwise
  mint a second row for an identity discovery already keys without the slash) and
  the same HTTPS-unless-loopback rule is enforced (without it the escape hatch
  could ship a password in cleartext to a plain-HTTP public host). It is not
  probed because it is caller-asserted — there is no guess to confirm — and a
  genuinely-bad URL surfaces at the SDK login call. The trim and scheme-check are
  pure string work, so an idempotent no-op login (already-active account) makes
  no upstream request on the explicit path.
- **Spec mapping, deliberately asymmetric:** an unreachable well-known, a
  non-2xx, or a 2xx whose body isn't JSON is treated like the spec's 404
  `IGNORE` — fall back to `https://<server name>` (same origin, so a dead host
  still fails, just at the versions probe). The non-JSON case matters in
  practice: catch-all servers (Caddy, SPA fallbacks, default vhosts) answer 200
  with an empty or HTML body for every path — `matrix.bostoncoop.net` does
  exactly this — and that is "no well-known published", not a malformed one.
  But a 2xx **JSON** document that lacks a usable `m.homeserver.base_url`, or
  names an invalid homeserver, is an **error** (the spec's
  `FAIL_PROMPT`/`FAIL_ERROR`), not a fallback: someone published a matrix
  well-known and got it wrong, and silently guessing would log the user into a
  homeserver their domain didn't name. All discovery failures surface as `502`
  (upstream), leaving no account row.
- **Egress discipline:** discovery HTTP requests carry a 10s timeout so a
  stalled user-named host can't hold a login open. This is axon's first
  user-controlled egress outside matrix-sdk itself; it is confined to the
  `discovery` module. The cap is *per request*, not per login, so the serial
  probes (well-known → versions → `key/v2/server`) could in the worst case spend
  ~30s before the password is checked. Acceptable: login is async with a
  busy-gate in the TUI (M7a PR 2/3), so it doesn't freeze a client; a tighter
  bound would be a single per-login deadline, deferred until there's a reason.
- **Clients do not discover.** The TUI's client-side discovery is removed in the
  same change-set; `/login @user:domain password` sends username + password and
  lets axon resolve.
- **The MXID's domain is checked too — wrong spellings are rejected with a
  suggestion, not silently corrected.** Users routinely type the homeserver
  host where their Matrix ID's domain is the server name
  (`@adam:matrix.example.org` for `@adam:example.org`) — without help that
  fails as a misleading `401` ("authentication failed", though the password is
  right), and naively accepting both spellings would mint duplicate accounts:
  the user-ID half of the `(user_id, homeserver_url)` natural key has the same
  hole the URL half had. After resolving the base URL (on both the discovered
  and the explicitly-supplied path), axon fetches the homeserver's own declared
  server name (`GET /_matrix/key/v2/server` — unauthenticated; technically
  federation surface, but Synapse and Dendrite serve it on the client listener)
  and, when it differs from the typed domain, **rejects the login with a `400`**
  whose message names the spelling the user almost certainly meant ("did you
  mean `@adam:example.org`?"). An earlier revision silently rewrote the domain
  and logged the user in; rejection was chosen instead because a login should
  never succeed as an identity other than the one the caller typed. The
  duplicate-account hole stays closed either way — the wrong spelling can never
  mint a row. The rejection is safe by construction: it fires only when the
  resolved homeserver declares its users live under a *different* domain — in
  which case the typed MXID cannot exist on it, so nothing legitimate is
  refused. **Best-effort:** a missing or unusable key endpoint passes the user
  ID through as typed (the homeserver's own auth error then speaks). This is a
  deliberate decision, not an oversight: it makes the *mistyped* MXID
  (`@adam:matrix.example.org`) non-deterministic across a flaky key endpoint —
  rejected when the probe succeeds, passed through (and so keyed under the typed
  domain) when it times out. We accept that over hard-failing, because a flaky
  federation endpoint must not block an otherwise-valid login, and the blast
  radius is bounded to the typo: a correctly-typed `@adam:example.org` is never
  rewritten, so it keys stably regardless of the probe's outcome.

## Alternatives considered

1. **Keep discovery client-side** (as first implemented): lightweight per
   client, but leaves the duplicate-account hole open and spreads user-named
   egress across every client.
2. **Client-side now, server-side later** (TODO): same holes, plus a migration
   for clients later.
3. **Server-side with client fallback:** redundant code path that reintroduces
   the duplicate-account hole exactly when the two paths disagree.
4. **A separate discovery endpoint** (`GET /v1/homeserver-discovery?user_id=`):
   keeps login's contract unchanged, but makes every client do two calls and
   re-send the resolved URL, which is precisely the value a client shouldn't
   need to handle.

## Consequences

- One canonical `homeserver_url` keys each identity regardless of which client
  logs it in; clients need only username + password.
- Axon performs server-side requests to user-named hosts at login time
  (loopback included, deliberately, for local dev). Acceptable for a
  self-hosted, loopback-guarded endpoint; worth revisiting if login is ever
  exposed beyond the auth layer planned in M7b.
- A future generic homeserver **passthrough endpoint** (under discussion, not
  yet specified) would not subsume this: well-known resolution targets the
  *server name's* domain, not the homeserver — it is how axon finds the
  homeserver in the first place.
