# ADR 0004 — Timestamp-prefixed migration filenames

## Context

`docs/mvp/implementation.md` specifies migrations under
`crates/axon-store/migrations/` with a "numeric prefix" and sqlx's migrate
runner. The natural reading is sequential numbering (`0001_`, `0002_`, …).

sqlx parses the filename prefix into an `i64` version, records applied versions
in `_sqlx_migrations`, and runs pending ones in ascending version order — so the
*ordering* mechanism is fixed by sqlx; only the prefix *scheme* is ours to choose.

Sequential numbers collide under concurrent development: two branches that each
add `0005_…` produce duplicate versions and a merge conflict. We expect multiple
contributors (human and agentic) opening migrations in parallel.

## Decision

Use **UTC timestamp prefixes** (`YYYYMMDDHHMMSS_description.sql`), the format
`sqlx migrate add` generates by default — e.g. `20260529213622_baseline.sql`.

- Monotonic and effectively collision-free across branches.
- No prefix-width ceiling.
- Generated for you by `sqlx migrate add <description>`.

This is a deliberate refinement of the frozen spec's "numeric prefix" wording (a
timestamp is still numeric); recorded here rather than editing `docs/mvp/`.

## Consequences

- Create new migrations with `sqlx migrate add <description>` (needs
  `cargo install sqlx-cli`), or hand-name them with a UTC timestamp prefix.
- `ls` ordering stays correct (timestamps sort lexicographically == numerically).
- Migrations remain forward-only (sqlx has no down-migrations); undo by writing a
  new migration.
- The baseline migration was renamed from `0001_baseline.sql` to its timestamp
  form. Any database that applied the old `0001` must reset its volume
  (`docker compose down -v`) — cheap at this stage since the only migration
  enables an extension and no application tables or data exist yet.
