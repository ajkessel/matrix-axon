# axon-api

axum HTTP and WebSocket handlers; OpenAPI spec via utoipa.

## Responsibility

Implements all `/v1/` HTTP routes and the `/v1/ws` WebSocket endpoint. The OpenAPI 3.1 spec is emitted by utoipa from handler type signatures and is the source of truth for the wire protocol. TypeScript client stubs are generated from the spec into `clients/web/src/api/`.

## Owns vs. consumes

- **Owns:** route definitions and the axum `Router`.
- **Consumes:** `axon-store` (reads/writes), `axon-core` types and auth middleware.

## Public API surface

- `router(store: Store) -> axum::Router` — builds the top-level router with the
  `Store` held as router state. Versioned `/v1/` routes mount here in later
  milestones.

## Notes

- `/healthz` is an unversioned operational liveness probe: it always returns
  `200 {"status":"ok"}` and does **not** touch the database, so a transient DB
  outage does not trigger restarts.

## Status

`router()` builder + `/healthz` liveness probe (Milestone 2). No `/v1/` routes
or WebSocket yet.
