# Axon MVP — Implementation Specification

**Audience:** an agentic coder (Claude Code or similar) scaffolding and building the Axon MVP. Reads top to bottom; a coder following it without reading the other docs should be able to scaffold the workspace, run Postgres, and reach Milestone 1.

Related docs: [`prd.md`](./prd.md), [`tech-spec.md`](./tech-spec.md).

## Project layout

End-state target. Create incrementally as milestones land.

```
matrix-axon/
  Cargo.toml                 # workspace
  CLAUDE.md                  # living context for agentic contributors
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
    web/                     # axon-web (Vite + React + TS)
  openapi/                   # spec source of truth (handwritten + utoipa-emitted)
  docs/
    mvp/                     # this directory
    self-hosting.md          # produced in Milestone 12
  docker-compose.yml         # Postgres for dev
```

`axon-web` is the seed of the eventual full web client, not a throwaway. Keep the directory name, evolve the contents.

## Settled stack

- **Language:** Rust. Pick a recent stable edition; pin MSRV in `Cargo.toml` once initial scaffolding lands.
- **HTTP / WS:** axum.
- **DB:** Postgres via sqlx (compile-time-checked queries).
- **Matrix:** matrix-rust-sdk (sync, olm/megolm, key backup, cross-signing, verification surface).
- **Search:** Tantivy.
- **OpenAPI:** utoipa for type-checked spec emission from handler signatures.
- **Web alpha:** Vite + React + TypeScript.
- **Client stubs:** openapi-typescript-codegen (or equivalent) for TypeScript; an OpenAPI-to-Swift generator for the deferred iOS client (run but unused at MVP).
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
- Create the initial `CLAUDE.md` (see "Maintaining CLAUDE.md" below).

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

### 5. Client API v0

- axum routes under `/v1/`:
  - `GET /v1/rooms` (list rooms across all accounts; filterable by `account_id`; sorted by most-recent activity).
  - `GET /v1/rooms/{room_id}/timeline` (paginated, reverse-chronological by default).
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

**Verification:** Send a message via curl, watch it round-trip through sliding sync, appear in the timeline, and arrive over WS. Redact and confirm the timeline read masks content.

### 7. Media proxy

- `axon-media` resolves MXC URLs against the upstream homeserver for the relevant account.
- Bounded LRU cache on local disk (size configurable; default 5GB).
- `GET /v1/media/{account_id}/{server}/{media_id}` with proper caching headers and range-request support.
- No S3 backend. Do not add one.

**Verification:** Send a message with an image attachment, fetch the image through `/v1/media/…`, confirm it renders inline in `axon-web` (once Milestone 11 lands; until then, curl the URL and inspect the bytes). Fill the cache past its limit and confirm LRU eviction.

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
- Remove index entries when their event is redacted.
- `GET /v1/search?q=…&account_id=…&room_id=…&sender=…&from=…&to=…`.
- BM25 ranking; paginated.

**Verification:** Index a known corpus (e.g. dump 1000 events from a test room); assert that an exact phrase query returns the expected top hit; redact one of those events and confirm it disappears from results; latency p95 under 200ms on the Riley-shape target.

### 10. Drafts and per-device read state

- Tables: `device_state` keyed by `(account_id, device_id, namespace, key)` with an opaque value blob and `updated_at`.
- Devices are identified by a client-supplied UUID at first registration.
- Endpoints: `GET/PUT /v1/devices/{device_id}/state/{namespace}`.
- Live sync via WS: changes broadcast to other devices owned by the same human; last-write-wins by `updated_at`.

**Verification:** Two `axon-web` tabs (acting as separate devices); typing a draft in one updates the other within a second.

### 11. Web alpha (`axon-web`)

