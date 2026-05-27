# Axon MVP — Product Requirements

**Status:** Draft for review by Jamie, Adam, Steve.
**Related docs:** [`tech-spec.md`](./tech-spec.md), [`implementation.md`](./implementation.md).

## Project

**Axon** is a self-hosted personal agent for Matrix. It sits between a
user's homeserver(s) and their clients, holding the persistent state,
search index, and per-device coherence that Matrix clients otherwise
reinvent themselves. Clients consume it through a documented,
versioned HTTP + WebSocket API.

**Naming convention:**

- Rust crates: `matrix-axon-*` prefix (e.g. `matrix-axon-store`).
- Internal workspace path: `crates/axon-*`.
- Binary / product: `axon`.
- Alpha web client: `axon-web`.

## One-line pitch

The personal agent layer Matrix is missing — what mail has had for
decades — designed to be self-hosted and consumed by arbitrary clients
through a stable API.

## Problem & opportunity

Matrix's client-server architecture pushes a lot of work onto clients:
continuous sync, E2EE key management, local indexing, push handling,
per-device state. This works for desktop clients with persistent
connections and generous storage. On mobile and across devices it
falls apart:

- Initial sync is slow because the phone fetches and decrypts hundreds
  of thousands of events.
- Local storage grows unboundedly as media accumulates.
- Search either lives only on-device (rebuilt per install, no
  cross-device) or doesn't exist.
- Push notifications either carry no content (useless) or require the
  phone to wake and decrypt (battery-hostile and brittle).
- Multi-device state beyond read receipts (drafts, scroll position,
  mutes) doesn't sync.
- Reinstalling a client starts from scratch.

Existing tools cover parts of this but not cleanly:

- **Element X / matrix-rust-sdk clients** push everything client-side.
- **gomuks** has the right shape but is single-user, single-account,
  no documented API, designed for its own UI.
- **Pantalaimon** decrypts only.
- **Beeper** solves it but is a commercial hosted service.

The conceptual gap: Matrix has no analogue of an IMAP / SMTP server
combined with a personal search index — a hosted persistent state
layer that arbitrary clients can consume.

## Target user for MVP

The "Steve-shape" user from the brief:

- 10–50 rooms, mostly active.
- 100–200k events of history.
- Mostly E2EE rooms.
- Currently uses one Matrix account.

The MVP must let Steve self-host Axon, point it at his homeserver,
and use the web alpha as his primary read/reply surface.

## MVP product scope

### Axon (the agent)

- **One process per human.** Each Axon instance has a single trust
  boundary: a single human owner.
- **Multiple Matrix accounts inside one Axon.** The agent supports N
  accounts (e.g. personal + work, across different homeservers) under
  that single owner. MVP provisions one account from config but the
  data model and code path are N-account from day one.
- **Sync.** Simplified Sliding Sync (MSC4186) against Synapse and
  Dendrite. Legacy `/sync` is not supported.
- **E2EE.** olm / megolm, server-side key backup, basic device
  verification (verification UX is API-side; not required in the
  alpha client).
- **Event store.** Postgres-backed. Decrypted timelines, room state,
  account data, original ciphertext, megolm metadata, sender device
  keys preserved for later verification.
- **Search.** Tantivy index populated on event ingestion. BM25
  ranking, room / sender / account filters. Backend only — no search
  UI in the alpha client.
- **Media proxy.** Fetches upstream MXC URLs, caches to local disk
  (S3-compatible backend optional), serves to clients with caching
  headers.
- **Drafts and per-device read state.** Opaque blobs scoped to
  `(account, device, namespace)`, synced over WS.
- **Local API.** REST over `/v1/`, WebSocket at `/v1/ws`. OpenAPI 3.1
  spec is the source of truth; TypeScript stubs generated for
  `axon-web`, Swift stubs generated for the deferred iOS client.
- **Auth.** Long-lived bearer tokens issued via an `axon` CLI
  subcommand. Tokens are per-device-name and revocable. Full OAuth
  2.0 + PKCE is deferred post-MVP.
- **Deployment.** Single Rust binary plus docker-compose for
  Postgres. Self-hosting guide bundled.

### `axon-web` (the alpha client)

A small React + TypeScript web app that exercises the agent API
end-to-end:

- Rooms list.
- Room view with paginated timeline.
- Composer: send, edit, redact, react.
- Inline media viewing through the agent media proxy.
- Auth: paste-token form using a token minted by the agent CLI.

`axon-web` exists to validate the API surface and to give Steve a
usable client on day one. It is not a polished product; it is a
deliberately minimal integration test.

## Out of scope for MVP

These are deliberately deferred. Many appear on the post-MVP roadmap.

- **Multi-human hosting** (SaaS-style isolation between different
  humans inside one Axon process). One human per Axon, always.
- **Push notifications.** No APNs, no FCM, no web push. The event
  store and event-emit surface are designed so a push router can be
  added later without schema changes.
- **Threads / spaces as first-class API resources.** Events flow
  through; no thread- or space-specific endpoints.
- **Voice / video signalling.**
- **Advanced search UI** (faceted, semantic). Search backend ships;
  rich UI does not.
- **Admin API.**
- **Backup / restore tooling.**
- **Migration tooling from existing clients.** Onboarding is fresh
  sync only.
- **E2EE verification UX in the alpha.** Agent-side capability
  exists; the alpha doesn't drive it.
- **Native desktop / iOS clients.** A reference iOS app is a separate
  future project; the MVP ships generated Swift stubs only.
- **Full OAuth 2.0 server.** Bearer tokens for MVP; OAuth + PKCE
  later.
- **Bridge metadata normalization.** Bridged events flow as ordinary
  Matrix events; clients render whatever the bridge places in event
  content.
- **Per-room or per-language search analyzers.** One default
  language-agnostic analyzer for MVP.

## Non-goals

These are not deferred — they are out of scope for the project as a
whole, not just the MVP.

- **Not a homeserver.** Axon sits in front of an existing homeserver
  (Synapse, Dendrite, Conduit). Federation, room creation, and state
  resolution stay the homeserver's job.
- **Not a bridge.** Bridges live elsewhere. Axon treats bridged
  events as ordinary Matrix events.
- **Not a UI as the deliverable.** `axon-web` exists as an
  integration test; the product is the agent and its API.
- **Not federated agents in v1.** A federated agent network is a
  credible v2/v3 direction but is not in scope here. Event provenance
  is captured in the schema so federation can be added later as an
  ingestion path, not a rewrite.
- **Not optimized for the no-E2EE case.** Plaintext rooms work fine,
  but the design assumes E2EE is the common case.

## Success criteria

The MVP is done when:

1. **Initial sync.** A Steve-shaped account completes initial sync
   against Synapse on a residential connection within ten minutes.
2. **Search latency.** Full-history search returns relevant results
   in under 200ms p95.
3. **Reinstall.** A client built against the API can be wiped and
   reach full functionality within seconds of re-authenticating; no
   local backfill required.
4. **API documentation quality.** A developer who has not read the
   source can build a working client from the OpenAPI spec alone.
5. **Self-hosting.** A competent Linux user deploys Axon on their own
   hardware in under an hour using the self-hosting guide.
6. **Steve in a browser.** Steve uses `axon-web` against his
   self-hosted Axon to read and reply to his real Matrix rooms for a
   week without falling back to another client.

## Open product questions

None material for MVP scope. Bridge metadata surfacing and
import-from-existing-client are both explicitly deferred post-MVP.
See [`tech-spec.md`](./tech-spec.md) for the implementation decisions
that touch product behaviour (auth, search analyzer, bridges,
onboarding).
