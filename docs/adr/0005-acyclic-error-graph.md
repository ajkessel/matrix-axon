# ADR 0005 — Acyclic error graph: `Store(String)` in `axon-core`

## Context

`axon-core` is the lowest crate in the dependency graph — every other crate
depends on it, so it must not depend on any of them. Its top-level `Error` enum
is the shared error type that `axon-server` (the binary boundary) works with.

`axon-store` produces a `StoreError` (sqlx variants) that `axon-server` needs to
report. The question is: how does `axon_core::Error` represent a store failure
without `axon-core` depending on `axon-store`?

## Decision

`axon_core::Error` carries a `Store(String)` variant. The structured `StoreError`
is stringified at the `axon-store` → `axon-core` boundary via:

```rust
impl From<StoreError> for axon_core::Error {
    fn from(err: StoreError) -> Self {
        axon_core::Error::Store(err.to_string())
    }
}
```

`axon-core` never depends on `axon-store`; the dependency arrow always points
toward `axon-core`.

## Consequences

**Pros**

- **Acyclic graph.** Crates compile in parallel, dependency direction is
  unambiguous, and no import cycle is possible.
- **`axon-core` stays lean.** It does not pull in sqlx (and its large transitive
  tree) merely to name an error variant.
- **Structured errors are still available where they're needed.** `axon-api`
  depends on `axon-store` directly. A handler can catch `StoreError` *before*
  it's flattened — e.g. match `sqlx::Error::Database` to detect a unique-
  constraint violation and return HTTP 409 — and only propagate the stringified
  form for the generic "something failed" path.

**Cons / risks**

- **Lossy at the boundary.** Once `.to_string()` is called, the structured
  `sqlx::Error` is gone. Callers holding an `axon_core::Error::Store(_)` cannot
  introspect the underlying cause programmatically.
- **Stack traces.** Rust does not embed stack traces in errors by default (unlike
  Python). `anyhow` captures a backtrace when `RUST_BACKTRACE=1` (or `full`) is
  set in the environment, so the binary boundary — `axon-server`'s `main` — will
  log a traceable chain via `anyhow::Context`. The stringification does not
  discard the causal chain visible to the operator; it only prevents programmatic
  branching on error subtypes above the boundary. Setting `RUST_BACKTRACE=1`
  restores the full call-chain view.

**When to revisit**

If any code *inside `axon-core` itself* needs to branch on a `StoreError`
subtype, this decision is wrong. The fix at that point is to extract an even
lower `axon-types` crate (pure data types, no sqlx dependency) rather than
inverting the dependency arrow — inverting would create a cycle.

## Alternatives considered

**`axon-core` depends on `axon-store`, holds `Store(#[from] StoreError)`**

Rejected: this inverts the dependency graph. `axon-store` already depends on
`axon-core` for shared types, so `axon-core` → `axon-store` would be a compile-
time cycle. It would also bloat `axon-core` with sqlx's dependency tree.