- Vite + React + TypeScript app under `clients/web/`.
- Uses generated API stubs.
- Bundled as a static asset and served by `axon` from the same origin in production. In development, Vite's dev server proxies API calls back to a local `axon`.
- Views:
  - Login (paste-token form).
  - Rooms list, sorted by most-recent activity (rooms from all accounts in one list — the data model supports N accounts; the alpha doesn't expose account-switching controls).
  - Room view: timeline reverse-chronological by default; infinite scroll backwards for history.
  - Composer: send, edit, redact, react.
  - Inline media (images, video, audio) via `/v1/media/…`.
  - Minimal search input that hits `/v1/search`, scoped to active room or all rooms.
- No verification UI.

**Verification:** Start docker-compose Postgres, start `axon`, issue a token, paste it into `axon-web`, exercise the chat loop in a real browser against a Synapse with real messages. Run a search query through the input and confirm a known phrase returns the expected hit. Screenshot the working timeline.

### 12. Self-hosting docs

- `docs/self-hosting.md` covering:
  - Prerequisites (Postgres, Synapse / Dendrite accessible).
  - Build / install (Cargo + Docker options).
  - Config reference (every setting from `axon-core`'s config loader).
  - First-run flow: account provisioning, token minting, accessing `axon-web`.
  - Operational basics: backups (`pg_dump` + media cache directory), upgrades, logs.
  - Cloud deployment recipes — at minimum one each for:
    - Railway (or a similar Procfile-style PaaS).
    - DigitalOcean droplet (Docker Compose + nginx reverse proxy + Let's Encrypt).
    - AWS (EC2 + RDS Postgres; ECS optional; reference Terraform welcome but not required).
    - Bare Linux VPS (the default — covered in the operational basics above).

**Verification:** A reader who has not touched the codebase follows the doc top to bottom on a fresh VM and reaches the "daily-driver in a browser" PRD success criterion. At least one cloud recipe is exercised end-to-end (any of the three) by someone other than the author.

## Open decisions that gate milestones

One genuinely open question carried over from [`tech-spec.md`](./tech-spec.md):

- **Threads in MVP or immediately after.** If threads make MVP, Milestone 4 needs thread-aware `relates_to` indexing (it already captures `m.thread`); Milestone 5 needs thread endpoints (`GET /v1/rooms/{id}/threads`, thread-scoped timeline reads); Milestone 11 needs a "view in thread" affordance. Stop and confirm with the human before pulling threads in — this is a scope expansion, not a free addition.

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
- **API milestones (5, 6, 7, 9, 10).** curl against the running server using the generated TypeScript types as fixtures.
- **Auth milestone (8).** End-to-end: mint, use, revoke, confirm rejection.
- **Search milestone (9).** Index a known corpus, assert top results; measure p95 against the Riley-shape target.
- **Web alpha (11).** Real dev server, real browser, real Synapse; exercise send / edit / redact / react / media / search; screenshot the timeline.
- **Self-hosting (12).** A reader follows the doc on a fresh VM and reaches the daily-driver success criterion in under an hour; at least one cloud recipe is exercised end-to-end.

## Maintaining CLAUDE.md

A `CLAUDE.md` at the repository root captures the living context that future agentic contributors need to ramp quickly: what the crates do, where things live, which conventions are non-obvious, and what's currently being worked on.

- **Create** `CLAUDE.md` during Milestone 1. Initial contents: project name, one-paragraph summary, pointer to `docs/mvp/`, the directory tree from "Project layout" above, and a short conventions section that links here.
- **Update** as you go. After every milestone, append or revise the relevant section. If you make a non-obvious design choice during a milestone — picking a library, deciding a schema detail that isn't in the specs, adding a build step — note it in `CLAUDE.md` so the next agent doesn't have to reverse-engineer it.
- **Keep it short.** `CLAUDE.md` is a high-density orientation, not a wiki. If a section grows past a page, that's a signal to break it out into a dedicated doc under `docs/` and leave a one-liner pointer behind.
- **Treat it as code.** Edits go through the same PR review as code changes.

The goal: any agentic contributor opening this repo cold reads `CLAUDE.md` and is productive within minutes, without having to grep around or re-read the MVP specs.

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
- No advanced search UI (faceted, semantic). Backend ships; a minimal search input in `axon-web` ships; rich UI does not.
- No verification UI in the alpha. The API surface ships; the client doesn't drive it.
- No S3 / object-store media backend. Local disk LRU cache only.
- No spaces-specific endpoints. Events flow through.
- **Threads:** see "Open decisions that gate milestones." Default to "not in MVP" unless the human confirms a scope expansion.
