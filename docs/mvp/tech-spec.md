# Axon MVP — Technical Specification

**Status:** Draft for review by Jamie, Adam, Steve.
**Audience:** Engineers and reviewers with working Matrix and Rust
literacy. Light on background; decisions and tradeoffs are the focus.
**Related docs:** [`prd.md`](./prd.md), [`implementation.md`](./implementation.md).

## Context & goals

Axon is the agent described in [`prd.md`](./prd.md): a self-hosted
persistent state layer for one human's Matrix accounts, consumed by
arbitrary clients (web alpha at MVP; native clients later) through a
stable HTTP + WebSocket API.

This document records the architectural decisions for the MVP, the
tradeoffs we weighed, and the items deliberately punted to post-MVP
or v2. It is not an implementation guide — that is
[`implementation.md`](./implementation.md).

## Architecture overview

```
                 ┌─────────────────────────┐
                 │   Upstream              │
                 │   Homeserver(s)         │
                 │   (Synapse / Dendrite)  │
                 └────────────┬────────────┘
                              │ Matrix C-S API
                              │ (Simplified Sliding Sync,
                              │  media)
                              ▼
                 ┌──────────────────────────┐
                 │   Axon (single binary)   │
                 │                          │
                 │   axon-sync              │
                 │   axon-crypto            │
                 │   axon-store ──────── Postgres
                 │   axon-search ─────── Tantivy
                 │   axon-media ──────── disk / S3
                 │   axon-api               │
                 └────────────┬─────────────┘
                              │ Axon API (REST + WS, /v1/)
                              ▼
                       ┌─────────────┐
                       │  axon-web   │  (MVP alpha)
                       │  (deferred: │
                       │   native    │
                       │   clients)  │
                       └─────────────┘
```

**Trust boundary.** Axon is a Matrix device, not a homeserver
extension. It is trusted by its single human owner the way a desktop
client would be. Upstream homeservers do not trust it beyond what
they trust any other client. Clients on the south side of Axon are
trusted only as far as their per-device bearer tokens.

## Settled decisions

### Single binary + Postgres + object store

One Rust binary (`axon`), one Postgres database (docker-compose
reference deployment), one object store backend for media (local disk
by default; S3-compatible adapter available). No microservices. No
required external dependencies beyond Postgres.

### matrix-rust-sdk for sync and crypto

Sync, olm/megolm, key backup, cross-signing, and verification are
delegated to matrix-rust-sdk. We do not reimplement any of it. Gaps
discovered during implementation get upstreamed.

### Simplified Sliding Sync only

Sync uses Simplified Sliding Sync (MSC4186) only. No legacy `/sync`
fallback. Tradeoff:

- **Win:** half the sync code path, half the test matrix, fits
  matrix-rust-sdk's preferred path.
- **Cost:** homeservers without SSS support are excluded. Synapse and
  Dendrite both ship it; the long tail of small homeservers is the
  excluded population.

We document the requirement and revisit if Steve / Adam / future
deployments hit a homeserver without SSS.

### Account model: one Axon per human, N Matrix accounts inside

A single Axon process serves a single human. That human may have N
Matrix accounts (e.g. personal + work, across different homeservers)
inside one Axon.

- Every account-scoped row carries an `account_id` foreign key.
- One matrix-rust-sdk `Client` and one crypto store per account.
- One combined Tantivy index with `account_id` as a facet field so
  unified search "just works" and per-account filtering is a query
  param.
- The WebSocket subscription delivers events from all accounts the
  human owns; every event carries its `account_id` in the envelope.
- The local API auth identifies the human (one token set per Axon);
  the human is authorised to act on any of their accounts.
- No cross-human isolation. Accounts inside one Axon share a process,
  a DB, and a filesystem because they belong to the same human.

This is deliberately not SaaS-style multi-tenancy. Multi-human
hosting is on the roadmap but is its own design problem (DB-level
isolation, operational tenancy boundaries, threat model expansion)
and is not in scope here.

### Event provenance

Every event row carries a `provenance` field. For MVP it is always
`upstream_homeserver`. The field exists so a future federated
ingestion path (a peer Axon importing decrypted history for a shared
room) can be modeled cleanly without schema changes. See "Federation
deferral" below.

