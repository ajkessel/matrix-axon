# axon-store

Postgres-backed event store, room state, and account data.

## Responsibility

Owns the `sqlx::PgPool`, runs migrations, and exposes typed query methods for reading and writing events, room state, account data, and device state.

## Owns vs. consumes

- **Owns:** the Postgres connection pool and all migrations under `migrations/`.
- **Consumes:** `axon-core` config and types.

## Status

Stub — no public API yet.
