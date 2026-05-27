# Axon MVP — Implementation Specification

**Status:** Draft, intended as a self-contained brief for an agentic coder (Claude Code or similar) to scaffold and build the Axon MVP. **Related docs:** [`prd.md`](./prd.md), [`tech-spec.md`](./tech-spec.md). **Reads top to bottom.** A coder following this without reading the brief or tech spec should be able to scaffold the workspace, run Postgres, and reach Milestone 1.

## Project layout

End-state target. Create incrementally as milestones land.

```
matrix-axon/
  Cargo.toml                 # workspace
  crates/
    axon-server/             # binary; wires components together
    axon-core/               # shared types, errors, config
    axon-store/              # Postgres + sqlx; event store, account data
    axon-sync/               # matrix-rust-sdk sync engine wrapper
    axon-crypto/             # thin verification surface over rust-sdk crypto
    axon-search/             # Tantivy index
    axon-media/              # media proxy + storage backend
    axon-api/                # axum HTTP + WS handlers, OpenAPI (utoipa)
  clients/
    web/                     # axon-web (Vite + React + TS)
  openapi/                   # spec source of truth (handwritten + utoipa-emitted)
  docs/
    mvp/                     # this directory
    self-hosting.md          # produced in Milestone 12
  docker-compose.yml         # Postgres for dev
```

## Settled stack

- **Language:** Rust. Pick a recent stable edition; pin MSRV in `Cargo.toml` once initial scaffolding lands.
- **HTTP / WS:** axum.
- **DB:** Postgres via sqlx (compile-time-checked queries).
- **Matrix:** matrix-rust-sdk (sync, olm/megolm, key backup, cross-signing, verification surface).
- **Search:** Tantivy.
- **OpenAPI:** utoipa for type-checked spec emission from handler signatures.
- **Web alpha:** Vite + React + TypeScript.
- **Client stubs:** openapi-typescript-codegen (or equivalent) for TypeScript; an OpenAPI-to-Swift generator for the deferred iOS client (run but unused at MVP).
- **Object store:** local disk by default; S3-compatible adapter available behind a feature.

## Settled decisions inherited from [`tech-spec.md`](./tech-spec.md)

Read the tech spec before starting. Highlights that gate implementation:

- One Axon per human, N Matrix accounts inside. Every account-scoped table carries `account_id`.
- Event provenance: `events.provenance` defaults to `upstream_homeserver`.
- Event schema is hybrid hot-columns + JSONB.
- Live updates: WebSocket at `/v1/ws`, envelope `{type, account_id, payload}`.
- Auth: bearer tokens minted by an `axon` CLI subcommand.
- API versioning: all routes under `/v1/`.
- Sync: Simplified Sliding Sync only.
- Search: single language-agnostic Tantivy analyzer; `account_id` is a facet field.
- Bridges: pass through, no normalisation.
- Onboarding: fresh sync only.
- Push: not in scope; do not write push code paths.

## Milestones

Each milestone has explicit deliverables and a verification step that exercises real behaviour, not just `cargo check`. Stop and ask before deviating; if an ambiguity arises that the specs do not cover, raise it instead of picking silently.

### 1. Workspace scaffolding