### Event store schema: hybrid hot-columns + JSONB

Hot fields are extracted to indexed columns; full content stays as
JSONB. Considered:

- **Pure relational:** every event type gets its own table. Brittle,
  worst for unknown event types and spec churn.
- **Pure JSONB blob:** simplest schema, awkward indexing and ranking,
  slow timeline queries.
- **Hybrid (chosen).** Columns: `event_id`, `room_id`, `account_id`,
  `sender`, `origin_ts`, `type`, `redacts`, `relates_to`,
  `decrypted_body_text`. Full decrypted content as JSONB. Original
  ciphertext + megolm session metadata + sender device keys in
  sibling tables linked by `event_id`. Indexes on the hot fields.

Tradeoff: a bit of write-time cost extracting the hot columns, paid
back many times over on timeline pagination, ranking, and filter
queries.

### Append-mostly storage and room-lifecycle semantics

Per the brief: the store is append-mostly. Membership changes do not
retroactively rewrite history. Leaving / being banned / rooms being
deleted upstream do not delete the local archive by default. Per-room
retention policy (retain / hide / delete) is exposed but defaults to
retain. Room upgrades (`m.room.tombstone`) link old and new rooms in
the data model so timeline navigation and search work across the
upgrade.

### Content authentication

The store keeps original event bytes (ciphertext for encrypted
events, signed JSON for unencrypted), the megolm session ID and
re-decryption metadata, and sender device identity and cross-signing
chain at the time of receipt. This means decrypted rows can be
re-verified against the cryptographic evidence Matrix already
provides; we do not invent a separate HMAC or agent-level signing
layer.

Verification is exposed as an opt-in API capability: clients fetch
decrypted events normally, or request a verification bundle per
event/per query when they need it. Most traffic carries no
verification overhead.

### Live updates: WebSocket with a custom envelope

One bidirectional WebSocket at `/v1/ws`. Server → client carries
event push; client → server carries typing, draft sync, and read
markers. Considered:

- **Server-Sent Events:** simpler, browser auto-reconnect, but
  unidirectional. Client-to-server signals would need a parallel POST
  path. Net complexity higher.
- **WebSocket (chosen).** One channel covers both directions. axum
  has first-class support. Envelope: `{type, account_id, payload}`
  JSON so every event carries its account.

### Local-API auth: bearer tokens for MVP, OAuth later

MVP issues long-lived bearer tokens via an `axon token issue` CLI
subcommand. Each token is bound to a human-readable label / device
name, hashed at rest, and individually revocable. axum middleware
validates `Authorization: Bearer …` on every `/v1/…` request.

Full OAuth 2.0 + PKCE is on the roadmap. The token storage table,
the middleware, and the API shape are designed so OAuth issuance can
drop in without breaking the wire protocol. Tradeoffs we accepted:

- **Win:** alpha ships without owning a security-sensitive
  authorisation-server implementation.
- **Cost:** initial onboarding for `axon-web` is "paste the token
  from your CLI" rather than a login flow. Acceptable for an alpha
  whose user base is Jamie and Steve.

### API versioning: path-prefix `/v1/`, SemVer on the spec

All routes live under `/v1/…`. SemVer applies to the OpenAPI spec.
Breaking changes bump the major and move to `/v2/…`. Previous major
remains supported in parallel for a defined window — target: two
minor releases past the next major's GA, which gives client authors
a real deprecation runway. Considered date-based versioning
(Stripe-style) and Accept-header media-type versioning; both are
heavier than this API needs.

### Search: backend in MVP, single default analyzer

Tantivy index populated on event ingestion. `account_id` is a facet
field; queries can scope to one account or aggregate across all.
BM25 ranking. Filters: room, sender, account, date range.

Single language-agnostic analyzer for MVP: Tantivy default tokenizer
+ lowercase + light stemming. Per-language detection and per-room
overrides are deferred. Rationale: Steve-shape users have mostly
English / Latin-script content; the cost-benefit of per-language
analyzers doesn't pay off at MVP scale, and we keep the door open by
versioning the index schema.

### Bridges

