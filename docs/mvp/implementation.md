# Axon MVP — Implementation Specification

**Audience:** an agentic coder (Claude Code or similar) scaffolding and building the Axon MVP. Reads top to bottom; a coder following it without reading the other docs should be able to scaffold the workspace, run Postgres, and reach Milestone 1.

Related docs: [`prd.md`](./prd.md), [`tech-spec.md`](./tech-spec.md).

## Project layout

End-state target. Create incrementally as milestones land.

```
matrix-axon/
  Cargo.toml                 # workspace
  AGENTS.md                  # canonical orientation for agentic contributors
  CLAUDE.md                  # one-line pointer to AGENTS.md
  crates/
    axon-server/             # binary; wires components together
    axon-core/               # shared types, errors, config
    axon-store/              # Postgres + sqlx; event store, account data
    axon-sync/               # matrix-rust-sdk sync engine wrapper
    axon-crypto/             # thin verification surface over rust-sdk crypto
    axon-search/             # Tantivy index
    axon-media/              # media proxy + disk-cache backend
    axon-api/                # axum HTTP + WS handlers, OpenAPI (utoipa)
  (each crate has its own README.md + crate-level //! rustdoc)
  clients/
    tui/                     # axon-tui (terminal client; the alpha client)
  openapi/                   # spec source of truth (handwritten + utoipa-emitted)
  docs/
    mvp/                     # this directory
    adr/                     # architecture decision records
    self-hosting.md          # produced in Milestone 13
  docker-compose.yml         # Postgres for dev
```

`axon-tui` is the alpha client — a terminal client that exercises the full `/v1/` surface end-to-end. It replaced the originally-planned `axon-web` (Vite + React) as the reference client; the API is the deliverable, and the TUI is the integration surface that proves it. A web client remains a credible later addition consuming the same API.

## Settled stack

- **Language:** Rust. Pick a recent stable edition; pin MSRV in `Cargo.toml` once initial scaffolding lands.
- **HTTP / WS:** axum.
- **DB:** Postgres via sqlx (compile-time-checked queries).
- **Matrix:** matrix-rust-sdk (sync, olm/megolm, key backup, cross-signing, verification surface).
- **Search:** Tantivy.
- **OpenAPI:** utoipa for type-checked spec emission from handler signatures.
- **Alpha client:** `axon-tui`, a Rust terminal client, replacing the originally-planned Vite + React web alpha.
- **Client stubs:** an OpenAPI-to-Swift generator for the deferred iOS client (run but unused at MVP); TypeScript stubs remain available for a future web client. `axon-tui`, being Rust, consumes the API types directly.
- **Media backend:** local disk LRU cache. No S3 adapter in MVP.

## Settled decisions inherited from [`tech-spec.md`](./tech-spec.md)

Read the tech spec before starting. Highlights that gate implementation:

- One Axon per human, N Matrix accounts inside. Every account-scoped table carries `account_id`.
- Event provenance: `events.provenance` defaults to `upstream_homeserver`.
- Event schema is hybrid hot-columns + JSONB. `origin_ts` is `bigint` milliseconds since Unix epoch (matches Matrix `origin_server_ts`).
- Redactions are stored as events with `type = m.room.redaction` and `redacts = <event_id>`; the target row's content is masked at read time, original ciphertext / megolm metadata preserved.
- Live updates: WebSocket at `/v1/ws`, envelope `{type, account_id, payload}`.
- Auth: bearer tokens minted by an `axon` CLI subcommand.
- API versioning: all routes under `/v1/`.
- Sync: Simplified Sliding Sync only.
- Search: single language-agnostic Tantivy analyzer; `account_id` is a facet field.
- Bridges: pass through, no normalization.
- Onboarding: fresh sync only.
- Push: not in scope; do not write push code paths.

## Milestones

Each milestone has explicit deliverables and a verification step that exercises real behavior, not just `cargo check`. Stop and ask before deviating; if an ambiguity arises that the specs do not cover, raise it instead of picking silently.

### 1. Workspace scaffolding

- Create the Cargo workspace per the project layout.
- Empty crates with `lib.rs` / `main.rs` stubs and minimal `Cargo.toml` files.
- `docker-compose.yml` running Postgres 16 with a named volume.
- Basic CI: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.
- Create the initial `AGENTS.md` and a one-line `CLAUDE.md` pointer (see "Documentation for agentic contributors" below).
- Seed `docs/adr/` with `0001-record-architecture-decisions.md` (the meta-ADR adopting the practice).

**Verification:** `docker compose up -d postgres && cargo build` succeeds; CI passes on the first push.

### 2. Config + bootstrap

- Config loader in `axon-core` (figment or config-rs). Sources: TOML file + env overrides.
- `axon-store` opens a sqlx pool, runs migrations from `crates/axon-store/migrations/`.
- `axon-server` starts an axum server with a `/healthz` route.

**Verification:** Run `axon` against the docker-compose Postgres; `curl localhost:PORT/healthz` returns 200.

### 3. Sync engine v0

