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

## Clients

| Client | Description |
| --- | --- |
| [`axon-tui`](clients/tui/README.md) | Terminal-based client designed to support all functionality exposed by Axon. |

## Developer quick-start

Prerequisites: Rust (stable). Docker is only needed if you don't have a local Postgres instance.

Once prerequisites are installed, the quickest path is:

```bash
./run.sh          # macOS / Linux / WSL  — starts axon-server (default)
./run.sh tui      # starts axon-tui instead
./run.sh clean    # destroys Postgres data volume and exits (no rebuild)
.\run.ps1         # Windows (PowerShell) — starts axon-server (default)
.\run.ps1 tui     # axon-tui
.\run.ps1 clean   # destroys Postgres data volume and exits (no rebuild)
```

The run script handles the rest automatically: validates that required
environment variables are set (offering to create `.env` from `.env.example`
if neither exists), runs the chosen target, and tears down any containers it
started on exit — whether by Ctrl-C, SIGTERM, or any other cause.

**Postgres:** if a Postgres instance is already reachable at
`POSTGRES_HOST:POSTGRES_PORT` (defaulting to `127.0.0.1:5432`) when the
script starts, it uses that directly and Docker is not required at all.
Otherwise it starts Postgres via Docker Compose automatically.

The steps below explain what the run scripts do and how to configure the pieces
individually.

### 1. Install Prerequisites

#### Ubuntu

This should work on a native Linux box or in a WSL environment on Windows.

```bash
sudo apt install docker.io docker-compose-v2
sudo snap install --classic rustup
```
#### macOS

If you don't yet have Homebrew, Rust, or Docker, these commmands will install all three:

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
brew install rust
brew install --cask docker
```

You likely need to start Docker from the MacOS desktop the first time and grant it administrative privileges to run.

#### Windows (PowerShell)

> WSL2 users should follow the Ubuntu path above instead.

Install Rust and Docker Desktop via [winget](https://learn.microsoft.com/en-us/windows/package-manager/winget/):

```powershell
winget install Rustlang.Rustup
winget install Docker.DockerDesktop
```

You likely need to start Docker Desktop from the Start menu the first time and grant it administrative privileges to run.

PowerShell restricts running local scripts by default. Allow it for your user account once:

```powershell
Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser
```

### 2. Install and Start Postgres

Run these commands from the top-level matrix-axon directory.

**With Docker (easiest, optional):**
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

### 3. Configure

```bash
cp .env.example .env
```

The server loads `.env` automatically on startup. The defaults in `.env.example` match the docker-compose settings; adjust `DATABASE_URL` if your Postgres is configured differently.

> **Local Postgres detected automatically.** If you already have Postgres running on `127.0.0.1:5432` (Homebrew, Postgres.app, a system package, etc.), `run.sh`/`run.ps1` will detect it and skip Docker entirely. Just make sure the role and database exist (see the "Without Docker" step above) and that `DATABASE_URL` in your `.env` points to it. To use a different host or port, set `POSTGRES_HOST` and `POSTGRES_PORT` in `.env`.
>
> **macOS + Docker note:** `localhost` can resolve to IPv6 (`::1`) on macOS, but Docker only binds to IPv4. The examples use `127.0.0.1` explicitly to avoid this.

### 4. Build and run

```bash
# Enable the git pre-commit hook (fmt + clippy) — once per clone
./scripts/setup-hooks.sh

# Quick path — auto-detects local Postgres or starts one via Docker, tears down on exit:
./run.sh          # macOS / Linux / WSL  — axon-server (default)
./run.sh tui      # axon-tui
./run.sh clean    # destroys Postgres data volume and exits (no rebuild)
.\run.ps1         # Windows (PowerShell) — axon-server (default)
.\run.ps1 tui     # axon-tui
.\run.ps1 clean   # destroys Postgres data volume and exits (no rebuild)

# Or run directly if Postgres is already up:
cargo run -p axon-server
cargo run -p axon-tui
```

In another shell:
```bash
curl localhost:8080/healthz     # -> {"status":"ok"}
```

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` on every push. The pre-commit hook in `.githooks/` runs the fmt + clippy subset locally (enable with `./scripts/setup-hooks.sh`); bypass a single commit with `git commit --no-verify`.

### 5. Start over

If you want to restart with a fresh instance and fresh data, just destroy and restart the postgres Docker instance per below.

```bash
docker compose down -v postgres
docker compose up -d postgres
```

### 6. Troubleshooting

During very early development, there may be some breaking updates. If you get an error like `Error: connecting to database` after `cargo run -p axon-server`, try starting a fresh postgres docker instance per the instructions directly above.

## Docs

|                                                   |                                                  |
| ------------------------------------------------- | ------------------------------------------------ |
| [PRD](docs/mvp/prd.md)                            | What we're building and why                      |
| [Tech spec](docs/mvp/tech-spec.md)                | Architecture decisions                           |
| [Implementation spec](docs/mvp/implementation.md) | Milestone-by-milestone build plan                |
| [AGENTS.md](AGENTS.md)                            | Orientation for contributors (human and agentic) |
| [ADRs](docs/adr/)                                 | Decisions made during implementation             |

## Deployment

### Authentication

All `/v1/` API endpoints require a bearer token. Mint one after startup:

```bash
axon token issue --label my-client   # prints the raw token once
axon token list                       # list tokens (never shows secrets)
axon token revoke <id>                # revoke a token
```

Tokens are instance-scoped — one token grants access to all accounts on that Axon instance. Supply the token to clients via their config file or environment; see [`clients/tui/README.md`](clients/tui/README.md) for the TUI.

### TLS

Axon serves plain HTTP. For any non-local deployment, place a TLS-terminating reverse proxy (Caddy, nginx, etc.) in front of it and keep Axon bound to loopback (the default). Axon refuses to start on a non-loopback address over plain HTTP unless `AXON_SERVER__ALLOW_INSECURE_BIND=true` is explicitly set.

## Status

MVP in progress — **Milestone 2 of 12 complete** (config loader, Postgres pool + migrations, `/healthz`).

Next: Milestone 3 — sync engine v0 (`accounts` table, matrix-rust-sdk client per account, Simplified Sliding Sync).
