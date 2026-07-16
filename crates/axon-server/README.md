# axon-server

Axon binary — wires all crates together and owns the process.

## Responsibility

Entry point for the `axon` binary. Reads config, initializes all subsystems (`axon-store`, `axon-sync`, `axon-search`, `axon-media`, `axon-api`), and drives the main async runtime. Also provides the `axon token` CLI subcommand for minting and revoking bearer tokens.

## Owns vs. consumes

- **Owns:** the process and runtime; `anyhow` error handling at the binary boundary.
- **Consumes:** every other Axon crate.

## Notes

- Subcommand dispatch (clap): no subcommand runs the server; `axon token …`
  is a short-lived DB-only path (no sync engine, no HTTP listener).
- Server boot sequence: load `Config` → init `tracing` (honors `RUST_LOG`, else
  the full `tracing_subscriber::EnvFilter` directive in `log.level`) →
  `Store::connect` (runs migrations) → optionally arm the one-time web
  bootstrap when an interactive fresh instance has no accounts or credentials →
  build `axon_api::router` (with a
  `StoreTokenVerifier` for the M7b auth gate) → bind + `axum::serve` with
  graceful shutdown on Ctrl-C / SIGTERM.
- First-credential web bootstrap is a temporary startup-only surface at
  the per-boot `/bootstrap/<code>` URL printed at startup. It is loopback-only
  unless `server.bootstrap_web_allow_remote` is explicitly enabled, locks after
  six wrong bootstrap URLs, and closes permanently once any account, token, or
  OAuth identity exists. If `server.web_client_url` is set, its success pages
  link to that web client after showing the newly minted credential. The
  six-wrong-URL lockout counter is process-local and shared by every caller —
  with `bootstrap_web_allow_remote = true`, a remote scanner can lock the
  operator's own setup flow in six requests, forcing a restart.

## Status

Boots the HTTP server with config + DB bootstrap and `/healthz` (Milestone 2).
The `axon token` CLI subcommand (issue / list / revoke client bearer tokens)
landed in M7b (ADR 0029).