- Create the Cargo workspace per the project layout.
- Empty crates with `lib.rs` / `main.rs` stubs and minimal `Cargo.toml` files.
- `docker-compose.yml` running Postgres 16 with a named volume.
- Basic CI: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`.

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
- Hot columns: `event_id`, `room_id`, `account_id`, `sender`, `origin_ts`, `type`, `redacts`, `relates_to`, `decrypted_body_text`.
- Full decrypted content as JSONB.
- Sibling tables for original ciphertext and megolm session metadata, linked by `event_id`.
- Indexes: `(room_id, origin_ts DESC)`, `(account_id, room_id)`, unique `(event_id)`, partial index on `redacts` where not null.
- Timeline read by room with pagination (cursor on `origin_ts`).
- Account data and room state tables.

**Verification:** SQL queries: paginate the most recent N events in a room; confirm sibling rows for ciphertext and megolm metadata exist for encrypted events; cursor-based pagination returns stable results across calls.

### 5. Client API v0

- axum routes under `/v1/`:
  - `GET /v1/rooms` (list rooms across all accounts; filterable by `account_id`).
  - `GET /v1/rooms/{room_id}/timeline` (paginated).
  - `GET /v1/events/{event_id}`.
- WebSocket at `/v1/ws`. Envelope: `{type, account_id, payload}`. Live timeline events fan out from the sync engine.
- OpenAPI spec emitted via utoipa; written in parallel.
- TypeScript stubs generated into `clients/web/src/api/`.

**Verification:** Boot the server, `curl /v1/rooms`, hit `/v1/rooms/{id}/timeline`, open a websocat session to `/v1/ws` and see live events arrive tagged with `account_id` as new events come in over sync.

### 6. Mutations

- `POST /v1/rooms/{room_id}/send` (send message; payload includes `account_id`).
- `PUT /v1/rooms/{room_id}/events/{event_id}` (edit).
- `DELETE /v1/rooms/{room_id}/events/{event_id}` (redact).
- `POST /v1/rooms/{room_id}/events/{event_id}/reactions` (react).
- All routed through matrix-rust-sdk's send path on the appropriate `Client`.

**Verification:** Send a message via curl, watch it round-trip through sliding sync, appear in the timeline, and arrive over WS.

### 7. Media proxy

- `axon-media` resolves MXC URLs against the upstream homeserver for the relevant account.
- Cache to local disk by default; S3-compatible adapter behind a feature.
- `GET /v1/media/{account_id}/{server}/{media_id}` with proper caching headers and range-request support.

**Verification:** Send a message with an image attachment, fetch the image through `/v1/media/…`, confirm it renders inline in `axon-web` (once Milestone 11 lands; until then, curl the URL and inspect the bytes).

### 8. Auth

- `axon token issue --label <name>` CLI subcommand. Mints a random token, stores a hash, prints the token once.
- `axon token list` and `axon token revoke <id>` subcommands.
- `tokens` table: `(id, label, hash, created_at, last_used_at, revoked_at)`.
- axum middleware validates `Authorization: Bearer …` on every `/v1/…` route; updates `last_used_at`; rejects revoked tokens.
- WebSocket auth: token in `Sec-WebSocket-Protocol` or initial envelope, validated on accept.

Design the token storage and middleware so a future OAuth 2.0 + PKCE issuer can replace the CLI mint path without breaking the on-the-wire `Authorization` header or any consumer code.

**Verification:** Issue a token; hit `/v1/rooms` with and without the header; revoke; confirm the next call is rejected.

### 9. Search backend

- `axon-search` opens a Tantivy index.
- Schema fields: `event_id`, `account_id` (facet), `room_id` (facet), `sender` (facet), `origin_ts` (date), `body` (text, default analyzer: tokenizer + lowercase + light stemming).
- Populate on event ingestion in the sync pipeline.
- `GET /v1/search?q=…&account_id=…&room_id=…&sender=…&from=…&to=…`.
- BM25 ranking; paginated.

**Verification:** Index a known corpus (e.g. dump 1000 events from a test room); assert that an exact phrase query returns the expected top hit; latency p95 under 200ms on the Steve-shape target.

### 10. Drafts and per-device read state

- Tables: `device_state` keyed by `(account_id, device_id, namespace, key)` with an opaque value blob and `updated_at`.
- Devices are identified by a client-supplied UUID at first registration.
- Endpoints: `GET/PUT /v1/devices/{device_id}/state/{namespace}`.
- Live sync via WS: changes broadcast to other devices owned by the same human; last-write-wins by `updated_at`.

**Verification:** Two `axon-web` tabs (acting as separate devices); typing a draft in one updates the other within a second.

### 11. Web alpha (`axon-web`)

- Vite + React + TypeScript app under `clients/web/`.
- Uses generated API stubs.
- Views:
  - Login (paste-token form).
  - Rooms list (across all accounts; sortable by recency).
  - Room view: paginated timeline with infinite scroll on backscroll.
  - Composer: send, edit, redact, react.
  - Inline media (images, video, audio) via `/v1/media/…`.
- No search UI. No verification UI. No multi-account switching UI beyond the implicit "rooms from all accounts in one list" — the data model supports N accounts; the alpha just doesn't expose account-switching controls.

**Verification:** Start docker-compose Postgres, start `axon`, issue a token, paste it into `axon-web`, exercise the chat loop in a real browser against a Synapse with real messages. Take a screenshot of the working timeline.

### 12. Self-hosting docs

- `docs/self-hosting.md` covering:
  - Prerequisites (Postgres, optional S3, Synapse / Dendrite accessible).
  - Build / install (Cargo + Docker options).
  - Config reference (every setting from `axon-core`'s config loader).
  - First-run flow: account provisioning, token minting, pointing `axon-web` at the agent.
  - Operational basics: backups (`pg_dump` + media directory), upgrades, logs.

**Verification:** A reader who has not touched the codebase follows the doc top to bottom on a fresh VM and reaches "Steve in a browser" — the MVP's sixth success criterion.

## Open decisions that gate milestones

None. All architectural decisions were resolved during planning (see the settled-decisions section of [`tech-spec.md`](./tech-spec.md) and the matching table at the end of that doc). If an ambiguity arises during implementation that neither the PRD nor the tech spec covers, stop and ask rather than picking silently.

## Conventions

- **Crate names.** `matrix-axon-*` on crates.io if we ever publish; internal workspace paths `crates/axon-*`. Binary name `axon`.
- **Migrations.** Under `crates/axon-store/migrations/`; numeric prefix; sqlx migrate.
- **Provenance.** All decrypted content rows include `account_id` and `provenance` (default `upstream_homeserver`) and link to original ciphertext rows.
- **Account scoping.** Every account-scoped table — rooms, events, room state, account data, device keys, drafts, read state, search index docs — carries `account_id` from day one. Cross-account aggregation happens in the API layer, not the store layer.
- **API routes.** All HTTP under `/v1/…`. WebSocket at `/v1/ws`. Envelope `{type, account_id, payload}` on every WS message.
- **OpenAPI.** The spec is the source of truth. Handler types must compile against it (utoipa). Drift between the spec and generated client stubs is a bug.
- **Errors.** `axon-core` defines the top-level error enum; crates re-export their own narrower errors that convert into it.
- **Logging.** `tracing` with structured fields including `account_id`, `room_id`, `event_id` where applicable.

## Verification per milestone (end-to-end, not just `cargo check`)

- **Sync milestones (3, 4).** Point at a Synapse-in-docker, watch decrypted rows accumulate in Postgres; query a known room's timeline by SQL.
- **API milestones (5, 6, 7, 9, 10).** curl against the running server using the generated TypeScript types as fixtures.
- **Auth milestone (8).** End-to-end: mint, use, revoke, confirm rejection.
- **Search milestone (9).** Index a known corpus, assert top results; measure p95 against the Steve-shape target.
- **Web alpha (11).** Real dev server, real browser, real Synapse; exercise send / edit / redact / react / media; screenshot the timeline.
- **Self-hosting (12).** A reader follows the doc on a fresh VM and reaches the MVP success criterion in under an hour.

## What not to build

Mirrors the PRD non-goals; the agent should not drift into these:

- No push code paths (no APNs, FCM, web push). The event-emit surface is designed to accept a router later; do not build the router.
- No multi-human (SaaS-tenant) isolation. One human per Axon.
- No federation hooks, no peer-to-peer ingestion.
- No native client scaffolding (iOS, desktop). Generated Swift stubs only.
- No admin API.
- No bridge metadata normalisation.
- No importers from existing clients.
- No full OAuth 2.0 server. Bearer tokens via CLI only.
- No advanced search UI in the alpha. The backend ships; the rich client doesn't.
- No verification UI in the alpha. The API surface ships; the client doesn't drive it.
