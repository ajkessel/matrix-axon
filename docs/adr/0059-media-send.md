# ADR 0059 — Media send: staged uploads and `m.image` / `m.file` mutations

**Status:** Proposed — targets **Milestone 15** (server-side media send/upload; review-only ADR, no code yet).

## Context

Axon's media story is currently asymmetric:

- **Read side:** M11 landed the authenticated MXC proxy (`GET /v1/media/{account_id}/{server_name}/{media_id}`) plus the bounded on-disk LRU cache (ADR 0045).
- **Write side:** M6 mutations deliberately stopped at text send/edit plus redact/react (ADR 0021). Its scope note explicitly deferred "richer msgtypes" to later work.

That gap is now visible in both the API and the product plan. Axon can fetch and
render media that already exists upstream, but it cannot originate an image or a
file attachment through its own `/v1/` surface. The integration seeder can send an
encrypted attachment directly with matrix-rust-sdk, which proves the SDK path is
viable, but that is test setup — not a client-facing Axon capability.

Milestone 15 closes this gap on the **server side only**. It does not include TUI
or iOS UX work. The goal is to make Axon capable of accepting bytes from a client,
staging them safely, and sending them into a room as a proper Matrix media event
through the same account-scoped SDK client that already powers text mutations.

The milestone is intentionally narrower than "all Matrix media":

- **In scope:** `m.image` and `m.file`
- **Out of scope:** `m.audio`, `m.video`, stickers, thumbnails, server-side image
  analysis, media editing, off-host/durable object storage, and client UX

## Decision

### Two-step contract: stage bytes first, then send from the staged upload

M15 uses a two-step, authenticated Axon API rather than a one-shot "upload and
send" endpoint:

1. `POST /v1/accounts/{account_id}/media/uploads`
   - Request body is the raw upload bytes.
   - Required query parameters: `kind=image|file`, `filename=<original name>`.
   - Optional `Content-Type` is accepted, sanitized, and persisted as metadata.
   - Response returns a server-issued `upload_id` plus normalized metadata.
2. `POST /v1/accounts/{account_id}/rooms/{room_id}/send-media`
   - Request body names the `upload_id` and optional `caption`, `reply_to`, and
     `thread_root`.
   - The response matches existing mutation style:
     `{ "data": { "event_id": "$..." } }`.

The split is deliberate:

- It keeps the final room send **room-aware** and able to reuse the existing M6
  relation model (`reply_to`, `thread_root`) rather than hiding it inside a
  multipart blob.
- It lets Axon validate and bound the inbound upload before attempting any
  homeserver operation.
- It gives the client a clean retry point when the send step fails after the
  bytes were already accepted.

### Server-owned staging area, not the media cache

Pending uploads are stored in a dedicated **data-dir-backed** staging area plus a
small Postgres metadata table. They do **not** live in the bounded M11 media cache.

The distinction is load-bearing:

- The M11 cache is a disposable read-through convenience whose source of truth is
  the homeserver.
- A pending upload is part of an **in-flight local mutation**. If Axon restarts
  after accepting the upload but before the user sends it, silently losing the
  bytes would be surprising and look like data loss.

So staged uploads are treated like durable crash-recoverable work-in-progress:

- bytes live under a dedicated uploads directory under Axon's data area
- a DB row records `account_id`, `upload_id`, `kind`, `filename`,
  `content_type`, `size_bytes`, `path`, `state`, `expires_at`, and timestamps
- boot reconcile prunes expired rows/files and orphan files, and resets any stale
  "sending" rows back to reusable staged state

### Keep `axon-api` SDK-free via ports, as with M6 and M11

The crate boundaries follow the established pattern:

- `axon-api` owns the **ports** it needs:
  - a staged-upload service for create/read/delete/consume
  - an outbound media-send capability alongside the existing message sender
- `axon-sync` owns the concrete SDK-driven send path
- `axon-server` adapts the sync-side implementation onto the API-owned ports

The send side must use the SDK's attachment-send path rather than a hand-built
raw event, because encrypted rooms require the SDK to upload ciphertext and emit
the matching `content.file` metadata correctly. This is the same reason the
integration seeder uses `room.send_attachment(...)` instead of constructing raw
JSON itself.

### Event shape and v1 semantics

M15 emits ordinary `m.room.message` events:

- `kind=image` -> `msgtype = "m.image"`
- `kind=file` -> `msgtype = "m.file"`

For both:

- `filename` is the uploaded filename
- `body` is the user caption when present, otherwise the filename

This matches the read-side assumptions already present in Axon's media handling
and in `axon-tui`'s event parsing: an image caption is the `body` when it differs
from the explicit filename.

No server-side thumbnail generation, dimension probing, duration probing, or EXIF
extraction is part of M15. Those are additive follow-up work once the core send
path exists.

### Resource bounds and failure model

Media upload crosses both a client boundary and a homeserver boundary, so M15
adds explicit bounds:

- `max_upload_bytes`
- `upload_request_timeout_secs`
- `upstream_upload_timeout_secs`
- `max_concurrent_uploads`
- `staged_upload_ttl_secs`

Uploads are streamed to disk with size enforcement before Axon attempts the
homeserver send. One failed upload or send is never fatal to the account
supervisor.

Error mapping stays HTTP-shaped and aligned with existing conventions:

- malformed params/body -> `400`
- missing account/room/upload -> `404`
- forbidden operation -> `403`
- account unreachable / transiently unavailable -> `503`
- homeserver upload/send failure -> `502`
- upload too large -> `413`

### Idempotency is deferred

M15 does **not** add client-supplied idempotency keys. If Axon crashes after the
homeserver accepts the attachment send but before the client receives the
response, a retry may duplicate the event. This matches M6's accepted duplicate
send caveat and keeps M15 focused on media support rather than a cross-cutting
retry protocol redesign.

## Consequences

- **Pro:** Axon's media story becomes symmetrical: clients can both send and fetch
  media through Axon's authenticated API.
- **Pro:** encrypted-room attachments use the SDK's native path, so the emitted
  events stay compatible with the existing M11 media proxy and the event-store
  lookups for `content.file` and `url`.
- **Pro:** the two-step staging model is crash-tolerant and gives clients a clear
  retry boundary without forcing a giant multipart "do everything at once" route.
- **Con / accepted:** the milestone is server-only; clients still need follow-up
  work to expose attachment-picking and send UX.
- **Con / accepted:** pending uploads consume local disk until sent, deleted, or
  expired; the TTL and size caps bound this.
- **Con / accepted:** audio/video/sticker send, thumbnail generation, and richer
  media metadata remain unimplemented after M15.

## Suggested PR sequence

1. **M15a — ADR + staged-upload substrate**
   - ADR 0059
   - upload metadata schema and store methods
   - staging filesystem service
   - `POST`/`DELETE /media/uploads`
   - OpenAPI + handler tests
2. **M15b — send-media mutation**
   - `POST /rooms/{room_id}/send-media`
   - SDK attachment-send adapter
   - relation support (`reply_to`, `thread_root`)
   - consume-on-success semantics
3. **M15c — reconcile and end-to-end coverage**
   - boot expiry/orphan cleanup
   - encrypted + unencrypted integration coverage
   - docs and verification guide updates
