# ADR 0001 — Record Architecture Decisions

## Context

As Axon is built, contributors (human and agentic) will make implementation decisions that are not covered by the MVP specs. Without a record, the reasoning behind those decisions is lost, leading to confusion about why things are the way they are or unintentional reversals of deliberate choices.

## Decision

We will record non-obvious implementation decisions as lightweight Architecture Decision Records (ADRs) under `docs/adr/`, using the Michael Nygard format: **Context / Decision / Consequences**, one page maximum.

Filename pattern: `NNNN-kebab-case-title.md`, monotonically numbered.

Write an ADR when:
- You pick one library over another for a non-trivial reason.
- You make a schema or API choice the specs don't prescribe.
- You discover an upstream bug or quirk and work around it.
- You decide *not* to do something that seems like an obvious next step.

Do **not** write ADRs for decisions already settled in `docs/mvp/` — those are anchored there and re-stating them creates drift. The ADR directory is for what happens *during* implementation that the specs don't cover.

## Consequences

- Decisions have a discoverable, dated record.
- Future contributors can understand the "why" without reverse-engineering the code.
- The `docs/mvp/` specs remain the source of truth for pre-implementation decisions; ADRs capture what happened after.
