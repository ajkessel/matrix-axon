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
    web/                     # axon-web (Vite + React + TS)
  openapi/                   # spec source of truth (handwritten + utoipa-emitted)
  docs/
    mvp/                     # this directory
    adr/                     # architecture decision records
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
- **Verification plumbing (build here, exercised in M5).** The programmatic SDK flow — surface a `VerificationRequest`, `accept()`, read `sas.emoji()` / `sas.decimals()`, `confirm()` / `cancel()`. "Headless" means `axon` has no UI of its own, not that it can't verify; the SDK API is fully programmatic. The user-facing emoji exchange can't be exercised until the M5 WebSocket exists, so this milestone builds the plumbing and M5 wires the UX. Note `axon-crypto` is the "thin verification surface over rust-sdk crypto" from the project layout.

**Verification (E2EE):** against a real homeserver with key backup enabled, supply `sync.account.recovery_key`, confirm UTD rows flip to `decrypted = true` as backed-up keys arrive, and that `axon` shows as a verified/cross-signed device.

### 5. Client API v0

- axum routes under `/v1/`:
  - `GET /v1/rooms` (list rooms across all accounts; filterable by `account_id`; sorted by most-recent activity).
  - `GET /v1/rooms/{room_id}/timeline` (paginated, reverse-chronological by default).
  - `GET /v1/events/{event_id}`.
- WebSocket at `/v1/ws`. Envelope: `{type, account_id, payload}`. Live timeline events fan out from the sync engine.
- OpenAPI spec emitted via utoipa; written in parallel.
- TypeScript stubs generated into `clients/web/src/api/`.
- Define a shared `ApiResponse<T>` / `ApiError` envelope type in `axon-api` and a
  custom `IntoResponse` impl so all handlers return consistent JSON shapes and error
  bodies. (Deferred from M2; designing against zero real handlers is premature.)

**Verification:** Boot the server, `curl /v1/rooms`, hit `/v1/rooms/{id}/timeline`, open a websocat session to `/v1/ws` and see live events arrive tagged with `account_id` as new events come in over sync.

**Interactive verification UX over the WebSocket (consumes the M4 plumbing — ADR 0011).** Wire the verification flow built in Milestone 4 to the client: `axon` relays incoming/outgoing `VerificationRequest`s and streams the SAS emoji/decimals over `/v1/ws`, so the user verifies the `axon` session *from the axon client*, exactly as they would verify any other device. This is the mature E2EE key-acquisition path: once `axon` is interactively verified, the user's other devices **gossip** the cross-signing secrets and key-backup key to it automatically — so the recovery key never has to be stored server-side. (This is the BFF answer to "no verification UI in the alpha" — the API surface ships here; `axon-web` driving it is a later/optional add. QR-code verification is a follow-up.)

**Verification (interactive E2EE):** from a trusted Element session, start verification of the `axon` device; confirm the SAS emoji arrive over `/v1/ws`, that confirming both sides cross-signs `axon`, and that subsequently-sent encrypted messages decrypt without the recovery key.

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
- Schema fields: `event_id`, `account_id` (facet), `room_id` (facet), `sender` (facet), `origin_ts` (date), `body` (text).
- `body` analyzer chain: default tokenizer + `LowerCaser` + `AsciiFoldingFilter` + `Stemmer` (English). All built-in Tantivy token filters — register the analyzer once and reference it from the field schema.
- Populate on event ingestion in the sync pipeline.
- `GET /v1/search?q=…&account_id=…&room_id=…&sender=…&from=…&to=…`.
- BM25 ranking; paginated.
- No fuzzy/typo, synonym, or semantic search in MVP (see tech-spec search section). If a bounded fuzzy mode is wanted later, it's a query-time `FuzzyTermQuery` toggle on this endpoint, not an analyzer change.

