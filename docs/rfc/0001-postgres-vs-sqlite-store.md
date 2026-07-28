# RFC 0001 - Archive store backend: keep Postgres for MVP, reassess SQLite only as a full switch

## Status

Proposed.

## Context

Axon's server-side archive store has depended on Postgres since the start of the
project (ADR 0003). We did not make that choice after an extended Postgres vs.
SQLite design discussion; it was the straightforward fit for a Rust server with a
durable event archive, JSON-heavy Matrix payloads, migrations, and a future path
to hosted or larger deployments.

The product shape now makes the tradeoff worth recording explicitly. Axon is
primarily a self-hosted personal agent: one human per process, with one or more
Matrix accounts inside. For that model, requiring Docker or a separate Postgres
service is a real adoption cost. SQLite would make the default path closer to a
single binary plus a local data directory.

At the same time, we do **not** want to support two archive-store backends. Two
SQL dialects would double the correctness surface for the parts of Axon where the
database is most load-bearing: lifecycle state, message history, relation
aggregation, media lookup, search convergence, and account-token storage.

## Decision

Keep Postgres as the archive store for the near-term MVP. Do not add a second
SQLite backend.

If "no Docker / no separate database server" becomes a hard pre-MVP product
requirement, make SQLite a **complete replacement** for Postgres in a dedicated
storage milestone, before adding more store-heavy features. That switch must
rewrite the store, migrations, tests, launcher scripts, and token-encryption
mechanism as one coherent change; it should not be introduced as a compatibility
layer.

The recommendation is therefore:

- **Default path:** keep Postgres through MVP and reduce setup friction with
  packaging, launchers, documentation, and possibly a bundled/local Postgres
  option.
- **Alternative path:** switch fully to SQLite only if the product requirement is
  "download Axon, run Axon, no external DB process." If we choose that, do it
  soon and stop building new Postgres-specific store features first.
- **Rejected path:** do not support both Postgres and SQLite.

## Current Postgres coupling

The current code is not database-neutral. `axon-store` owns a `PgPool`, all row
decoding is typed to `PgRow` / `Postgres`, and `axon-server` and the token CLI
connect through `Store::connect(&config.database.url, ...)`. The configuration
surface documents `database.url` as a required Postgres URL, and the local
developer path centers on `docker-compose.yml`, `.env`, and `DATABASE_URL`.

The schema and query layer depend on Postgres behavior in several important
places:

- **Types and migrations.** Migrations use `UUID`, `JSONB`, `TIMESTAMPTZ`,
  `BIGSERIAL`, `BYTEA`, Postgres expression indexes, partial indexes, and
  plpgsql triggers for `updated_at`.
- **Access-token encryption.** ADR 0008 stores Matrix access tokens with
  `pgcrypto` (`pgp_sym_encrypt` / `pgp_sym_decrypt`) and enables the extension in
  the baseline migration. Bearer tokens are already hashed in Rust, but Matrix
  access tokens still rely on pgcrypto.
- **Relation aggregation.** ADR 0033's resolved timeline projection uses
  Postgres JSONB operators, `LEFT JOIN LATERAL`, `DISTINCT ON`, `array_agg`,
  `bool_or`, aggregate `FILTER`, `IS DISTINCT FROM`, and `jsonb_*` constructors.
  This is the hot path for edit collapse, reaction tallies, redaction masking,
  replies, and thread reads.
- **Search convergence.** ADR 0039's transactional `search_outbox` writes append
  indexing obligations in the same statement as event mutations using writable
  CTEs with `RETURNING`. That is central to the "persisted event and indexing
  obligation commit atomically" guarantee.
- **Media lookup.** The media proxy uses partial expression indexes over JSONB
  paths such as `content->'info'->'thumbnail_file'->>'url'`.
- **Test and operational surface.** Ignored store/API/sync/search integration
  tests expect Postgres and `DATABASE_URL`; integration scripts use `psql` for
  setup and assertions.
- **Dependency graph.** ADR 0006 intentionally avoided `sqlx-sqlite` because it
  conflicted with the Matrix SDK's `rusqlite` / `libsqlite3-sys` version at the
  time. The current lockfile has the SDK path on `rusqlite` and
  `libsqlite3-sys`; a SQLite switch must re-check this dependency story rather
  than assume `sqlx-sqlite` can be enabled trivially.

None of these are impossible to port. Together, they make switching a storage
milestone, not a config change.

## Postgres vs. SQLite

### Postgres advantages

Postgres fits Axon's current implementation and future headroom:

- It already matches the implemented schema and query strategy.
- MVCC gives a strong default concurrency model for simultaneous sync ingest,
  API reads, lifecycle writes, token verification writes, and search indexing.
- JSONB and expression/partial indexes are mature and already used in the hot
  paths.
- pgcrypto covers the existing access-token-at-rest design.
- Hosted or heavier deployments have a straightforward path to external managed
  Postgres.
- Existing smoke/integration scripts and DB-gated tests already exercise the
  real deployed shape.

The cost is operational: users must run a DB service, manage credentials, avoid
port conflicts, back it up, and understand Docker or local Postgres.

### SQLite advantages

SQLite is attractive for Axon's personal, single-human deployment model:

