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
    tui/                     # axon-tui — terminal client for the Axon API, should grow to support all API endpoints as they are enabled
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
- **Clients:** client apps live under `clients/`. Follow any subtree `AGENTS.md` there; `clients/tui/AGENTS.md` covers axon-tui-specific conventions.
- **Sync:** Simplified Sliding Sync (MSC4186) only. No legacy `/sync`.
- **Event schema:** hybrid hot-columns + JSONB. `origin_ts` is `BIGINT` milliseconds since Unix epoch.
- **Provenance:** every event row has `provenance = 'upstream_homeserver'` for MVP.
- **API:** all routes under `/v1/`. WebSocket at `/v1/ws`. Envelope `{type, account_id, payload}`.
- **Migrations:** under `crates/axon-store/migrations/`, UTC timestamp prefixes (`YYYYMMDDHHMMSS_description.sql`, via `sqlx migrate add`) to avoid cross-branch collisions; forward-only — see ADR 0004. For any table that carries `updated_at TIMESTAMPTZ`, add a `BEFORE UPDATE` trigger (using a shared `trigger_set_updated_at()` plpgsql function) so application queries never need to remember `updated_at = now()` — the DB enforces it automatically.
- **Errors:** `thiserror` in libraries; `anyhow` only at the `axon-server` binary boundary.
- **Logging:** `tracing` with structured fields — always include `account_id`, `room_id`, `event_id` where applicable.
- **OpenAPI:** the spec is the source of truth. Handler types must compile against it (utoipa). Drift between spec and generated stubs is a bug.
- **Pull requests:** every PR body includes, by default, a **Verification guide** (prereqs + copy-pasteable, end-to-end steps that exercise real behavior — not just `cargo check`) and a **Code review guide** (a suggested file-by-file review order, dependencies first, plus a "where to keep a close eye" section calling out correctness, security, and lifetime concerns). Match the format of PRs #6 and #7. Scope both guides to the PR's actual diff.
- **`#N` is a GitHub autolink — never use it for anything else.** GitHub renders `#<number>` (in PR/issue bodies, comments, and commit messages) as a link to the issue or pull request with that number. Only write `#` immediately before a number when you mean a link to that exact, existing issue or PR. For every other numbered thing — review-comment indices, milestone phases, list items, ordinals, counts, versions — omit the `#` (write "comment 4", "step 3", "milestone 7a", "v2") so prose never sprouts bogus cross-links to unrelated issues.
- **What not to build:** no push (APNs/FCM), no admin API, no multi-human-per-process, no federation, no S3 media backend, no OAuth server — see `docs/mvp/implementation.md` "What not to build" for the full list.
- **Spelling:** U.S. English throughout all source files, comments, and docs (e.g. "initialize" not "initialise", "honors" not "honours").

Full conventions are in `docs/mvp/implementation.md` under "Conventions."

## Current state

**Milestone 7a in flight** (PR 1 of ~6) — M6 (mutations) is complete and the post-M6 sequence was rethought (see `docs/mvp/implementation.md` "Milestone resequencing" + ADR 0022). **M7** is account lifecycle & auth, in three phases: **7a** the Matrix-account lifecycle (login/verify/recover/logout/delete) — which also folds in the interactive SAS verification deferred from M5 (the old "5c") as its *last* PR; **7b** the client↔axon bearer-token gate (was M8); **7c** sender-device trust. The interactive-verification work and `axon-crypto` remain a stub until 7a's final PR.

PR 1 of 7a (the account state machine) is the only part landed so far — see its notes immediately below.

Non-obvious choices made in 7a (state machine — see ADR 0022):

- **Explicit `accounts.state` (`active` / `deactivated` / `deleting`), orthogonal to a `verified` flag.** `active` syncs+sends; `deactivated` is a reversible pause that **retains all data** (logout / token loss); `deleting` is a *transient* teardown breadcrumb a later boot reconcile completes (deletion is a hard row removal, no resting tombstone). A separate `verified` bool caches whether axon's own device is cross-signed (re-derived from the SDK, not write-once; its derivation is stubbed to `false` until a later 7a PR). `state` is never set directly by a client — it's a consequence of the lifecycle verbs.
- **Connection gated on `state = active` at the single choke point.** `Store::list_accounts()` returns **only `active` rows** (the safe default the sync boot loop iterates); `ClientManager::get_or_connect` refuses a non-active account with `GatewayError::AccountNotActive` (→ `403`), so the lazy gateway send path is gated too. This is the **groundwork** for #24, not the whole fix: the gate is **cold-connect only** (an already-cached client isn't re-checked), and nothing deactivates a stale row yet — so the invariant today is "a non-active row gets no *new* client", and the eviction-on-transition + stale-row reconcile/orphan-GC that actually retire #24's rows land in 7a-3/7a-4. Surfacing `deactivated`/`deleting` rows (read API, teardown reconcile, orphan GC) is a separate, explicitly-named accessor added when that caller lands — there is deliberately no shared "all rows" method to misfire from the connect path.
- **`AccountState` is a Rust enum in `axon-store`** (re-exported), stored as `TEXT` + `CHECK` (matches the rest of the schema, not a PG `ENUM`); an unknown stored value is a column-decode error, not a silent default.