- `accounts` table in `axon-store`: `(account_id, user_id, homeserver_url, device_id, access_token_encrypted, sync_token, created_at, …)`.
- `axon-sync` wires one matrix-rust-sdk `Client` per account. MVP provisions a single account from config but the code path iterates over all rows in `accounts`.
- Run Simplified Sliding Sync; persist raw + decrypted events into `axon-store` scoped by `account_id` with `provenance = 'upstream_homeserver'`.

**Verification:** Point `axon` at a Synapse running in docker; log in a test account; watch decrypted rows accumulate in the events table over a fresh sync.

### 4. Event store schema

- Hybrid hot-columns + JSONB.
- Hot columns: `event_id`, `room_id`, `account_id`, `sender`, `origin_ts` (bigint ms), `type`, `redacts`, `relates_to`, `decrypted_body_text`.
- Full decrypted content as JSONB.
- Sibling tables for original ciphertext and megolm session metadata, linked by `event_id`.
- Indexes: `(room_id, origin_ts DESC)`, `(account_id, room_id)`, unique `(event_id)`, partial index on `redacts` where not null.
- Timeline read by room with pagination, reverse-chronological by default (cursor on `origin_ts`).
- Redaction handling: timeline reads mask `decrypted_body_text` for redacted events and emit a `redacted_because` field; original ciphertext / megolm metadata stay in sibling tables.
- Account data and room state tables.

**Verification:** SQL queries: paginate the most recent N events in a room reverse-chronologically; redact an event and confirm timeline reads mask its content while ciphertext sibling row remains; cursor-based pagination returns stable results across calls.

**E2EE key acquisition & device trust (ADR 0011).** A fresh `axon` device is unverified, so encrypted rooms show UTDs until it obtains keys. Two complementary paths; this milestone builds both pieces that can stand alone, the rest lands in Milestone 5:

- **Recovery-key bootstrap (build here, end-to-end).** `client.encryption().recovery().recover(key)` restores both the Megolm key backup (history) and the cross-signing private keys (so `axon` self-verifies and future keys flow). Add `sync.account.recovery_key`, encrypted at rest like the access token (ADR 0008) — prefer transient-only handling of this crown-jewel secret. This path needs no client, so it's what lets this milestone **prove decryption end-to-end before any front-end exists**.
- **Verification plumbing (build here, exercised in M7a).** The programmatic SDK flow — surface a `VerificationRequest`, `accept()`, read `sas.emoji()` / `sas.decimals()`, `confirm()` / `cancel()`. "Headless" means `axon` has no UI of its own, not that it can't verify; the SDK API is fully programmatic. The user-facing emoji exchange can't be exercised until the `/v1/ws` WebSocket exists (M5); the interactive verification UX itself was deferred and now lands in **M7a** alongside the rest of the Matrix-account lifecycle. Note `axon-crypto` is the "thin verification surface over rust-sdk crypto" from the project layout.

**Verification (E2EE):** against a real homeserver with key backup enabled, supply `sync.account.recovery_key`, confirm UTD rows flip to `decrypted = true` as backed-up keys arrive, and that `axon` shows as a verified/cross-signed device.

### 5. Client API v0

- axum routes under `/v1/`:
  - `GET /v1/rooms` (list rooms across all accounts; filterable by `account_id`; sorted by most-recent activity).
  - `GET /v1/rooms/{room_id}/timeline` (paginated, reverse-chronological by default).
  - `GET /v1/events/{event_id}`.
- WebSocket at `/v1/ws`. Envelope: `{type, account_id, payload}`. Live timeline events fan out from the sync engine.
- OpenAPI spec emitted via utoipa; written in parallel.
- TypeScript stubs (deferred — they targeted the dropped web alpha; `axon-tui` consumes the Rust API types directly, and a future web client can regenerate them from the OpenAPI spec).
- Define a shared `ApiResponse<T>` / `ApiError` envelope type in `axon-api` and a
  custom `IntoResponse` impl so all handlers return consistent JSON shapes and error
  bodies. (Deferred from M2; designing against zero real handlers is premature.)

**Verification:** Boot the server, `curl /v1/rooms`, hit `/v1/rooms/{id}/timeline`, open a websocat session to `/v1/ws` and see live events arrive tagged with `account_id` as new events come in over sync.

**Interactive verification UX deferred to M7a.** The SAS-emoji exchange over `/v1/ws` — formerly planned here as "M5c", consuming the M4 plumbing — was never built. It now lands in **M7a**, where it belongs conceptually: verifying `axon`'s *device* for a *Matrix account* is part of that account's lifecycle. M5 ships the `/v1/ws` channel the exchange rides on; M7a wires the flow. See M7a.

### 6. Mutations

- `POST /v1/rooms/{room_id}/send` (send message; payload includes `account_id`).
- `PUT /v1/rooms/{room_id}/events/{event_id}` (edit).
- `DELETE /v1/rooms/{room_id}/events/{event_id}` (redact).
- `POST /v1/rooms/{room_id}/events/{event_id}/reactions` (react).
- All routed through matrix-rust-sdk's send path on the appropriate `Client`.

