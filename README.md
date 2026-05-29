# Axon

Axon is a self-hosted personal agent for [Matrix](https://matrix.org). It sits between your homeserver(s) and your clients, providing the persistent state, full-text search index, and per-device coherence that Matrix clients otherwise have to reinvent on every install.

Matrix's encrypted and decentralized architecture can make full client usability challenging. This "middle" layer aims to solve that challenge. It is similar to the [back-end for front-end](https://philcalcado.com/2015/09/18/the_back_end_for_front_end_pattern_bff.html) concept, with the added wrinkle that it is intended to run as a separate instance per user. Old-timers may find a familiar with analogy with [ZNC Bouncer](https://en.wikipedia.org/wiki/ZNC), an agent that sits between an IRC client and an IRC server.

See [`docs/mvp/prd.md`](docs/mvp/prd.md) for the full product description and [`docs/mvp/tech-spec.md`](docs/mvp/tech-spec.md) for the architecture.

## Architecture overview

```
Homeserver(s)  →  Axon (single binary)  →  axon-web (alpha client)
(Synapse /         sync · crypto · store      + any client built
 Dendrite)         search · media · api         against /v1/ API
```

One Rust binary, one Postgres database, media cached to local disk. See the [architecture diagram](docs/mvp/tech-spec.md#architecture-overview) for detail.

## Developer quick-start

Prerequisites: Rust (stable), Docker, Docker Compose.

```bash
# Start Postgres
docker compose up -d postgres

# Build everything
cargo build

# Run the server (stub — returns nothing useful until Milestone 2)
cargo run -p axon-server
```

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` on every push.

## Docs

|                                                   |                                                  |
| ------------------------------------------------- | ------------------------------------------------ |
| [PRD](docs/mvp/prd.md)                            | What we're building and why                      |
| [Tech spec](docs/mvp/tech-spec.md)                | Architecture decisions                           |
| [Implementation spec](docs/mvp/implementation.md) | Milestone-by-milestone build plan                |
| [AGENTS.md](AGENTS.md)                            | Orientation for contributors (human and agentic) |
| [ADRs](docs/adr/)                                 | Decisions made during implementation             |

## Status

MVP in progress — **Milestone 1 of 12 complete** (workspace scaffolding).

Next: Milestone 2 — config loader, Postgres pool, `/healthz`.
