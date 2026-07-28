# ADR 0002 — Configuration library: figment

## Context

Milestone 2 needs a configuration loader in `axon-core` that reads a TOML file
and layers environment-variable overrides on top, deserialising into a typed
struct. The implementation spec named two candidates: `figment` and `config-rs`.

## Decision

Use **figment** (`figment = { features = ["toml", "env"] }`).

Sources are merged in precedence order (lowest first): struct defaults → TOML
file (if found) → the bare `DATABASE_URL` env var (mapped onto `database.url`)
→ `AXON_`-prefixed env vars with `__` denoting nesting (`AXON_SERVER__PORT`).

Rationale:

- Explicit provider/merge model makes layer precedence obvious and easy to extend.
- Built-in `Jail` test helper lets us unit-test config loading with faked env
  vars and files, so config tests need no database and no real filesystem state.
- Battle-tested as the configuration engine behind Rocket; small, synchronous.
- Mapping the un-prefixed `DATABASE_URL` onto `database.url` keeps the existing
  `.env.example` and sqlx CLI tooling working unchanged.

`config-rs` would also satisfy the requirement but has fiddlier
environment-variable / nested-key handling and less precise errors.

## Consequences

- `axon-core` depends on `figment`; the `test` feature is a dev-dependency only.
- The on-disk format is plain TOML (`axon.toml`), independent of this choice, so
  the library could be swapped later without changing operator-facing config.
- `figment::Error` is large, so `ConfigError::Figment` boxes it to keep
  `Result` types small (satisfies clippy's `result_large_err`).
