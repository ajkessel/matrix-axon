# ADR 0003 — Postgres access via sqlx with embedded migrations

## Context

`axon-store` owns all Postgres access. Milestone 2 wires the connection pool and
a migration runner. We need to decide the sqlx feature set, the TLS backend, how
migrations are shipped, and what the first migration contains given no
application tables exist until Milestones 3–4.

## Decision

Use **sqlx** with features `runtime-tokio`, `tls-rustls`, `postgres`, `migrate`,
`uuid`, `chrono`, `macros`.

- **`tls-rustls`** (not native-tls) avoids an OpenSSL system build dependency,
  keeping the build self-contained across dev and CI.
- **Embedded migrations** via `sqlx::migrate!("./migrations")`: migrations are
  compiled into the binary, so a deployed `axon` needs no migration files on disk.
- **`uuid` + `chrono`** are enabled now though unused, pre-staging the UUID
  `account_id` keys and millisecond timestamps that arrive in Milestone 3.
- **Baseline migration `0001_baseline.sql`** enables the `pgcrypto` extension
  (`gen_random_uuid()` for UUID primary keys; `digest()`/`crypt()` for token
  hashing in Milestone 8). It creates no tables — those land in M3–4 — but gives
  the migration runner a real, idempotent first revision.
- The `Store` handle wraps `PgPool` and is `Clone` (the pool is
  reference-counted) so it can be shared across axum handlers via router state.

## Consequences

- No `query!`/`query_as!` macros exist yet, so sqlx's compile-time query
  verification is **not** triggered this milestone: `DATABASE_URL`-at-build-time
  and the `.sqlx` offline cache are non-issues until Milestone 3. When the first
  checked queries land, CI will need either a Postgres service container with
  migrations applied before build, or a committed `.sqlx` cache with
  `SQLX_OFFLINE=true`. That decision is deferred to M3 (record an ADR then).
- CI for M2 stays DB-free: `cargo test --all` runs no test that opens a
  connection. End-to-end DB verification is done manually against the
  docker-compose Postgres.