Bridged events flow through as ordinary Matrix events. The agent
does not parse mautrix / Beeper / other bridge-specific formats and
does not surface a normalised `bridge_metadata` field. Clients
render whatever the bridge places in event content. Normalisation
is on the post-MVP roadmap.

### Onboarding: fresh sync only

First run logs the agent in as a new Matrix device and runs a fresh
sliding sync. No importer from Element X / gomuks / Fluffychat /
others. The Steve-shape success criterion (initial sync in under ten
minutes) is what makes fresh sync acceptable.

### Push deferred entirely

No APNs / FCM / web push code paths in MVP. The event store schema
and the event-emit surface inside the agent are designed so a push
router can be added later without schema changes.

## Open decisions for the group

None material. Every item from the brief's "Open questions for the
implementation plan" was resolved during planning:

| Brief open question | Decision |
|---------------------|----------|
| Multi-tenancy model | One human per Axon; N Matrix accounts inside, `account_id`-scoped tables. SaaS multi-tenancy deferred. |
| Sliding sync vs legacy sync fallback | Simplified Sliding Sync only. |
| Event store schema | Hybrid hot-columns + JSONB. |
| Search analyzer defaults | Single language-agnostic analyzer for MVP. |
| Push payload format | Push deferred entirely. |
| OAuth implementation | Bearer tokens for MVP; OAuth 2.0 + PKCE post-MVP. |
| Live-update transport | WebSocket with custom envelope. |
| API versioning policy | Path-prefix `/v1/`, SemVer; previous major supported two minor releases after next major GA. |
| Migration story | Fresh sync only for MVP. |
| Bridge event handling | Treated as ordinary Matrix events; no normalisation. |

This table exists so Adam and Steve can push back on any individual
call without having to reconstruct the alternatives from scratch.

## Threat model summary

(Condensed from the brief; flag for what changes when push lands.)

- **Operator trust.** The Axon operator can read all decrypted
  content for the human it hosts. For Axon this is the human
  themself, since one Axon hosts one human. Self-hosting is the
  story.
- **Disk compromise.** Event store and search index encrypted at
  rest via filesystem / Postgres. Per-account content-encryption
  keys are deferred to v2.
- **Network — agent ↔ homeserver.** Standard Matrix C-S over TLS.
- **Network — client ↔ agent.** TLS required; bearer tokens scoped
  per device, revocable individually.
- **Client compromise.** Per-device tokens limit blast radius.
  Revocation invalidates a single device. Already-pulled history is
  out of the agent's hands — same as any Matrix client.
- **Compromised agent process.** Worst case: all data for the human
  owner. Mitigations: process isolation, principle of least
  privilege, audit logging. Not solved at v1.

**Changes when push lands:** APNs / FCM payload privacy levels
become a user-facing setting. The push router becomes another
process with access to decrypted content. Threat-model section will
need updating in the push design doc.

## Federation deferral

Axon v1 is one-agent-per-human with no agent-to-agent communication.
The brief argues for keeping the door open by capturing event
provenance now: every event row records where its decrypted content
came from (`upstream_homeserver` only, for v1) and preserves the
cryptographic evidence (original ciphertext, megolm session, sender
device keys, cross-signing chain) so a peer agent's content could be
verified against the homeserver's signatures without trusting the
peer.

Implications for MVP schema:

- `events.provenance` column exists from day one.
- Original ciphertext and megolm metadata are sibling tables with
  `event_id` FK, not just optional verification fields.
- Content / metadata separation is observed so a federated path can
  ingest decrypted content while metadata (read state, drafts) stays
  per-account.

We do not build any federation code in v1.

## Roadmap signposts

Post-MVP, roughly in priority order:

1. Full OAuth 2.0 + PKCE.
2. Push (APNs first, then FCM and web push).
3. Bridge metadata normalisation.
4. Import-from-existing-client onboarding (Element X store reader,
   maybe gomuks).
5. Per-room / per-language search analyzers.
6. Threads / spaces as first-class API resources.
7. Native clients (iOS first, then desktop).
8. Multi-human (SaaS-style) hosting with cross-human isolation.
9. Federation of agents v2.
