# axon-store

Postgres-backed event store, room state, and account data.

## Responsibility

Owns the `sqlx::PgPool`, runs migrations on startup, and (in later milestones)
exposes typed query methods for events, room state, account data, and device state.

## Owns vs. consumes

- **Owns:** the Postgres connection pool and all migrations under `migrations/`.
- **Consumes:** `axon-core` config and types.

## Public API surface

- `Store` — a cheaply-cloneable handle to the database.
  - `Store::connect(database_url, max_connections)` — opens the pool and runs
    pending migrations.
  - `Store::pool()` — borrow the underlying `PgPool`.
- `StoreError` — converts into `axon_core::Error`.

## Notes

- Migrations are embedded at compile time via `sqlx::migrate!("./migrations")`,
  so the deployed binary needs no migration files on disk.
- Migration files use UTC timestamp prefixes (`YYYYMMDDHHMMSS_description.sql`,
  via `sqlx migrate add`) to avoid version collisions across branches — see
  ADR 0004.
- TLS uses `tls-rustls` (no OpenSSL build dependency).
- The baseline migration only enables `pgcrypto`; the first application tables
  land in Milestones 3–4.

## Status

Connection pool + migration runner (Milestone 2). No query methods yet.
