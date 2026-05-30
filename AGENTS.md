# Axon — Contributor Orientation

Axon is a self-hosted personal agent for Matrix: a persistent state layer (sync, E2EE decryption, search, media proxy) that sits between a user's homeserver(s) and their clients. Arbitrary clients consume it through a stable, versioned HTTP + WebSocket API at `/v1/`. See `docs/mvp/prd.md` for the full product description.

## Docs

| File | Contents |
|---|---|
| `docs/mvp/prd.md` | Product requirements — what we're building and why |
| `docs/mvp/tech-spec.md` | Architecture decisions and tradeoffs |
| `docs/mvp/implementation.md` | Milestone-by-milestone build plan (authoritative for agentic contributors) |
| `docs/adr/` | Architecture decision records — decisions made during implementation |

## Directory layout

```
matrix-axon/
  Cargo.toml                 # workspace
  AGENTS.md                  # this file
  CLAUDE.md                  # one-line pointer to this file
  crates/
    axon-server/             # binary; wires components together
    axon-core/               # shared types, errors, config
    axon-store/              # Postgres + sqlx; event store, account data
    axon-sync/               # matrix-rust-sdk sync engine wrapper
    axon-crypto/             # thin verification surface over rust-sdk crypto
    axon-search/             # Tantivy index
    axon-media/              # media proxy + disk-cache backend
    axon-api/                # axum HTTP + WS handlers, OpenAPI (utoipa)
  clients/
    web/                     # axon-web (Vite + React + TS) — alpha client
  openapi/                   # OpenAPI 3.1 spec (source of truth)
  docs/
    mvp/                     # PRD, tech spec, implementation spec (frozen at MVP ship)
    adr/                     # architecture decision records
    self-hosting.md          # produced in Milestone 12
  docker-compose.yml         # Postgres 16 for dev
  .github/workflows/
    lint-and-test.yml        # cargo fmt + clippy + test on every push/PR
```

## Key conventions

- **One human per Axon process.** N Matrix accounts inside, every account-scoped table carries `account_id`.
- **Sync:** Simplified Sliding Sync (MSC4186) only. No legacy `/sync`.
- **Event schema:** hybrid hot-columns + JSONB. `origin_ts` is `BIGINT` milliseconds since Unix epoch.
- **Provenance:** every event row has `provenance = 'upstream_homeserver'` for MVP.
- **API:** all routes under `/v1/`. WebSocket at `/v1/ws`. Envelope `{type, account_id, payload}`.
- **Migrations:** under `crates/axon-store/migrations/`, UTC timestamp prefixes (`YYYYMMDDHHMMSS_description.sql`, via `sqlx migrate add`) to avoid cross-branch collisions; forward-only — see ADR 0004.
- **Errors:** `thiserror` in libraries; `anyhow` only at the `axon-server` binary boundary.
- **Logging:** `tracing` with structured fields — always include `account_id`, `room_id`, `event_id` where applicable.
- **OpenAPI:** the spec is the source of truth. Handler types must compile against it (utoipa). Drift between spec and generated stubs is a bug.
- **What not to build:** no push (APNs/FCM), no admin API, no multi-human-per-process, no federation, no S3 media backend, no OAuth server — see `docs/mvp/implementation.md` "What not to build" for the full list.
- **Spelling:** U.S. English throughout all source files, comments, and docs (e.g. "initialize" not "initialise", "honors" not "honours").

Full conventions are in `docs/mvp/implementation.md` under "Conventions."

## Current state

**Milestone 2 complete** — the binary boots: typed config, Postgres pool + migrations, and an axum server with `/healthz`.

Non-obvious choices made in Milestone 2 (see ADRs 0002–0003):

- **Config:** figment (TOML + env). Precedence low→high: defaults < TOML file < `DATABASE_URL` < `AXON_`-prefixed env (`__` = nesting, e.g. `AXON_SERVER__PORT`). File resolved from `$AXON_CONFIG`, else `./axon.toml`, else env-only. Sample at `axon.toml.example`.
- **Defaults:** bind `127.0.0.1:8080`; pool `max_connections = 5`; log `info` (overridable by `RUST_LOG`).
- **Store:** sqlx with `tls-rustls` (no OpenSSL); migrations embedded via `sqlx::migrate!`. `Store` is a `Clone` handle over `PgPool`, shared into `axon-api` as router state.
- **Migrations:** baseline migration only enables `pgcrypto` (no tables until M3–4). Timestamp-prefixed filenames — see ADR 0004.
- **`/healthz`:** liveness-only, always 200, no DB ping.
- **Errors:** top-level `axon_core::Error` is acyclic — its `Store(String)` variant carries a message so `axon-core` need not depend on `axon-store`; leaf crates impl `From<LeafError> for axon_core::Error`.
- **CI:** unchanged. No `query!` macros yet, so sqlx compile-time checks aren't triggered and tests need no DB. When checked queries land in M3, add a Postgres service or a `.sqlx` offline cache.
- **Pre-commit hook:** `.githooks/pre-commit` runs the fmt + clippy subset of CI; enable per clone with `./scripts/setup-hooks.sh` (`core.hooksPath`). Full `cargo test` stays in CI.

Next: **Milestone 3** — `accounts` table in `axon-store`; `axon-sync` wires one matrix-rust-sdk `Client` per account and runs Simplified Sliding Sync, persisting events scoped by `account_id`.
