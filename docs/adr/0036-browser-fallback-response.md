# ADR 0036 — Browser fallback response

## Context

The Axon server is an API-only backend. Its Axum router defines explicit routes
under `/v1/` (all bearer-token-gated) and `/healthz` (unversioned liveness
probe); no fallback route is defined. When a user navigates to the server address
in a web browser — a natural thing to try when first setting up a self-hosted
instance — Axum returns a plain `404 Not Found` with no body. There is no
indication that the server is running, what it is, or how to interact with it.

This is the only unmatched-route behavior Axum provides out of the box. A small
informational response in its place would noticeably improve the first-run
experience without any cost to the API contract (the fallback only fires when no
registered route matches, so all existing routes are unaffected).

## Decision

Add an Axum `.fallback()` handler to the top-level router
(`crates/axon-api/src/lib.rs`). The handler returns a minimal HTML page
(`axum::response::Html`, `200 OK`) that:

- Identifies the server as "Axon."
- Briefly describes what it is (a self-hosted Matrix agent / API server).
- States that no web interface is served at this address and that a compatible
  client (e.g. `axon-tui`) is required.
- Points developers to `/healthz` and the `/v1/` API prefix.

The response is intentionally minimal — no CSS framework, no external resources,
no JavaScript. The goal is a legible human-readable message in a browser, not a
marketing page.

The handler returns `200 OK` rather than `404` because the server *is* running
and *is* reachable; the "not found" status would be misleading to a human reader
who reached the right host and port.

No existing routes, response envelopes, or authentication behavior change. The
fallback is last in the router resolution order and can never intercept a
path that a registered route would have matched.

## Consequences

- A browser navigating to the server's root (or any unregistered path) sees a
  plain HTML page instead of a blank 404.
- `axon-api` gains a dependency on `axum::response::Html`, which is already
  present in the `axum` crate the project uses — no new dependency.
- The OpenAPI document is unaffected (the fallback is not an API route and is
  not expressible as one).
- Smoke tests and integration tests are unaffected; they target registered routes.