**Verification:** Index a known corpus (e.g. dump 1000 events from a test room); assert that an exact phrase query returns the expected top hit; confirm case- and diacritic-insensitivity (`cafe` matches `café`) and plural matching (`cat` matches `cats`); latency p95 under 200ms on the Riley-shape target. (Redacted-event behavior is an open question — see tech-spec; don't bake an assertion in here yet.)

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
  - Deployment recipes — at minimum one each for:
    - Home machine behind a private mesh VPN (the recommended self-host path). axon + Postgres on hardware you own — the box under your desk, a home server, a NAS — reached from your other devices over a private network such as Tailscale, with **no port ever exposed to the public internet**. This best fits axon's premise: your data stays on your hardware. It also pairs with the Milestone 8 token auth as defense-in-depth — the VPN is the network gate, the token is the application gate (and the gate that makes "no app auth yet" safe in earlier milestones).
    - Railway (or a similar Procfile-style PaaS).
    - DigitalOcean droplet (Docker Compose + nginx reverse proxy + Let's Encrypt).
    - AWS (EC2 + RDS Postgres; ECS optional; reference Terraform welcome but not required).
    - Bare Linux VPS (covered in the operational basics above).

**Verification:** A reader who has not touched the codebase follows the doc top to bottom on a fresh VM and reaches the "daily-driver in a browser" PRD success criterion. At least one deployment recipe is exercised end-to-end (any of them) by someone other than the author.

### 13. Threads (post-MVP)

Deferred out of the MVP and handled as a self-contained milestone after the web alpha ships. The store already captures `m.relates_to` generically — including `m.thread` — in `events.relates_to` (ADR 0015), so this milestone is **additive and backfill-free**: the thread membership of every already-stored event is recoverable from data on disk, with no re-sync or re-parse. The deferral cost is a future index + endpoints, not a re-architecture.

- Migration: a thread lookup over `events.relates_to` — an expression/partial index (or a generated column) keyed on the thread root, e.g. over `relates_to->>'event_id' WHERE relates_to->>'rel_type' = 'm.thread'`. Applies retroactively to existing rows.
- `axon-store` reads: list a room's threads (root event + latest reply + reply count) and a thread-scoped timeline read (reuse the cursor pagination from the timeline read, scoped to a thread root).
- Endpoints: `GET /v1/rooms/{room_id}/threads` and a thread-scoped timeline (e.g. `GET /v1/rooms/{room_id}/threads/{root_id}/timeline`).
- Mutations: thread-aware send (set `m.relates_to` with `rel_type: m.thread`) on the existing send path (Milestone 6).
- `axon-web`: a "view in thread" affordance and a thread panel.

**Verification:** In a room with a threaded conversation, `GET …/threads` lists the thread with the correct reply count; the thread-scoped timeline returns only that thread's events, reverse-chronological with stable pagination; a reply sent into the thread round-trips and appears under the right root. Confirm the thread index resolves over events stored *before* this milestone (proving the backfill-free claim).

## Open decisions that gate milestones

The one genuinely open question carried over from [`tech-spec.md`](./tech-spec.md) is now resolved:

- **Threads — resolved: deferred to a dedicated post-MVP Milestone 13.** Rather than thread the feature through Milestone 4 (indexing), Milestone 5 (endpoints), and Milestone 11 (UI), it is handled as one self-contained milestone after the MVP ships. The MVP store deliberately captures `m.relates_to` generically (incl. `m.thread`) in `events.relates_to` (ADR 0015), so the deferral is forward-compatible — Milestone 13 is additive and backfill-free, not a re-architecture. See Milestone 13.

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
- No advanced search UI (faceted, semantic). Backend ships; a minimal search input in `axon-web` ships; rich UI does not.
- No verification UI in the alpha. The API surface ships; the client doesn't drive it.
- No S3 / object-store media backend. Local disk LRU cache only.
- No spaces-specific endpoints. Events flow through.
- **Threads:** see "Open decisions that gate milestones." Default to "not in MVP" unless the human confirms a scope expansion.