**Verification:** Send a message via curl, watch it round-trip through sliding sync, appear in the timeline, and arrive over WS. Redact and confirm the timeline read masks content.

### 7. Account lifecycle and auth

Two layers of auth, split into subphases. **7a** brings the *Matrix* accounts under runtime control (login, verify, recover, logout) and finally closes the interactive-verification work deferred from M5 (the old "M5c"). **7b** puts the *client ↔ axon* bearer-token gate in front of the whole API. Said another way: 7a is auth between `axon` and the homeserver(s); 7b is auth between a client and `axon`.

#### 7a. Homeserver account lifecycle & verification

Today an account is provisioned exactly once from config, and there is no supported way to add, verify, or remove one at runtime. Changing `sync.account.user_id` in config does **not** replace the account — it inserts a new `accounts` row and strands the old one, which keeps syncing and can still *send* (any row with a decryptable token gets connected). This was hit in a real debugging session: a message went out authored by a previously-configured account that was no longer in config. 7a makes the Matrix-account lifecycle a first-class API and folds in the interactive device verification deferred from M5. It is the milestone that closes GH issues #14 (stale-DB cleanup) and #24 (account lifecycle / active-account gating / runtime provisioning).

**Account state machine.** Add an explicit lifecycle `state` to `accounts` — `active` or `deactivated` — kept *orthogonal* to verification status (a device can be `active` but not yet verified: it syncs and shows UTDs until it acquires keys). The sync engine **and** the M6 mutations gateway connect and serve **only `active` accounts** — never "any row with a decryptable token," the bug behind #24; `get_or_connect` gates on `state`. `deactivated` is a **reversible pause that retains all data** — a stale or token-expired account stops syncing and sending but is not erased (a natural home for the per-account failure isolation in ADR 0010). This is *not* a soft-delete model: `deactivated` is the soft stop; deletion (via logout, below) is a hard removal of the row, not a `deleted` tombstone.

**Lifecycle endpoints** (account-nested per the M5a convention; behind the 7b token gate once that lands):

