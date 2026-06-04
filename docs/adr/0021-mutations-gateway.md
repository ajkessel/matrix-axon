# ADR 0021 — Mutations via a `MessageSender` port

## Context

M6 ("Mutations") adds the write half of the client API: `POST …/send`,
`PUT …/events/{id}` (edit), `DELETE …/events/{id}` (redact), and
`POST …/events/{id}/reactions` (react), each routed through matrix-rust-sdk's
send path on the appropriate account's `Client`.

This breaks an isolation the read side (5a/5b) carefully kept: `axon-api` depends
on neither `axon-sync` nor `matrix-sdk`. It consumes the wire-neutral
`axon_core::LiveEvent` and maps it to a DTO; it never touches the SDK. M6 needs
the API layer to *invoke SDK behavior* (send a message), and the per-account
`matrix_sdk::Client` lives only inside the sync engine's supervised tasks
(`run_account` → `connect_account`). So we need a seam that lets a handler send
without coupling `axon-api` to the SDK or to `axon-sync`.

We deferred 5c (interactive SAS verification) and did M6 first; 5c follows.

## Decision

### Consumer-owned port + composition-root adapter

`axon-api` defines the capability it *needs* as a trait it owns —
`MessageSender` (`send_message` / `edit` / `redact` / `react`, over plain types,
returning the new event id or a `SendError`). `AppState` holds it as
`Arc<dyn MessageSender>`; the mutation handlers call it and map `SendError` onto
the shared `{error}` envelope. `axon-api` gains no dependency on `axon-sync` or
`matrix-sdk`.

`axon-sync` exposes the concrete capability as `SdkGateway`, with inherent
methods that build the ruma content and call the SDK. It implements **no foreign
trait** — it does not know `MessageSender` exists.

`axon-server` (the composition root, which already depends on both) owns the
`GatewayAdapter` newtype: `impl MessageSender for GatewayAdapter`, delegating to
`SdkGateway` and mapping `axon_sync::GatewayError → axon_api::SendError`. The
orphan rule forces the impl into a local newtype here, which is exactly where
wiring belongs.

```
[core]   nothing new (stays pure data: Config, Error, LiveEvent)
[api]    trait MessageSender + enum SendError   (the port; SDK-free)
[sync]   ClientManager + SdkGateway             (concrete; no foreign trait)
[server] GatewayAdapter: impl MessageSender     (maps GatewayError→SendError)
```

**Why not the simpler alternatives.** Putting the trait in `axon-core` (like
`LiveEvent`) works but parks a *behavioral* contract in the foundation crate so
two higher crates can meet there — `LiveEvent` is data and feels at home there, a
send interface does not. Making `axon-api` depend on `axon-sync` directly removes
the indirection but pulls `matrix-sdk` into the API's build graph and makes
handler tests need a real `Client`. The consumer-owned port keeps `axon-api`
SDK-free *and* keeps the foundation crate pure, paying the one-time cost of a
~70-line adapter in the binary and a mechanical error-mapping.

### `ClientManager` owns connection; `SdkGateway` owns message semantics

Two responsibilities, two types:

- **`ClientManager`** is the single authority on whether a client exists for an
  account and how it's built. It owns `connect_account`'s logic (build the
  SQLite-backed client, login/restore), caches one Arc-backed `Client` per
  `account_id`, and exposes `get_or_connect` / `evict`. A **per-account
  single-flight** guard (an async mutex per slot) coalesces concurrent callers
  onto one connect rather than building two clients.
- **`SdkGateway`** owns message semantics only: resolve the client via
  `get_or_connect`, build the ruma content, issue the send. It knows nothing
  about retry or caching.

This keeps the manager free of edit-envelope knowledge and the gateway free of
connection concerns (single responsibility), and guarantees the gateway and the
sync task operate on the **same** client (same crypto store + send queue).

### Lazy connect; the supervisor still owns retry

The manager runs no loop of its own. The sync **supervisor** remains the always-on
driver that keeps each account online: its backoff loop calls
`manager.get_or_connect`, runs the `SyncService`, and on failure calls
`manager.evict` then backs off and retries — unchanged in cadence from before.
The **gateway connects lazily** through the same `get_or_connect`, so a send to a
not-yet-synced account drives (or coalesces onto) the connect.

Consequently, during a homeserver outage a send returns `503`/`502` and the
client retries — the correct behavior, since you cannot send through an
unreachable homeserver. Homeserver unreachability (maintenance, restarts, blips)
is a normal recurring condition, and this preserves the existing resilience: a
down homeserver never fails boot or a send permanently; the supervisor keeps
retrying and the next send succeeds once it returns.

### Routes, wire shape, and error mapping

Routes continue the M5a nested-account convention; `account_id` is in the path,
not the body:

- `POST   /v1/accounts/{account_id}/rooms/{room_id}/send`
- `PUT    /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}` (edit)
- `DELETE /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}` (redact; `?reason=`)
- `POST   /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}/reactions`

Every mutation returns `{ "data": { "event_id": "$…" } }` (`SendResultDto`) with
status `200`. The created event is **not** echoed in the response body — it
round-trips back through sync and appears in the timeline read and over `/v1/ws`,
the same path any event takes. Edits are sent as a raw `m.replace` envelope
(`m.new_content` + `m.relates_to`) so we don't need the original event in hand.

`SendError` maps 1:1 to status: `NotFound → 404` (unknown account/room),
`Forbidden → 403`, `Unavailable → 503` (couldn't connect), `Invalid → 400`
(bad id/body), `Upstream → 502` (homeserver rejected). A malformed request body
is a `400` in the same envelope, via a `Json` extractor wrapper mirroring the
existing `Path`/`Query` wrappers.

### Edit authorship is enforced by us, not the homeserver

A Matrix edit is an `m.replace` relation on a normal `m.room.message`. The rule
that *only the original author may edit* is a **client-interpretation** rule
(MSC2676) — the homeserver does **not** enforce it and will accept (and `200`) an
`m.replace` pointing at anyone's event. So the gateway's `edit` first fetches the
target event (`Room::event`) and rejects with `Forbidden` (→ `403`) unless its
sender is the account's own user. Otherwise we would send a forged edit of
someone else's message and report success — which a client that applies edits
without re-checking authorship would render. Redact is *not* analogous: redacting
others' messages is legitimately power-level-gated and the homeserver enforces it,
so we surface its `M_FORBIDDEN` as `403` too (via `client_api_error_kind`) rather
than a generic `502`.

## Consequences

- **Pro:** `axon-api` stays free of `axon-sync`/`matrix-sdk`; `axon-core` stays
  pure data. The gateway and sync share one client per account. Handler tests use
  a `StubSender` and need no homeserver. Connection retry stays in one place
  (the supervisor).
- **Con:** the decoupling costs a small adapter + error-mapping in the binary, and
  a send racing ahead of first sync (or during an outage) gets a transient
  `503`/`502` the client must retry.
- **Idempotency:** the SDK mints a fresh transaction id per `send` call, so a
  client that retries a send can create a duplicate. Acceptable pre-auth;
  revisit with M8 (a client-supplied idempotency/transaction key).
- **Scope:** plain-text messages and edits only (`m.text`); richer msgtypes and
  formatted bodies are additive later. No optimization yet to reuse one client
  across `SyncService` restarts (the supervisor evicts on failure and rebuilds);
  add it if reconnect churn matters.
- **Revisit** if mutations need richer content types, client-side idempotency
  keys (M8), or per-route status codes (`201` for creates).
```
