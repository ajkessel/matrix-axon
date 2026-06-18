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
  `Store::connect` (runs migrations) → build `axon_api::router` (with a
  `StoreTokenVerifier` for the M7b auth gate) → bind + `axum::serve` with
  graceful shutdown on Ctrl-C / SIGTERM.

## Status

Boots the HTTP server with config + DB bootstrap and `/healthz` (Milestone 2).
The `axon token` CLI subcommand (issue / list / revoke client bearer tokens)
landed in M7b (ADR 0029).