Non-obvious choices made in 6 (see ADR 0021):

- **Consumer-owned port + composition-root adapter.** `axon-api` defines the `MessageSender` trait it needs (`send_message`/`edit`/`redact`/`react`; `AppState` holds `Arc<dyn MessageSender>`) and stays free of `axon-sync`/`matrix-sdk`. `axon-sync` exposes a concrete `SdkGateway` implementing **no foreign trait**; `axon-server` owns the `GatewayAdapter` newtype that `impl MessageSender` and maps `axon_sync::GatewayError → axon_api::SendError`. `axon-core` stays pure data. `axon-api` and `axon-sync` never depend on each other.
- **`ClientManager` (connection) vs `SdkGateway` (message semantics).** The manager is the single owner of per-account `Client`s — build/auth/cache via `get_or_connect`, `evict`, with a per-account single-flight guard. The gateway only builds ruma content and sends. Both share the *same* client per account (same crypto store + send queue).
- **Lazy connect; supervisor still owns retry.** The supervised sync loop remains the always-on driver (`get_or_connect` → run sync → on failure `evict` + backoff). The gateway connects lazily through the same `get_or_connect`. A send during a homeserver outage returns `503`/`502` and is retried by the client — correct, since unreachability is a normal recurring condition the supervisor rides out.
- **Wire shape.** Routes nest `account_id` in the path (M5a convention); every mutation returns `{ "data": { "event_id": "$…" } }` at `200`. The created event is not echoed — it round-trips through sync into the timeline read and `/v1/ws`. Edits are sent as a raw `m.replace` envelope. Redact reason is `?reason=`. The SDK mints a fresh txn id per send, so a client retry can duplicate (acceptable pre-auth; revisit M8).

Non-obvious choices made in 5b (see ADR 0020):

- **Live-event bus = `tokio::sync::broadcast`, owned by the sync engine.** `SyncEngine::live_events()` hands a `broadcast::Sender<LiveEvent>` clone to `AppState`; the `/v1/ws` handler `subscribe()`s once per connection. A slow client gets `RecvError::Lagged` (skips the backlog, stays connected) — sync is **never** back-pressured by a client. Capacity 1024; the producer skips the work entirely when `receiver_count() == 0`.
- **`LiveEvent` lives in `axon-core`** (the only crate both sibling producers/consumers share) and is **wire-neutral**; `axon-api` owns the envelope and maps `LiveEvent → EventDto`. The WS payload is the **same `EventDto`** the read API returns.
- **Wire envelope** `{ "type": "timeline.event", "account_id": <uuid>, "payload": <EventDto> }`. `type` is namespaced so M5c verification frames extend it without colliding. Live frames are always `redacted: false` (a redaction is a separate later event).
- **Live tail, not replay.** `/v1/ws` delivers events arriving *after* connect; history is the HTTP read API's job. Not in the OpenAPI doc (a WS upgrade isn't expressible in OpenAPI 3.1) — golden test unaffected. No auth yet (M8). Re-decryption back-fill of a UTD is **not** re-emitted over WS (documented scope limit).

Non-obvious choices made in 5a (see ADR 0019):

- **Account-nested canonical routes.** `GET /v1/rooms` is the flat cross-account aggregate (newest-activity first, optional `?account_id=` filter); detail routes nest the account: `GET /v1/accounts/{account_id}/rooms/{room_id}/timeline` and `GET /v1/accounts/{account_id}/events/{event_id}` (event under the **account, not the room** — the store keys events by `(account_id, event_id)`). A deliberate deviation from the spec's literal flat routes, consistent with M7's already-nested `/v1/media/{account_id}/…`. **Convention going forward: nest `account_id` on all account-scoped resource routes** — so M6 mutations become `/v1/accounts/{account_id}/rooms/{room_id}/send`, dropping `account_id` from the body.
- **Response envelope.** Success is `{ "data": <T> }` (`ApiResponse<T>`), errors `{ "error": { "code", "message" } }` (`ApiError`), each with one `IntoResponse`. `StoreError` → logged `500` with a generic body. Missing event → `404`; bad cursor/param → `400`; an unknown room's timeline is an empty `200` page, not `404`.
- **Opaque cursor.** The store's `(origin_ts, id)` `TimelineCursor` is serialized to the wire as base64url(`"{ts}.{id}"`), returned as `next_cursor` per page (`null` at the end); a malformed cursor is a `400`. Codec in `crates/axon-api/src/cursor.rs`.
- **`AppState` + `FromRef` seam.** `axon-api::router` takes `AppState { store }` and handlers extract `State<Store>` via `FromRef`, so 5b can add a `broadcast::Sender` field with zero churn to existing handlers.
- **OpenAPI golden file.** utoipa builds the spec from handler signatures; a DB-free test diffs it against `openapi/openapi.json` (regenerate with `UPDATE_OPENAPI=1 cargo test -p axon-api --test openapi`), making drift a CI failure. TypeScript client stubs are deferred to M11.
- **Store reads (no new tables/migration).** `list_rooms(Option<account_id>)` aggregates `events` for activity + latest event id and pulls name/topic/avatar/alias from the `room_state` projection in one query (note avatar state is `m.room.avatar` → `content.url`). `get_event(account_id, event_id)` reuses `room_timeline`'s read-time redaction masking via a shared `TIMELINE_SELECT` projection.

