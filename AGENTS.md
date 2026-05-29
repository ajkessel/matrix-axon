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
- **Errors:** `thiserror` in libraries; `anyhow` only at the `axon-server` binary boundary.
- **Logging:** `tracing` with structured fields — always include `account_id`, `room_id`, `event_id` where applicable.
- **OpenAPI:** the spec is the source of truth. Handler types must compile against it (utoipa). Drift between spec and generated stubs is a bug.
- **What not to build:** no push (APNs/FCM), no admin API, no multi-human-per-process, no federation, no S3 media backend, no OAuth server — see `docs/mvp/implementation.md` "What not to build" for the full list.

Full conventions are in `docs/mvp/implementation.md` under "Conventions."

## Current state

**Milestone 1 complete** — workspace scaffolded, all crate stubs created, CI configured, docs seeded.

Next: **Milestone 2** — config loader in `axon-core`, sqlx pool + migrations in `axon-store`, axum server with `/healthz` in `axon-server`.
