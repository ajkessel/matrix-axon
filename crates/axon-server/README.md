# axon-server

Axon binary — wires all crates together and owns the process.

## Responsibility

Entry point for the `axon` binary. Reads config, initializes all subsystems (`axon-store`, `axon-sync`, `axon-search`, `axon-media`, `axon-api`), and drives the main async runtime. Also provides the `axon token` CLI subcommand for minting and revoking bearer tokens.

## Owns vs. consumes

- **Owns:** the process and runtime; `anyhow` error handling at the binary boundary.
- **Consumes:** every other Axon crate.

## Notes

- Boot sequence: load `Config` → init `tracing` (honors `RUST_LOG`, else
  the full `tracing_subscriber::EnvFilter` directive in `log.level`) →
  `Store::connect` (runs migrations) → build `axon_api::router`
  → bind + `axum::serve` with graceful shutdown on Ctrl-C / SIGTERM.

## Status

Boots the HTTP server with config + DB bootstrap and `/healthz` (Milestone 2).
The `axon token` CLI subcommand arrives in Milestone 8.
