# axon-core

Shared types, errors, and configuration for Axon.

## Responsibility

Provides the `Config` struct (loaded from TOML + env), the top-level error enum, and any primitive types shared across crates (e.g. `AccountId`).

## Owns vs. consumes

- **Owns:** nothing external — pure types and config parsing logic.
- **Consumed by:** every other Axon crate.

## Status

Stub — no public API yet.
