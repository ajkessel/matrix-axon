# axon-sync

matrix-rust-sdk sync engine wrapper (Simplified Sliding Sync only).

## Responsibility

Manages one `matrix_sdk::Client` per account, runs Simplified Sliding Sync (MSC4186), and feeds decrypted events into `axon-store`. No legacy `/sync` support.

## Owns vs. consumes

- **Owns:** per-account `matrix_sdk::Client` instances and their SQLite-backed crypto stores.
- **Consumes:** `axon-store` (to persist events) and `axon-core` config.

## Status

Stub — no public API yet.