- It removes the biggest first-run dependency: no Docker, no local Postgres
  daemon, no DB credentials, no open port.
- A single DB file is easy to copy, back up, inspect, and move with the Axon data
  directory.
- WAL mode supports concurrent readers with a writer and is often fast enough
  for local single-process applications.
- SQLite has useful modern primitives: JSON functions/operators, expression
  indexes, partial indexes, `UPSERT`, `RETURNING`, and `STRICT` tables.
- Axon already uses SQLite indirectly through the Matrix SDK's per-account store,
  so the deployment would no longer mix "Postgres archive plus SQLite SDK store";
  it would be SQLite-backed storage throughout, though still in separate files.

The cost is architectural: SQLite has one writer at a time in WAL mode, no
standard pgcrypto equivalent, different JSON/type semantics, and a different SQL
dialect for the exact queries Axon currently relies on.

## Switching cost

Switching now would be **high but bounded**. It is still cheaper now than after
history backfill, more search API work, or additional materialized read models
land, but it is not a small refactor.

A credible SQLite replacement must include:

- A new store driver choice (`rusqlite` to align with the Matrix SDK, or a
  proven `sqlx-sqlite` dependency plan that resolves cleanly with the SDK).
- SQLite migrations for every current table and index, including explicit
  `PRAGMA foreign_keys = ON`, WAL configuration, busy timeouts, and a checkpoint
  policy.
- A replacement for pgcrypto. The likely answer is application-layer
  authenticated encryption in Rust, keyed by `sync.store_key`, with a key/version
  envelope that preserves the later rotation path ADR 0008 already calls out.
- Rewrites of relation aggregation SQL. `DISTINCT ON`, `LATERAL`, Postgres arrays,
  JSONB constructors, and `FILTER`-based JSON aggregation need SQLite equivalents
  or staged Rust-side assembly. The result must keep ADR 0033's semantics exactly.
- Rewrites of the search outbox write path. SQLite supports top-level
  `RETURNING`, but its docs do not allow DML-with-`RETURNING` to be used as a
  table expression in the same way Axon's Postgres writable CTEs are used today.
  The atomic "mutation plus outbox obligation" guarantee must be preserved with
  explicit transactions or triggers.
- Rewrites of media URL lookup indexes over JSON paths.
- Type mapping and stricter validation for UUIDs, timestamps, JSON, booleans, and
  binary ciphertext now represented as SQLite storage classes.
- Updated config, `.env`, launchers, integration scripts, docs, and test helpers
  so the default data path is file-based rather than `DATABASE_URL`.
- Performance validation under realistic workloads: live sync ingest, timeline
  paging, relation aggregation, UTD re-decryption, search seeding/draining, token
  verification, account deletion, and backup/checkpoint behavior.

The safest implementation shape would be a clean branch that replaces the
Postgres store wholesale, ports the existing DB-gated tests to temp SQLite files,
and keeps the public `/v1/` API unchanged.

## Security and stability notes

SQLite does not weaken Axon by definition, but it moves several responsibilities
from the database/service boundary into the application:

- Access-token encryption must be owned by Axon code or by a carefully chosen
  SQLite encryption extension. A plain SQLite file containing plaintext Matrix
  access tokens is not acceptable.
- Filesystem encryption remains the story for message bodies and the Tantivy
  index, just as it is today for Postgres data files and the search index.
- WAL creates `-wal` and `-shm` sidecar files; backup documentation must account
  for them or use the SQLite backup API.
- The one-writer model needs explicit bounded behavior. Long writes, reindexing,
  account deletion, or re-decryption must not cause user-visible reads and token
  checks to fail under normal personal-scale use.
- SQLite over network filesystems is not a good deployment target; the data
  directory should be local disk.

## Consequences

- Postgres remains the source of truth for MVP planning and implementation.
- New store-heavy work may continue using Postgres-specific SQL, but reviewers
  should remember that this increases any future SQLite switching cost.
- If onboarding friction becomes the deciding product issue, the project should
  pause new store features and schedule a storage-backend replacement milestone
  rather than accumulating more Postgres-only behavior.
- The project should prefer packaging improvements before a database switch if
  the product requirement is only "make setup easier" rather than "remove the
  external database process entirely."

## References

- ADR 0003 - Postgres access via sqlx with embedded migrations.
- ADR 0006 - sqlx direct driver crates and the `sqlx-sqlite` dependency issue.
- ADR 0008 - access tokens encrypted at rest via pgcrypto.
- ADR 0033 - relation aggregation in the store.
- ADR 0039 - full-text search and the transactional `search_outbox`.
- PostgreSQL MVCC documentation:
  <https://www.postgresql.org/docs/current/mvcc-intro.html>
- PostgreSQL pgcrypto documentation:
  <https://www.postgresql.org/docs/current/pgcrypto.html>
- SQLite WAL documentation:
  <https://www.sqlite.org/wal.html>
- SQLite JSON documentation:
  <https://www.sqlite.org/json1.html>
- SQLite `RETURNING` documentation:
  <https://www.sqlite.org/lang_returning.html>
- SQLite expression and partial index documentation:
  <https://www.sqlite.org/expridx.html>,
  <https://www.sqlite.org/partialindex.html>
- SQLite strict tables documentation:
  <https://www.sqlite.org/stricttables.html>
