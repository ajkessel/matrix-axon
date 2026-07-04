# ADR 0051 — First-run config bootstrap (`axon init`)

**Status:** Proposed — targets **Milestone 13** (deployment docs / onboarding; see
`docs/mvp/implementation.md` §13). Depends on the platform config-dir discovery from
ADR 0050 (PR #206).

## Context

Axon is meant to be a "download one binary and run it" self-host (M13, implementation
spec §13). Today the very first run is a cliff: `axon` with no configuration fails while
loading config because `database.url` has no default, and the error is a raw figment
extraction message, not guidance. A new operator has to already know to hand-write an
`axon.toml` (or set `DATABASE_URL` + friends), pick a `store_key`, and mint a bearer
token before any client can connect. The `run.sh` / `run.ps1` launchers paper over this
for the *dev* workflow (they generate a `.env` and start a Docker Postgres), but the
shipped binary — the thing an operator actually installs — has no equivalent.

The implementation spec already sets the target (§ "Config becomes optional past 7a"):
the **minimal boot configuration is just Postgres + `store_key`**; everything
account-shaped is provisioned at runtime through the API (`POST /v1/accounts/login`), and
the first token is minted via the CLI. So a first-run helper does not need to collect a
Matrix account at all — it needs to produce a valid, secure *server* config and get out
of the way.

## Decision

Add an **`axon init`** subcommand that generates a starter config in the platform config
directory, plus a light touch on the bare-`axon` path that points users to it. Design
around one hard guardrail and a clear auto/confirm/runtime split.

### 1. Explicit subcommand, not implicit magic

`axon init` is the primary entry point (the `git init` / `gh auth login` pattern). The
normal server path stays non-interactive and predictable. As sugar, **bare `axon` with no
config resolved *and* an interactive TTY** prints a one-line offer ("No configuration
found. Run `axon init` to create one now? [Y/n]") that simply delegates to the same init
routine; declining, or any non-TTY invocation, falls back to today's behavior. This keeps
the discoverability of a prompt without putting interactivity on the hot server path.

### 2. Hard guardrail: never block a headless boot

All interactivity is gated on `std::io::stdin().is_terminal()`. Under systemd / Docker /
CI there is no TTY, so:

- bare `axon` **never** prompts — it uses env + defaults and, if `database.url` is still
  absent, fails fast with an improved, actionable message (naming `axon init`, the config
  path, and the `DATABASE_URL` / `AXON_DATABASE__URL` env vars);
- `axon init` in a non-TTY context runs **non-interactively** from flags/defaults (or
  errors asking for the missing flag) — usable from provisioning scripts.

A server binary that hangs waiting for keyboard input in a container is a release blocker;
this gate is the invariant the whole feature is built around.

### 3. What init determines — the auto / confirm / runtime split

| Setting | Handling |
|---|---|
| `server.*`, `search.*`, `media.*`, `backfill.*`, timeouts | **Omit** — the built-in defaults already apply; a generated file stays minimal |
| On-disk dirs (`sync`/`search`/`media`) | **Omit** — inherit the ADR 0050 platform defaults |
| `sync.store_key` | **Auto-generate** a CSPRNG 32-byte hex value (the `run.sh` approach), write once |
| `database.url` | **Confirm** — default `postgres://axon:axon@127.0.0.1:5432/axon`, editable at the prompt; **probe** reachability and report ✅/❌ so the operator knows if a DB still needs starting |
| `sync.account.*` | **Skip** — provisioned at runtime via `POST /v1/accounts/login` (spec §"Config becomes optional") |
| First bearer token | **Offer to mint** (reuse `token::issue`) *iff* the DB is reachable, and print it once; otherwise tell the operator to run `axon token issue` after the DB is up |

The net result: a near-zero-config first run. The only value the operator may need to
touch is `database.url`, and only because Postgres is a genuine external dependency.

### 4. The one thing init cannot conjure: Postgres

Postgres is a hard dependency (ADR 0006 / 0042 ruled out embedded SQLite). `axon init`
writes the *config* but does not start or create a database, and cannot `CREATE
DATABASE`/`ROLE` without admin credentials. The binary deliberately stays **decoupled from
Docker** — bringing up the bundled compose Postgres remains `run.sh`'s job. Init's
contribution here is a reachability probe and a clear message, not orchestration.

### 5. Safety / correctness invariants

- **`store_key` is write-once.** It encrypts access tokens at rest and passphrases the SDK
  store; regenerating it orphans all existing encrypted data. Init refuses to overwrite an
  existing config (require `--force` to replace) and never re-mints a key over a live one.
- **The generated file holds a secret** → create it `0600` and create the config directory
  (via ADR 0050's `project_dirs().config_dir()`) if absent. On Windows, rely on the
  user-profile ACLs of `%APPDATA%` (no chmod).
- **Idempotent and explicit.** `axon init` with a config already present is a no-op that
  reports the path (or `--force` to replace). It writes exactly one file and reports where.
- **Reuse, don't fork, the resolver.** Init writes to the same location `Config::resolve_path`
  discovers (ADR 0050), so a freshly-generated file is found on the next run with no flags.

### 6. `run.sh` / `run.ps1` relationship

To avoid two competing config generators, the launchers' bespoke `.env`-templating
(`sed`/`-replace` of `AXON_SYNC__STORE_KEY` + `POSTGRES_*`) should eventually **delegate to
`axon init --non-interactive`** for the axon-side config, keeping only the Docker-Postgres
orchestration that is genuinely theirs. That refactor can follow the init landing; it is
not a prerequisite.

## Consequences

- A non-Rust operator can `install axon` → `axon init` → start Postgres → `axon`, then add
  their Matrix account from a client — no hand-edited TOML, no guessing at `store_key`.
- The server hot path and all headless deployments are unchanged (TTY gate); the only new
  always-on behavior is a friendlier fail-fast message when `database.url` is missing.
- One more secret-bearing file on disk (`0600` config with `store_key`), consistent with
  how `run.sh` already writes `store_key` into `.env`.
- Feeds M13's `self-hosting.md`: the "localhost / first-run path" recipe becomes "run
  `axon init`," and the config reference documents what init omits (everything that has a
  default) versus what it sets.

## Implementation plan (for the M13 PR — `crates/` silo)

1. **`crates/axon-server/src/cli.rs`** — add an `Init` variant to `Command` with flags:
   `--database-url <URL>`, `--force`, `--non-interactive` (and `--print-token` / `--no-token`
   to control minting). Global `--config <PATH>` (ADR 0050) already sets the write target.
2. **`crates/axon-server/src/init.rs`** (new) — the routine:
   - resolve the target path (CLI `--config` → else `project_dirs().config_dir()/axon.toml`);
   - refuse if it exists without `--force`;
   - TTY-gate prompts (`IsTerminal`); in non-interactive mode take everything from flags/defaults;
   - generate `store_key` (CSPRNG; reuse the same primitive `run.sh` documents);
   - render a minimal TOML (`[database].url` + `[sync].store_key`, everything else omitted) —
     ideally via `toml_edit` (already a workspace dep) so comments can be embedded;
   - write with `0600` perms (Unix) and create parent dirs;
   - probe `database.url` (a bounded `Store::connect` attempt) and, if reachable and
     requested, mint + print a token via the existing `store.issue_token` path.
3. **`crates/axon-server/src/main.rs`** — route `Some(Command::Init …)`; on the `None`
   (server) branch, when config resolution found no file and `stdin().is_terminal()`, offer
   to run init before the existing `serve`. Improve the missing-`database.url` error text.
4. **`crates/axon-core`** — optionally expose a small helper for the "was a config file
   actually loaded?" signal so `main` can distinguish "no file" from "file present but
   incomplete" (today `resolve_path` is private; a public "resolved path, if any" accessor
   keeps init and the server in agreement without duplicating logic).
5. **Docs** — update `axon.toml.example` to mention `axon init` as the easy path; fold the
   first-run steps into `docs/self-hosting.md` (the M13 deliverable) and reference this ADR.

### Verification (for the M13 PR)

- Non-TTY invariant: `axon </dev/null` with no config never blocks — it fails fast (or runs
  from env) — asserted in a test that closes stdin.
- `axon init --non-interactive --database-url … --config <tmp>` writes a `0600` file
  containing a fresh `store_key`, discoverable by a subsequent `Config::load_default`.
- With a reachable dev Postgres, `axon init` (or `--print-token`) mints a working token that
  authenticates against `/v1/`; with no DB, it writes the config and prints the deferred
  `axon token issue` guidance instead of failing.
- `--force` replaces; without it, an existing config is a reported no-op (no `store_key`
  churn).