- `POST /v1/accounts/login` — body `{ homeserver_url, username, password }`. `axon` logs in as a fresh device, mints an `account_id`, encrypts the access token at rest (ADR 0008), provisions the per-account SDK store dir, and starts sync. This is the supported way to add accounts #2…N without swapping config and stranding the prior account. The `password` is consumed once and never stored (matches the M3 login path) — a crown-jewel secret handled transient-only.
- `POST /v1/accounts/{account_id}/verify` — drives the interactive SAS (emoji) handshake: relay a `VerificationRequest` to/from another of the user's trusted devices, surface `sas.emoji()` / `sas.decimals()`, `confirm()` / `cancel()`. The emoji stream rides `/v1/ws` (the plumbing M4 built and M5 unblocked). After mutual confirm, `axon` is cross-signed and the user's other devices **gossip** the cross-signing secrets and the key-backup key — so the recovery key never has to live server-side. This is the mature key-acquisition path (ADR 0011).
- `POST /v1/accounts/{account_id}/recover` — the bootstrap path: accept a Secure-Storage (4S) recovery key and call `client.encryption().recovery().recover(key)`, which imports the megolm key **backup** and the cross-signing private keys into the per-account crypto store. Two effects: (1) holding the recovered user-signing key lets `axon` **self-verify its own device** with no interactive partner — "verify a device via backup-key recovery"; and (2) the imported keys let the existing M3c re-decryption queue flip already-stored UTD rows to `decrypted`. It does **not** fetch *history* — recovering *keys* is not the same as fetching *messages* (ADR 0011/0018). Pulling a room's pre-install timeline is **M8 backfill**, which consumes exactly these keys; this is why M7a (acquire keys + verify the device) deliberately precedes M8 (use them). The recovery-key *string* is transient-only — never persisted, consistent with the M3c boot-time `recover()` — but note that is distinct from the imported *backup keys*, which persist durably in the crypto store, so M8 still has them.
- `POST /v1/accounts/{account_id}/logout` — invalidate the device's token upstream, then **hard-delete every trace of the account from `axon`**: the `accounts` row itself (existing cascades drop `events` / `account_data` / `room_state`) **and** the on-disk SDK store at `data_dir/<account_id>/`. No tombstone is kept — re-adding the same Matrix account later is a fresh `login` with a new `account_id`. This is the destructive teardown that replaces today's manual DB surgery (#14); the *non-destructive* stop is `deactivated` (above).

**Store-dir GC.** Deletion removes the per-account store dir; add a boot-time reconcile that prunes orphan dirs under `data_dir/` with no matching active account (5 orphans were observed in #24).

**Verification status.** Persist per account whether `axon`'s device is verified / cross-signed (distinct from lifecycle `state`), so the API can report key-acquisition state and a client can prompt for verify-or-recover while the device is still unverified.

Out of scope here but explicitly tracked: `store_key` rotation (one key decrypts every account's token) stays deferred (ADR 0008), noted against #24 so it isn't lost once multi-account raises the stakes. Per-account *authorization* scoping remains a non-goal — one human owns all their accounts.

**Verification (7a):** `POST /v1/accounts/login` against a real homeserver provisions a second account that syncs independently; from a trusted Element session drive `POST …/verify`, watch the SAS emoji arrive over `/v1/ws`, confirm both sides, and see `axon` become cross-signed and subsequently-sent encrypted messages decrypt without a recovery key. Alternatively `POST …/recover` with a 4S key flips already-stored UTD rows to `decrypted` and marks the device verified (without fetching history — that's M8). `POST …/logout` removes the `accounts` row and the SDK store dir; confirm a `deactivated` account neither syncs nor can send while its data is retained.

#### 7b. Client ↔ axon bearer-token auth

The local-API gate. (This is the work formerly numbered M8.)

- `axon token issue --label <name>` CLI subcommand: mints a random token, stores a hash, prints the token once. `axon token list` and `axon token revoke <id>`.
- `tokens` table: `(id, label, hash, created_at, last_used_at, revoked_at)`.
- axum middleware validates `Authorization: Bearer …` on every `/v1/…` route — including the 7a lifecycle endpoints — updates `last_used_at`, and rejects revoked tokens.
- WebSocket auth: token in `Sec-WebSocket-Protocol` or the initial envelope, validated on accept.

Design the token storage and middleware so a future OAuth 2.0 + PKCE issuer can replace the CLI mint path without breaking the on-the-wire `Authorization` header or any consumer code. The first token is minted by the CLI (bootstrap); clients carry it thereafter. Until 7b lands, the 7a endpoints are unauthenticated like the rest of the pre-auth API — the M13 private-mesh-VPN deployment is the network gate in the interim, and the application gate that makes "no app auth yet" safe earlier.

**Verification (7b):** Issue a token; hit `/v1/rooms` with and without the header; revoke; confirm the next call is rejected. Confirm the M6 txn-id retry-duplication caveat is now attributable to a token.

### 8. History backfill

Sync alone only ingests events *going forward* (plus the shallow `sync.timeline_limit` window on a room's first sync — ADR 0015). Backfill is the engine that reaches back for a room's pre-existing history, so the timeline read, the M9 aggregations, and the M10 search index cover more than the post-install slice. `recover()` (M7a) imports the *keys* to decrypt old messages; it does not fetch the *messages* — those must be paged from the room. (ADR 0018.) Backfill runs here, ahead of aggregation and search, because both of those are only as good as the history they can see — together with sync this is what satisfies the PRD's full-history success criterion and the 100–200k-event working-set target.

- A bounded, **resumable** engine that pages backward through each room's timeline via the SDK's room pagination (`/messages`), decrypting with the keys already imported by `recover()` / gossip (M7a), and persisting through the **same ingestion path** as live sync — so hot columns, crypto siblings, redaction handling, and (once M10 lands) the search index all apply uniformly, and re-runs are idempotent (`ON CONFLICT DO NOTHING`).
- Per-room backfill state (e.g. a `room_backfill` table: `(account_id, room_id, oldest_seen_token, complete, updated_at)`) so progress survives restarts and the engine knows where to resume and when a room is exhausted.
- Background + throttled: rate-limited so it never starves live sync; configurable target depth (a bounded number of events/days, or "to room start").
- This retires the `sync.timeline_limit` bump as the "bounded substitute" for real backfill (ADR 0015).

**Verification:** Point `axon` at a room with substantial pre-existing history; confirm the stored event count climbs toward the room's full history rather than the initial window; confirm backfilled *encrypted* events decrypt (keys via `recover()`); kill and restart `axon` mid-backfill and confirm it resumes without duplicating rows. (Search over backfilled history is asserted in M10.)

### 9. Relation aggregation

Matrix expresses edits, reactions, replies, and threads as *relation* events — `m.relates_to` with `rel_type` `m.replace` (edit) / `m.annotation` (reaction) / `m.in_reply_to` (reply) / `m.thread`. `axon` already stores them as ordinary events with the relation captured in the `relates_to` hot column (ADR 0015). But reading them raw forces every client to re-aggregate over whatever timeline window it happens to hold, which silently breaks for relations that land *outside* that window — a reaction or an edit that arrives long after the original message. The TUI hit exactly this: reactions and edits to messages older than the loaded 50-event slice are dropped (GH issue #22). M9 moves aggregation server-side so the API serves resolved, complete views regardless of pagination.

This **subsumes the formerly-standalone Threads milestone** (old M13): a thread is just the `m.thread` case of the same machinery, and the store already captures `m.thread` generically, so the work is additive and backfill-free. Split: 9a builds the store-layer aggregation, 9b exposes it over the API.

#### 9a. Aggregation backend

- Expression / partial indexes over `events.relates_to` keyed by the target `event_id` and `rel_type` (generalizing the thread index sketched in ADR 0017), so "all relations pointing at event X" is an indexed lookup rather than a window scan. Applies retroactively to already-stored rows — additive, backfill-free.
- Store reads, all scoped by `(account_id, …)`:
  - **Edits (`m.replace`).** Resolve the latest edit per target by `origin_ts`; surface the replaced content plus edit metadata (`edited`, `edit_count`, `latest_edit_ts`). The raw edit events stay on disk (append-mostly; provenance and original ciphertext preserved — the same philosophy as redaction masking). The timeline read *collapses* them into the target rather than emitting standalone edit rows. matrix-rust-sdk has relation-aggregation support we can lean on, but the durable resolution is a store concern so it holds for events outside any client window.
  - **Reactions (`m.annotation`).** Group by target, then by `key`; per-emoji counts (plus the senders, for "did I react / who reacted").
  - **Replies (`m.in_reply_to`).** Direct replies to an event.
  - **Threads (`m.thread`).** Thread membership; a per-thread summary (root event + latest reply + reply count) and a thread-scoped timeline read (reuse the M5 cursor pagination, scoped to a thread root).
- Computed at read time for MVP — the indexes make it cheap at Riley scale. Incremental materialization (maintaining tallies on ingest) is a later optimization, not a re-architecture.

**Verification (9a):** Seed a room with a message, then add a reaction and an edit *far outside* the default timeline window; store-layer reads return the correct per-emoji count and the edited body regardless of window position; thread and reply lookups resolve over rows stored *before* this milestone (proving the backfill-free claim).

#### 9b. Aggregation API endpoints

Account-nested per the M5a convention:

- `GET /v1/accounts/{account_id}/events/{event_id}/reactions` — per-emoji tallies (issue #22 Option A): `{ "👍": { "count": 2, "me": true, "senders": […] }, "❤️": { … } }`.
- `GET /v1/accounts/{account_id}/events/{event_id}/replies` — direct replies to an event.
- `GET /v1/accounts/{account_id}/rooms/{room_id}/threads` — thread list (root + latest reply + reply count).
- `GET /v1/accounts/{account_id}/rooms/{room_id}/threads/{root_id}/timeline` — thread-scoped timeline, reverse-chronological with the same cursor pagination as the room timeline.
- **The M5 timeline read now returns aggregated events:** the latest edited body in place, standalone edit events stripped, plus a per-event `reactions` summary and `edited` / `edit_count` fields on `EventDto` (issue #22 Option B). An optional `GET …/events/{event_id}/edits` exposes the forensic edit history.
- WS (`/v1/ws`): raw relation events keep flowing live so clients can apply deltas, but the aggregation endpoints and the `EventDto` fields are the authoritative resolved view. Dedicated aggregation-update WS frames (a delivered tally delta) are a later add, not MVP.

**Verification (9b):** `GET …/reactions` returns grouped counts for a message whose reactions arrived in a later page; the timeline read shows the latest edited body with no stray edit rows; `GET …/threads` lists a thread with the correct reply count and its scoped timeline returns only that thread's events, reverse-chronological with stable pagination; an edit / reaction / reply sent over M6 round-trips and shows up aggregated.

### 10. Search

The Tantivy index. It runs after backfill (M8), so the corpus is deep, and after aggregation (M9), so the text it indexes is the *latest* edited body rather than a superseded one.

- `axon-search` opens a Tantivy index.
- Schema fields: `event_id`, `account_id` (facet), `room_id` (facet), `sender` (facet), `origin_ts` (date), `body` (text).
- `body` analyzer chain: default tokenizer + `LowerCaser` + `AsciiFoldingFilter` + `Stemmer` (English). All built-in Tantivy token filters — register the analyzer once and reference it from the field schema.
- Populate on event ingestion in the shared pipeline (so anything ingested — live sync *or* M8 backfill — is indexed by the same path).
- **Initial index build (one-time).** When M10 ships, the `events` table already holds everything synced (M3+) and backfilled (M8) before the index existed; the live path only indexes events arriving *after* it is wired. So a bulk pass streams the existing `events` rows (batched, ordered) into the index. It runs on first boot after search is enabled, gated by an index-built marker (search-index metadata / a `search_index_built` flag) so it does not repeat on later boots, and is also exposed as an `axon search reindex` CLI subcommand for schema-change rebuilds. The index is derived data keyed by `event_id`, so the pass is idempotent and a from-scratch rebuild is always safe — an interrupted build just re-runs.
- **Edit / redaction interaction (from M9):** an edit reindexes the target doc to the latest body (M9 makes "latest body" well-defined); a redaction removes the doc so search never surfaces deleted content (tech-spec).
- `GET /v1/search?q=…&account_id=…&room_id=…&sender=…&from=…&to=…`.
- BM25 ranking; paginated.
- No fuzzy/typo, synonym, or semantic search in MVP (see tech-spec search section). If a bounded fuzzy mode is wanted later, it's a query-time `FuzzyTermQuery` toggle on this endpoint, not an analyzer change.

**Verification:** Index a known corpus (e.g. dump 1000 events from a test room); assert an exact phrase query returns the expected top hit; confirm case- and diacritic-insensitivity (`cafe` matches `café`) and plural matching (`cat` matches `cats`); confirm a phrase from a **backfilled** pre-install message returns it, and an **edited** message is found by its new text and not its old; latency p95 under 200ms on the Riley-shape target.

### 11. Media proxy

- `axon-media` resolves MXC URLs against the upstream homeserver for the relevant account.
- Bounded LRU cache on local disk (size configurable; default 5GB).
- `GET /v1/media/{account_id}/{server}/{media_id}` with proper caching headers and range-request support.
- No S3 backend. Do not add one.

**Verification:** Send a message with an image attachment, fetch the image through `/v1/media/…`, confirm it renders inline in `axon-tui` (or curl the URL and inspect the bytes). Fill the cache past its limit and confirm LRU eviction.

### 12. Drafts and per-device read state

- Tables: `device_state` keyed by `(account_id, device_id, namespace, key)` with an opaque value blob and `updated_at`.
- Devices are identified by a client-supplied UUID at first registration.
- Endpoints: `GET/PUT /v1/devices/{device_id}/state/{namespace}`.
- Live sync via WS: changes broadcast to other devices owned by the same human; last-write-wins by `updated_at`.

**Verification:** Two `axon-tui` instances (acting as separate devices); typing a draft in one updates the other within a second.

### 13. Deployment docs

- `docs/self-hosting.md` covering:
  - Prerequisites (Postgres, Synapse / Dendrite accessible).
  - Build / install (Cargo + Docker options).
  - Config reference (every setting from `axon-core`'s config loader).
  - First-run flow: account provisioning via `POST /v1/accounts/login`, device verification (`POST …/verify` or `…/recover`), token minting, running `axon-tui`.
  - Operational basics: backups (`pg_dump` + media cache directory + the per-account SDK store dirs under `data_dir/`), upgrades, logs.
  - Deployment recipes — at minimum one each for:
    - Home machine behind a private mesh VPN (the recommended self-host path). axon + Postgres on hardware you own — the box under your desk, a home server, a NAS — reached from your other devices over a private network such as Tailscale, with **no port ever exposed to the public internet**. This best fits axon's premise: your data stays on your hardware. It also pairs with the M7b token auth as defense-in-depth — the VPN is the network gate, the token is the application gate (and the gate that makes "no app auth yet" safe in earlier milestones).
    - Railway (or a similar Procfile-style PaaS).
    - DigitalOcean droplet (Docker Compose + nginx reverse proxy + Let's Encrypt).
    - AWS (EC2 + RDS Postgres; ECS optional; reference Terraform welcome but not required).
    - Bare Linux VPS (covered in the operational basics above).

**Verification:** A reader who has not touched the codebase follows the doc top to bottom on a fresh VM and reaches the "daily-driver through `axon-tui`" PRD success criterion. At least one deployment recipe is exercised end-to-end (any of them) by someone other than the author.

## Milestone resequencing (post-M6)

Milestones 1–6 shipped as originally numbered. After M6, the sequence was rethought to reflect what the project actually needs next — a real account lifecycle, server-side relation aggregation, and a terminal client in place of the planned web alpha. The current plan (this document) supersedes the original M7–M13 ordering:

| Now | Was | Change |
|---|---|---|
| **7a** Homeserver account lifecycle & verification | — (new) + old "M5c" | Login / verify / recover / logout as a first-class API; closes the interactive verification deferred from M5 and GH issues #14, #24. |
| **7b** Client ↔ axon bearer-token auth | old M8 (Auth) | Unchanged in substance; renumbered. |
| **8** History backfill | old M9b | Promoted ahead of search; aggregation and search both depend on it. |
| **9** Relation aggregation (9a backend, 9b API) | — (new), subsumes old M13 (Threads) | Edits / reactions / replies / threads aggregated server-side (GH issue #22). Threads are the `m.thread` case, no longer a standalone milestone. |
| **10** Search | old M9a | Renumbered; now runs after backfill + aggregation. |
| **11** Media proxy | old M7 | Unchanged in substance; renumbered. |
| **12** Drafts & per-device read state | old M10 | Unchanged in substance; renumbered. |
| **13** Deployment docs | old M12 | Retargeted at `axon-tui`; first-run flow now uses the M7a account endpoints. |
| _(dropped)_ | old M11 (Web alpha) | `axon-tui` replaced `axon-web` as the alpha/reference client; it is tracked outside the milestone sequence. |

Older docs (`AGENTS.md` "Current state", the ADR log) still reference the original numbers as historical context; this table is the bridge. References to milestone numbers in those frozen/append-only docs are not retro-renumbered.

## Open decisions that gate milestones

The threads question carried over from [`tech-spec.md`](./tech-spec.md) is now resolved:

- **Threads — resolved: folded into M9 (Relation aggregation), in MVP.** Rather than a dedicated post-MVP milestone, threads ship as the `m.thread` case of the M9 aggregation machinery. The store captures `m.relates_to` generically (incl. `m.thread`) in `events.relates_to` (ADR 0015), so this is additive and backfill-free — the thread membership of every already-stored event is recoverable from data on disk. See Milestone 9.

Everything else is settled. If an ambiguity arises during implementation that neither the PRD nor the tech spec covers, stop and ask rather than picking silently.

## Conventions

Follow Matrix OSS community conventions first; fall back to standard Rust conventions where Matrix doesn't speak to the question. Match `matrix-rust-sdk`'s style and naming where there is overlap (event types in `snake_case` like the Matrix spec, room/event identifiers as opaque strings, error enums per crate with `thiserror`).

- **Crate names.** `matrix-axon-*` on crates.io if we ever publish; internal workspace paths `crates/axon-*`. Binary name `axon`.
- **Migrations.** Under `crates/axon-store/migrations/`; numeric prefix; sqlx migrate.
- **Provenance.** All decrypted content rows include `account_id` and `provenance` (default `upstream_homeserver`) and link to original ciphertext rows.
- **Account scoping.** Every account-scoped table — rooms, events, room state, account data, device keys, drafts, read state, search index docs — carries `account_id` from day one. Cross-account aggregation happens in the API layer, not the store layer.
- **API routes.** All HTTP under `/v1/…`. WebSocket at `/v1/ws`. Envelope `{type, account_id, payload}` on every WS message.
- **OpenAPI.** The spec is the source of truth. Handler types must compile against it (utoipa). Drift between the spec and generated client stubs is a bug.
- **Errors.** `axon-core` defines the top-level error enum; crates re-export their own narrower errors that convert into it. Use `thiserror` for definitions and `anyhow` only at binary boundaries.
- **Logging.** `tracing` with structured fields including `account_id`, `room_id`, `event_id` where applicable. Match `matrix-rust-sdk`'s span layout where the two libraries are in the same call path.

## Verification per milestone (end-to-end, not just `cargo check`)

- **Sync milestones (3, 4).** Point at a Synapse-in-docker, watch decrypted rows accumulate in Postgres; query a known room's timeline by SQL; confirm redactions mask content.
- **Account-lifecycle milestone (7a).** Login a second account at runtime; verify the device (interactive SAS over `/v1/ws`, or recovery-key); logout and confirm the DB rows and SDK store dir are gone and a stale account neither syncs nor sends.
- **API milestones (5, 6, 9b, 11, 12).** curl against the running server; assert aggregated reads (reactions/threads/edits) resolve outside the timeline window.
- **Auth milestone (7b).** End-to-end: mint, use, revoke, confirm rejection.
- **Backfill milestone (8).** Deep-history room: stored count climbs toward full history; restart mid-backfill resumes without duplicates.
- **Search milestone (10).** Index a known corpus, assert top results (including backfilled and edited messages); measure p95 against the Riley-shape target.
- **Deployment docs (13).** A reader follows the doc on a fresh VM and reaches the daily-driver success criterion in under an hour; at least one cloud recipe is exercised end-to-end.

## Documentation for agentic contributors

The OpenAPI spec covers the wire protocol but not the codebase. Future coding agents (Claude Code, Codex, Cursor, whatever comes next) need a separate set of in-repo docs to understand structure, intent, and non-obvious decisions. We maintain four:

### 1. `AGENTS.md` (canonical) + `CLAUDE.md` (pointer)

`AGENTS.md` at the repository root is the vendor-neutral orientation doc that most coding agents now look for by convention. `CLAUDE.md` is a one-line pointer to `AGENTS.md` so Claude Code finds it without us maintaining two copies.

- **Create** `AGENTS.md` during Milestone 1. Initial contents: project name, one-paragraph summary, pointer to `docs/mvp/`, the directory tree from "Project layout" above, a short conventions section that links to the "Conventions" section of this doc, and a "Current state" section that records which milestone is in flight.
- **Create** `CLAUDE.md` during Milestone 1 with one line: `See AGENTS.md.`
- **Update** `AGENTS.md` as you go. After every milestone, revise the "Current state" section and append any non-obvious design choices made during that milestone — library picks, schema details that aren't in the specs, build steps, gotchas. The next agent shouldn't have to reverse-engineer those.
- **Keep it short.** `AGENTS.md` is a high-density orientation, not a wiki. If a section grows past a page, break it out into a dedicated doc under `docs/` and leave a one-liner pointer behind.
- **Treat it as code.** Edits go through the same PR review as code changes.

Goal: any agentic contributor opening this repo cold reads `AGENTS.md` and is productive within minutes, without having to grep around or re-read the MVP specs.

### 2. Per-crate `README.md` + crate-level `//!` rustdoc

Every crate under `crates/` has a `README.md` and a crate-level doc comment (`//!`) in `lib.rs`. They serve different audiences but cover the same ground:

- **What this crate is responsible for** in one sentence.
- **Public API surface** at a glance — the main types and entry points.
- **Dependencies it owns** vs. dependencies it consumes (e.g. `axon-store` owns Postgres connections; `axon-api` consumes a `Store` handle).
- **Anything load-bearing that isn't obvious** from the code — invariants, "do not call this from inside a sync handler," etc.

Rustdoc renders for human-readable browsing on docs.rs (if we publish) and for `cargo doc --open` locally. The `README.md` is what an agent or human reads first when grepping by file. Keep them consistent; if they drift, the README is the source of truth and rustdoc is regenerated to match.

### 3. Architecture decision records under `docs/adr/`

Lightweight ADRs (Michael Nygard format — Context / Decision / Consequences, one page max) capture non-obvious decisions as they're made. Filename pattern: `NNNN-kebab-case-title.md`, monotonically numbered.

Write an ADR when:

- You pick one library over another for a non-trivial reason.
- You make a schema or API choice the specs don't prescribe.
- You discover an upstream bug or quirk and work around it.
- You decide *not* to do something that seems like an obvious next step.

The first ADR (`0001-record-architecture-decisions.md`) is the meta-ADR adopting the practice — created in Milestone 1.

Don't write ADRs for decisions the MVP specs already settle; those are anchored in `docs/mvp/` and re-stating them in ADRs creates drift. The ADR directory is for what happens *during* implementation that the specs don't cover.

### 4. `docs/mvp/` (this directory, locked at end of MVP)

PRD, tech spec, and implementation spec freeze at MVP ship. After that, they become historical reference — changes to product/architecture go through new docs (or new versions). An agent reading them later should treat them as "what we decided going in," not "current state."

The current state of the system lives in `AGENTS.md` and the ADR log.

## References and test corpus

We're not the first project in this space. Other Matrix clients have already hit the protocol's sharp edges; their issue trackers are a cheap source of edge cases we should cover from the start.

### Architectural references

Read these for lessons, not to copy code.

- **gomuks.** Closest architectural cousin — persistent backend, thin frontend, similar problem framing. Differences: single-user / single-account, no documented API, mautrix-go instead of matrix-rust-sdk. **Transfers:** protocol handling, sync edge cases, room-state management, redaction / edit semantics, what a server-side Matrix client actually has to do. **Doesn't transfer:** anything API-shaped or multi-account-scoped.
- **Element X / matrix-rust-sdk.** Same crypto and sync library we use. Their issue tracker is where library-level bugs surface first. **Transfers:** library gotchas, sliding-sync edge cases, megolm session handling, key-backup recovery flows. **Doesn't transfer:** their client-side state model — we own that on the server.
- **mautrix bridges** (mautrix-telegram, mautrix-discord, mautrix-whatsapp, etc.). Source of unusual bridged event shapes. Their issues reveal what real bridge traffic looks like and where bridges produce content our timeline rendering needs to tolerate.
- **Synapse / Dendrite issues.** Where homeserver-side quirks we have to tolerate get discussed (rate limits, sync response oddities, MSC4186 compliance gaps).

### Test corpus

A `tests/fixtures/` directory holds JSON event payloads, recorded sync responses, and end-to-end scenarios that drive the protocol-level test suite. Build it up by harvesting categories from the trackers above and add fixtures as new edge cases appear (whether discovered locally or upstream).

Categories to cover, all with at least one fixture before MVP ships:

- Gappy backfill (sliding sync delivering a window with gaps in the timeline).
- Megolm session loss → undecryptable events (UTDs) → key-backup recovery once keys arrive.
- Redaction edge cases: redaction of an edit, redaction of a redaction, redacted-while-decrypting.
- Room upgrades mid-conversation (`m.room.tombstone` arrives during active reading).
- Large rooms (10k+ members, multi-MB state events).
- Slow / flaky upstream homeservers (timeouts, partial sync responses, retry behavior).
- Bridged event shapes (mautrix `m.room.message` variants, bridge-specific `body` and `formatted_body` content, reply-to chains across the bridge).
- Malformed or unexpected events from upstream (we ignore-and-log, don't crash).

Treat this as a living checklist: when an upstream issue closes with "fixed bug in X edge case" and X is one we'd plausibly see, add the fixture for X if we don't have it.

## What not to build

Mirrors the PRD non-goals and out-of-scope items; the agent should not drift into these:

- No push code paths (no APNs, FCM, web push). The event-emit surface is designed to accept a router later; do not build the router.
- No multi-human-per-process isolation. One human per Axon.
- No federation hooks, no peer-to-peer ingestion.
- No native client scaffolding (iOS, desktop). Generated Swift stubs only.
- No admin API.
- No bridge metadata normalization.
- No importers from existing clients.
- No full OAuth 2.0 server. Bearer tokens via CLI only.
- No advanced search UI (faceted, semantic). Backend ships; a minimal search input in `axon-tui` ships; rich UI does not.
- No S3 / object-store media backend. Local disk LRU cache only.
- No spaces-specific endpoints. Events flow through.
- No `store_key` rotation. One key decrypts every account's token; rotation stays deferred (ADR 0008), tracked against #24.
- No incremental/materialized aggregation tallies in MVP. M9 aggregates at read time over indexed relations; maintaining counters on ingest is a later optimization.
