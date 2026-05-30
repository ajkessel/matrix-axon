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

Prerequisites: Rust (stable), Postgres 16.

### 1. Start Postgres

**With Docker (easiest):**
```bash
docker compose up -d postgres
```

**Without Docker** — create the role and database in your local Postgres instance:
```bash
psql postgres <<SQL
CREATE ROLE axon LOGIN PASSWORD 'axon';
CREATE DATABASE axon OWNER axon;
SQL
```

### 2. Configure

```bash
cp .env.example .env
```

The server loads `.env` automatically on startup. The defaults in `.env.example` match the docker-compose settings; adjust `DATABASE_URL` if your Postgres is configured differently.

> **Port already in use?** If you have a local Postgres (Homebrew, Postgres.app) on 5432, the docker-compose container can't claim the port and connections hit the local instance instead — you'll see `role "axon" does not exist`. Set `POSTGRES_PORT` to a free port in `.env` (e.g. `5433`), update the port in `DATABASE_URL` to match, then `docker compose up -d postgres`.
>
> **macOS + Docker note:** `localhost` can resolve to IPv6 (`::1`) on macOS, but Docker only binds to IPv4. The examples use `127.0.0.1` explicitly to avoid this.

### 3. Build and run

```bash
# Enable the git pre-commit hook (fmt + clippy) — once per clone
./scripts/setup-hooks.sh

cargo run -p axon-server
```

In another shell:
```bash
curl localhost:8080/healthz     # -> {"status":"ok"}
```

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` on every push. The pre-commit hook in `.githooks/` runs the fmt + clippy subset locally (enable with `./scripts/setup-hooks.sh`); bypass a single commit with `git commit --no-verify`.

## Docs

|                                                   |                                                  |
| ------------------------------------------------- | ------------------------------------------------ |
| [PRD](docs/mvp/prd.md)                            | What we're building and why                      |
| [Tech spec](docs/mvp/tech-spec.md)                | Architecture decisions                           |
| [Implementation spec](docs/mvp/implementation.md) | Milestone-by-milestone build plan                |
| [AGENTS.md](AGENTS.md)                            | Orientation for contributors (human and agentic) |
| [ADRs](docs/adr/)                                 | Decisions made during implementation             |

## Status

MVP in progress — **Milestone 2 of 12 complete** (config loader, Postgres pool + migrations, `/healthz`).

Next: Milestone 3 — sync engine v0 (`accounts` table, matrix-rust-sdk client per account, Simplified Sliding Sync).
