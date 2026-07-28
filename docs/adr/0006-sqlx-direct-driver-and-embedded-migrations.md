# ADR 0006 — sqlx: direct driver crates, runtime queries, hand-embedded migrations

## Context

Milestone 3 adds `matrix-sdk` to the workspace. Its persistent store,
`matrix-sdk-sqlite`, depends on `rusqlite 0.37 → libsqlite3-sys 0.35`. The
`sqlx` umbrella crate declares `sqlx-sqlite` as an (optional) dependency, and
`sqlx-sqlite 0.8.x → libsqlite3-sys 0.28–0.30`.

`libsqlite3-sys` sets `links = "sqlite3"`. Cargo permits only **one** version of
a `links` crate anywhere in the resolved graph — and this check covers even
*feature-inactive* optional dependencies, because a single `Cargo.lock` must be
valid for any feature selection. The two `libsqlite3-sys` majors cannot coexist,
and they cannot be aligned: no `sqlx-sqlite` release uses `0.35`, and
`rusqlite 0.37` is pinned transitively by `matrix-sdk-sqlite 0.17`. Result: as
long as the `sqlx` umbrella is in the graph, the workspace does not resolve.

## Decision

1. **Drop the `sqlx` umbrella; depend on `sqlx-core` + `sqlx-postgres`
   directly.** This removes `sqlx-sqlite` (and its `libsqlite3-sys`) from the
   resolution graph entirely, leaving only matrix-sdk's `libsqlite3-sys 0.35`.

2. **Use the runtime `query` / `query_as` API, not the `query!` macros.** The
   compile-time-checked macros live in the umbrella and require a database at
   build time. Runtime queries keep CI database-free (consistent with the M2
   stance) and are unaffected by dropping the umbrella. `FromRow` is implemented
   by hand for the same reason (the derive is an umbrella macro).

3. **Embed migrations with `include_dir` and build a `Migrator` by hand.** The
   `sqlx::migrate!` macro is umbrella-only. We embed the `migrations/` directory
   at compile time and construct a `sqlx_core::migrate::Migrator`, using the same
   filename convention (`<version>_<description>.sql`) and SHA-384 SQL checksum
   as sqlx, so the `_sqlx_migrations` bookkeeping table is byte-for-byte what the
   macro would have produced. Compile-time embedding is preserved — a deployed
   binary still needs no migration files on disk.

4. **Pin `sqlx-core` / `sqlx-postgres` to `=0.8.2`.** Going direct to
   `sqlx-core` requires enabling its `_rt-tokio` and `_tls-rustls-aws-lc-rs`
   features. The `_` prefix marks these as **semver-exempt internal features** —
   the umbrella exposes them under the stable public names `runtime-tokio` /
   `tls-rustls`, but callers who bypass the umbrella must use the `_` names
   directly. Because these names are not part of sqlx's public API contract, a
   patch release (e.g. 0.8.2 → 0.8.3) could rename or remove them without
   a semver bump, silently breaking our build. An exact `=0.8.2` pin prevents
   `cargo update` from ever pulling in such a release without a deliberate,
   tested bump.

## Consequences

**Pros**
- The workspace resolves with matrix-sdk present.
- No compile-time database dependency; CI stays DB-free.
- Migrations remain embedded in the binary.

**Cons / risks**
- Depending on `_`-prefixed sqlx-core features is fragile; mitigated by the exact
  version pin. Bumping sqlx is now a deliberate, tested step.
- Hand-rolled `FromRow` and migration embedding are a small amount of code we own
  instead of getting from the macros (~80 lines total, unit-tested).

**When to revisit**
- If sqlx ships a way to use `sqlx-postgres` with public runtime/TLS features (or
  splits the umbrella so the macros don't drag in `sqlx-sqlite`), reconsider.
- If we ever drop matrix-sdk's SQLite store, the umbrella becomes usable again.

## Alternatives considered

- **`[patch.crates-io]` stub for `sqlx-sqlite`.** Keeps the umbrella (and the
  `migrate!` / `FromRow` macros) by shadowing `sqlx-sqlite` with an empty crate.
  Rejected: ships a fake crate shadowing a real one, breaks on every umbrella
  patch bump (the umbrella pins `=`), and confuses audit tooling and readers.
- **Align `libsqlite3-sys` versions.** Impossible — no overlapping requirement
  exists between `sqlx-sqlite` and `rusqlite 0.37`.
- **Replace sqlx with tokio-postgres.** Larger change, discards sqlx's migration
  and query ergonomics, and contradicts ADR 0003.
- **Downgrade matrix-sdk** to a version whose rusqlite matches sqlx. Rejected:
  the tail wagging the dog; 0.17 carries sync/UTD fixes we want.
