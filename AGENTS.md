# Axon — Contributor Orientation

Axon is a self-hosted personal agent for Matrix: a persistent state layer (sync, E2EE decryption, search, media proxy) that sits between a user's homeserver(s) and their clients. Arbitrary clients consume it through a stable, versioned HTTP + WebSocket API at `/v1/`. See `docs/mvp/prd.md` for the full product description.

## Docs

| File | Contents |
|---|---|
| `docs/mvp/prd.md` | Product requirements — what we're building and why |
| `docs/mvp/tech-spec.md` | Architecture decisions and tradeoffs |
| `docs/mvp/implementation.md` | Milestone-by-milestone build plan (authoritative for agentic contributors) |
| `docs/adr/` | Architecture decision records — decisions made during implementation |
| `docs/integration-testing.md` | Running axon against a local Synapse (sync + M3c re-decryption) by hand |
| `scripts/integration-test.sh` | One-command end-to-end re-decryption test: seeds an encrypted room + key backup via `axon-itest`, runs axon as a fresh device, and asserts UTDs back-fill. Also runs in CI (`.github/workflows/integration.yml`). |

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
    axon-itest/              # dev-only: integration-test seeder (the `seed` binary)
  clients/
    web/                     # axon-web (Vite + React + TS) — alpha client
  openapi/                   # OpenAPI 3.1 spec (source of truth)
  docs/
    mvp/                     # PRD, tech spec, implementation spec (frozen at MVP ship)
    adr/                     # architecture decision records
    self-hosting.md          # produced in Milestone 12
  docker-compose.yml         # Postgres 16 for dev; Synapse under `integration` profile
  scripts/
    integration-test.sh      # end-to-end E2EE re-decryption test vs local Synapse
  .github/workflows/
    lint-and-test.yml        # cargo fmt + clippy + test on every push/PR
    integration.yml          # E2EE re-decryption test (Synapse + Postgres) on PRs
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
- **Pull requests:** every PR body includes, by default, a **Verification guide** (prereqs + copy-pasteable, end-to-end steps that exercise real behavior — not just `cargo check`) and a **Code review guide** (a suggested file-by-file review order, dependencies first, plus a "where to keep a close eye" section calling out correctness, security, and lifetime concerns). Match the format of PRs #6 and #7. Scope both guides to the PR's actual diff.
- **What not to build:** no push (APNs/FCM), no admin API, no multi-human-per-process, no federation, no S3 media backend, no OAuth server — see `docs/mvp/implementation.md` "What not to build" for the full list.
- **Spelling:** U.S. English throughout all source files, comments, and docs (e.g. "initialize" not "initialise", "honors" not "honours").

Full conventions are in `docs/mvp/implementation.md` under "Conventions."

## Current state

**Milestone 3, subphase 3c complete** — on top of 3b's per-account sync + event persistence, the binary now back-fills UTDs: when megolm room keys arrive, a re-decryption queue finds the matching `content IS NULL` rows and writes their decrypted `content` + real `event_type`. A transient `recovery_key` config knob drives the queue end-to-end on a fresh device. Milestone 3 is complete; **Milestone 4 (event store schema + key lifecycle)** is next.

Non-obvious choices made in 3c (see ADR 0014):

- **Re-decryption queue (`crates/axon-sync/src/redecrypt.rs`):** two drivers — the SDK's `room_keys_received_stream()` (drains the pending UTDs for each arriving `(room_id, session_id)`) and a one-shot startup sweep over all of an account's `content IS NULL` rows (catches keys already in the crypto store or imported by `recover()` before we subscribe). The sweep first calls `backups().download_room_keys_for_room()` per room — `recover()` imports the backup *decryption key* but not the megolm room keys themselves, so on a quiet account nothing else would fetch them; the arrival-stream path skips the download (its keys just landed). Runs as a child task of `run_account` on a child `CancellationToken`, joined on return. Per-row failures are logged and skipped — never fatal to sync. ADR 0014.
- **`events.megolm_session_id` hot column + partial index:** UTDs lift `content.session_id` into a first-class column; `events_pending_utd_idx (account_id, room_id, megolm_session_id) WHERE content IS NULL` makes the arriving-key lookup an index hit rather than a JSONB scan. The back-fill `UPDATE … WHERE content IS NULL` guard is idempotent and won't clobber a row a live dispatch already decrypted.
- **Transient `recover()` (ADR 0011, 0014):** `sync.account.recovery_key` is consumed once on boot to import the megolm backup + cross-signing keys (the queue's driver on a fresh, unverified device). It is **never persisted** — not part of `Credential`, no column on `accounts`; durable at-rest storage + interactive verification stay in M4/M5. A wrong/rotated key is a readable `tracing::error`, non-fatal.

Non-obvious choices made in 3b (see ADR 0012):

- **Event persistence hook:** `Client::add_event_handler(persist_timeline_event)` with `AnySyncTimelineEvent` + `RawEvent` context. Registered on the `Client` before `SyncService::start()` so no events are missed during initial sync. matrix-rust-sdk decrypts Megolm payloads before dispatch, so `raw_event` in the `events` table holds the plaintext envelope for decrypted events (or the `m.room.encrypted` envelope — ciphertext + `session_id` — for UTDs). ADR 0012.
- **Timeline coverage is latest-event-only (current limitation):** the handler persists the events Simplified Sliding Sync surfaces, which is the *latest* timeline event per room — not full room history. Seeding a room with N messages and syncing a fresh device archives ~1 event (the most recent), so the integration test asserts on the count axon actually archived, not on how many were sent. Per-room timeline backfill is future work (M4+).
- **Events table:** `(id BIGSERIAL, event_id TEXT, room_id TEXT, account_id UUID, sender TEXT, origin_ts BIGINT, event_type TEXT, content JSONB, raw_event JSONB, provenance TEXT DEFAULT 'upstream_homeserver', received_at TIMESTAMPTZ)`. `raw_event` is the full event envelope as dispatched (plaintext for decrypted events, the `m.room.encrypted` ciphertext for UTDs); `content` is the extracted decrypted payload. Unique on `(account_id, event_id)` — upsert is idempotent. `content` is nullable: UTDs arrive as `m.room.encrypted` with `content = NULL`; the M3c re-decryption queue back-fills those rows, reading the ciphertext straight from `raw_event`. Index on `(account_id, room_id, origin_ts DESC)` for timeline reads. M4 ("Event store schema") adds hot-column refinements and sibling tables holding the original ciphertext + megolm session metadata + sender device keys (keyed by `event_id`, for every event) so decrypted rows stay re-verifiable against Matrix's signatures — distinct from M3c re-decryption, which reads UTD ciphertext straight from `raw_event`.
- **sqlx JSONB:** added `json` feature to `sqlx-postgres` (transitively enables `sqlx-core/json`) so `serde_json::Value` binds as JSONB.
- **Ctrl-C shutdown investigation (ADR 0013):** a post-3b hang turned out to be caused by an inherited blocked signal mask in the developer's terminal session (`SIGINT` blocked in the kernel pending queue, never delivered to any handler). No code change to axon was needed. The original `with_graceful_shutdown` + `tokio::signal::ctrl_c()` shutdown path is correct. If Ctrl-C does not work, run `python3 -c "import signal; print(signal.pthread_sigmask(signal.SIG_BLOCK, []))"` — if `2` appears, open a new terminal window.

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