Non-obvious choices made in 4a (see ADR 0015):

- **Hot columns + indexes (`events`):** `redacts`, `relates_to` (JSONB, captured generically incl. `m.thread` but not thread-indexed — threads deferred), `decrypted_body_text`; plus `(account_id, room_id)` and a partial `(account_id, redacts) WHERE redacts IS NOT NULL` index. Unique stays `(account_id, event_id)` (account-scoped), a deliberate deviation from the spec's literal `(event_id)`.
- **Crypto sibling tables** keyed 1:1 by `(account_id, event_id)`, FK→`events` ON DELETE CASCADE: `event_ciphertext`, `event_megolm_session`, `event_sender_device_keys`. The megolm + device siblings come from the SDK's `EncryptionInfo` (handler extractor `Option<EncryptionInfo>` on the live path; `Arc<EncryptionInfo>` on the re-decryption path) for **every decrypted event**. **`event_ciphertext` is written on the UTD path only** — the SDK never surfaces the ciphertext of events it decrypts before dispatch, so live-decrypted events have no ciphertext row (documented limitation, ADR 0015). Sibling writes are best-effort: logged, never fatal to sync. The pure extractors live in `crates/axon-sync/src/meta.rs`.
- **`Store::room_timeline`:** newest-first, cursor on `(origin_ts, id)` (the `BIGSERIAL` `id` is the tiebreaker so pages never overlap/skip). Redaction is masked **at read time** via a `LEFT JOIN LATERAL` (LIMIT 1) — `content`/`decrypted_body_text` nulled, `redaction_event_id` set — leaving the stored row and ciphertext sibling untouched. No HTTP endpoint yet (that's M5); verified via `--ignored` store tests.
- **Sliding-sync timeline depth:** the SDK default is **1** (latest event only — the root of M3's latest-only archive). Raised via `SyncServiceBuilder::with_room_list_timeline_limit`, driven by new `sync.timeline_limit` (default 20). Deepens new syncs only; not retroactive backfill.
- **M4 re-scope (ADR 0011, 0015):** the recovery-key bootstrap landed in M3c (transient-only, kept that way — the M4 at-rest review is closed in ADR 0015); the interactive **verification plumbing moved wholly to M5** (untestable before `/v1/ws`), so `axon-crypto` stays a stub until M5.

Non-obvious choices made in 4b (see ADR 0016):

- **`room_state` / `account_data` are current-value projections**, upserted in place — not logs. The raw events still land in `events` (state events are part of the timeline); these tables hold the resolved latest value a room-summary or read-marker read needs. `room_state` PK is the Matrix state identity `(account_id, room_id, event_type, state_key)`; `account_data` PK is `(account_id, room_id, event_type)`.
- **Global account data uses `room_id = ''`** (a `NOT NULL DEFAULT ''` sentinel — real room ids start with `!`), so the natural PK carries uniqueness and `ON CONFLICT` targets it directly. A nullable `room_id` would make two global rows for one type "distinct" under SQL NULL semantics and defeat the upsert. The store API maps `room_id: Option<&str>` ↔ `''` at the boundary.
- **`room_state` upsert is freshness-guarded** (`ON CONFLICT … DO UPDATE … WHERE EXCLUDED.origin_ts >= room_state.origin_ts`): an older replayed state event can't clobber newer state. `account_data` events carry no timestamp, so theirs is plain last-write-wins.
- **Three new SDK handlers** (`crates/axon-sync/src/engine.rs`): `persist_state_event` (`AnySyncStateEvent`), `persist_room_account_data` (`AnyRoomAccountDataEvent`), `persist_global_account_data` (`AnyGlobalAccountDataEvent`). All reuse the one `PersistContext`. The global handler takes **no `Room` argument** — global account data has no room, and the SDK skips a handler whose `Room` extractor fails. `updated_at` on both tables is maintained by the shared `trigger_set_updated_at()` trigger.

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
- **E2EE key acquisition (deferred at 3a; see ADR 0011 Status):** a fresh `axon` device is unverified, so encrypted rooms show UTDs until it obtains keys. Two complementary paths (ADR 0011): the *bootstrap/fallback* account **recovery key** (Secure Storage / 4S) — one `recover()` call unlocks both key backup and cross-signing — **landed in M3c** (transient-only); the *mature* path, **BFF-proxied interactive verification** (axon streams the SAS emoji over `/v1/ws` so the user verifies the axon session from the axon client; after trust, the user's other devices gossip the secrets so the recovery key never touches the server), is **M5**.

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
