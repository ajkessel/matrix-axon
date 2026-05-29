# axon-server

Axon binary — wires all crates together and owns the process.

## Responsibility

Entry point for the `axon` binary. Reads config, initializes all subsystems (`axon-store`, `axon-sync`, `axon-search`, `axon-media`, `axon-api`), and drives the main async runtime. Also provides the `axon token` CLI subcommand for minting and revoking bearer tokens.

## Owns vs. consumes

- **Owns:** the process and runtime; `anyhow` error handling at the binary boundary.
- **Consumes:** every other Axon crate.

## Status

Stub — `main()` is empty.
