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
- **Migrations:** under `crates/axon-store/migrations/`, UTC timestamp prefixes (`YYYYMMDDHHMMSS_description.sql`, via `sqlx migrate add`) to avoid cross-branch collisions; forward-only — see ADR 0004. For any table that carries `updated_at TIMESTAMPTZ`, add a `BEFORE UPDATE` trigger (using a shared `trigger_set_updated_at()` plpgsql function) so application queries never need to remember `updated_at = now()` — the DB enforces it automatically.
- **Errors:** `thiserror` in libraries; `anyhow` only at the `axon-server` binary boundary.
- **Logging:** `tracing` with structured fields — always include `account_id`, `room_id`, `event_id` where applicable.
- **OpenAPI:** the spec is the source of truth. Handler types must compile against it (utoipa). Drift between spec and generated stubs is a bug.
- **What not to build:** no push (APNs/FCM), no admin API, no multi-human-per-process, no federation, no S3 media backend, no OAuth server — see `docs/mvp/implementation.md` "What not to build" for the full list.
- **Spelling:** U.S. English throughout all source files, comments, and docs (e.g. "initialize" not "initialise", "honors" not "honours").

Full conventions are in `docs/mvp/implementation.md` under "Conventions."

## Current state

**Milestone 3, subphase 3b complete** — the binary provisions the configured account, logs in (or restores a session), runs Simplified Sliding Sync per account, and persists every incoming Matrix timeline event into Postgres scoped by `account_id`. Decryption robustness and the edge-case corpus (3c) are next.

Non-obvious choices made in 3b (see ADR 0012):

- **Event persistence hook:** `Client::add_event_handler(persist_timeline_event)` with `AnySyncTimelineEvent` + `RawEvent` context. Registered on the `Client` before `SyncService::start()` so no events are missed during initial sync. matrix-rust-sdk decrypts Megolm payloads before dispatch, so `raw_content` in the `events` table always holds plaintext (or the `m.room.encrypted` wrapper for UTDs). ADR 0012.
- **Events table:** `(id BIGSERIAL, event_id TEXT, room_id TEXT, account_id UUID, sender TEXT, origin_ts BIGINT, event_type TEXT, content JSONB, raw_content JSONB, provenance TEXT DEFAULT 'upstream_homeserver', received_at TIMESTAMPTZ)`. Unique on `(account_id, event_id)` — upsert is idempotent. `content` is nullable: UTDs arrive as `m.room.encrypted` with `content = NULL`; the M3c re-decryption queue will back-fill those rows. Index on `(account_id, room_id, origin_ts DESC)` for timeline reads. M4 adds hot-column refinements and sibling ciphertext/session tables.
- **sqlx JSONB:** added `json` feature to `sqlx-postgres` (transitively enables `sqlx-core/json`) so `serde_json::Value` binds as JSONB.

Non-obvious choices made in 3a (see ADRs 0006–0008, 0010, 0011):

- **Dependency conflict:** adding `matrix-sdk` (→ rusqlite → `libsqlite3-sys 0.35`) collides with the `sqlx` umbrella's `sqlx-sqlite` (`libsqlite3-sys` 0.28–0.30) over the `links = "sqlite3"` native lib. Fix: drop the umbrella for `sqlx-core` + `sqlx-postgres` directly (pinned `=0.8.2`), embed migrations via `include_dir` (hand-built `Migrator`, same checksum format as `sqlx::migrate!`), and align sqlx onto the **aws-lc-rs** rustls provider to match matrix-sdk (two providers → runtime TLS panic). ADR 0006.
- **Sync:** `matrix_sdk_ui::sync_service::SyncService` (not the low-level `SlidingSync`); one `Client` + one SQLite store per account under `sync.data_dir/<account_id>`; one supervised task per account, restarted with exponential backoff on `SyncService` `Error`/`Terminated`; `CancellationToken` for graceful drain. ADR 0007.
- **Auth:** login once, restore thereafter. Access token stored encrypted at rest via pgcrypto `pgp_sym_encrypt` keyed by `sync.store_key`; password consumed once, never stored. Tokens treated as long-lived; `M_UNKNOWN_TOKEN` recovery + MSC2918 refresh deferred to M4. ADRs 0008, 0010.
- **sqlx queries:** runtime `query`/`query_as` (no `query!` macros, no compile-time DB); `FromRow` hand-implemented. CI stays DB-free; account store methods covered by `#[ignore]` integration tests (`cargo test -p axon-store -- --ignored` with `DATABASE_URL`).
- **E2EE key acquisition (deferred):** a fresh `axon` device is unverified, so encrypted rooms show UTDs until it obtains keys. Two complementary paths (ADR 0011): the *mature* path is **BFF-proxied interactive verification** — axon streams the SAS emoji over `/v1/ws` so the user verifies the axon session from the axon client (M4/M5+); after trust, the user's other devices gossip the cross-signing + backup secrets, so the recovery key never touches the server. The *bootstrap/fallback* (no client yet) is the account **recovery key** (Secure Storage / 4S): one `recover()` call unlocks both key backup and cross-signing (M4).

---

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

Next: **Milestone 3, subphase 3b** — the `events` table and `archive.rs`: consume the SDK's room-update firehose and persist events (raw + decrypted) into Postgres scoped by `account_id`.
