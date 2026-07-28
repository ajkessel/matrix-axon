# axon-api

axum HTTP and WebSocket handlers; OpenAPI spec via utoipa.

## Responsibility

Implements all `/v1/` HTTP routes and the `/v1/ws` WebSocket endpoint. The OpenAPI 3.1 spec is emitted by utoipa from handler type signatures and is the source of truth for the wire protocol. TypeScript client stubs are generated from the spec into `clients/web/src/api/`.

## Owns vs. consumes

- **Owns:** route definitions, the axum `Router`, and the bearer-token auth gate
  (the `TokenVerifier` port + `require_bearer` middleware, M7b).
- **Consumes:** `axon-store` (reads/writes) and `axon-core` types.

## Public API surface

- `router(store: Store) -> axum::Router` — builds the top-level router with the
  `Store` held as router state. Versioned `/v1/` routes mount here in later
  milestones.

## Notes

- `/healthz` is an unversioned operational liveness probe: it always returns
  `200 {"status":"ok"}` and does **not** touch the database, so a transient DB
  outage does not trigger restarts. It is the one route outside the auth gate.
- Every `/v1/` route — HTTP and the `/v1/ws` WebSocket — requires a bearer token
  (M7b, ADR 0029). The HTTP gate is a single `require_bearer` layer; the socket
  authenticates at upgrade time (a browser can't set the header on a socket).

## Status

`router()` builder + `/healthz` liveness probe (Milestone 2). No `/v1/` routes
or WebSocket yet.
