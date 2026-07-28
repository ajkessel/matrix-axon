# ADR 0019 — Client read API: account-nested routes, envelope, cursor

## Context

M5 ("Client API v0") opens the first real read surface over the M3/M4 store. M5
is split three ways along its dependency seams — **5a** is the read-only HTTP API
(this ADR), **5b** the `/v1/ws` live fan-out, **5c** interactive verification.
5a ships three endpoints (`GET /v1/rooms`, a room timeline, a single event), a
shared JSON envelope, and the utoipa-emitted OpenAPI spec.

`docs/mvp/implementation.md` §5 sketches the routes as flat
(`/v1/rooms/{room_id}/timeline`, `/v1/events/{event_id}`) with `account_id` as a
filter. But the store is account-scoped to its roots — every table carries
`account_id`, and the read methods are keyed `(account_id, room_id)` /
`(account_id, event_id)`. A flat `room_id`/`event_id` is ambiguous: the same
Matrix room or event can exist under two of a user's accounts. The spec didn't
pin down how to resolve that, so this ADR records the decisions 5a makes.

## Decision

### Account-nested canonical routes

Account-scoped *resources* nest the account in the path; the cross-account list
stays flat:

- `GET /v1/rooms` — the **cross-account aggregate** ("unified inbox"), newest
  activity first, with an optional `?account_id=` filter. Each item carries its
  `(account_id, room_id)`, so a client can build the canonical detail URL.
- `GET /v1/accounts/{account_id}/rooms/{room_id}/timeline`
- `GET /v1/accounts/{account_id}/events/{event_id}` — the event nests under the
  **account, not the room**: the store keys events by `(account_id, event_id)`,
  so `room_id` is neither needed nor always known to a caller holding only an
  event id (a reply reference, a future search hit).

This deviates from the literal flat routes in the (frozen) implementation spec,
but is **more** consistent with the spec's own M7 media route,
`/v1/media/{account_id}/{server}/{media_id}`, which already nests `account_id`.
It removes the ambiguity at the type level — no resolution layer, no "optional
but sometimes required" query param — and maps 1:1 onto the store methods.

**Convention (forward-looking).** Nest `account_id` on *all* account-scoped
resource routes. This ripples to later milestones: M6 mutations become
`POST /v1/accounts/{account_id}/rooms/{room_id}/send` (etc.), dropping
`account_id` from the request body. `/v1/rooms` (and any future cross-account
aggregate) stays flat by design — it is the one view that spans accounts.

### Response envelope

Every `/v1/` response is one of two shapes:

- success: `{ "data": <payload> }` (`ApiResponse<T>`);
- error: `{ "error": { "code": <stable string>, "message": <human text> } }`
  (`ApiError`), with the HTTP status carrying the category.

A single `IntoResponse` per type keeps every handler consistent. `StoreError`
converts into a logged `500` with a generic body (SQL detail never crosses the
wire). Status mapping: missing event → `404`; malformed cursor / bad path param
→ `400`; any store error → `500`. An unknown room's timeline returns an empty
`200` page rather than a `404` — an empty timeline and a non-existent room are
indistinguishable without an extra probe, and an empty page is the natural
answer.

### Opaque pagination cursor

The store's `TimelineCursor` is a public `(origin_ts, id)` sort key. The wire
cursor is the base64url (no padding) of `"{origin_ts}.{id}"`, returned as
`next_cursor` on each timeline page (`null` at the end). Encoding it keeps the
on-the-wire contract fixed even if the internal sort key changes later, and a
malformed cursor is a clean `400`.

### OpenAPI as a golden file

utoipa emits the spec from the handler signatures. A DB-free test serializes
`ApiDoc::openapi()` and diffs it against the checked-in `openapi/openapi.json`,
so handler/spec drift fails CI. Regenerate with
`UPDATE_OPENAPI=1 cargo test -p axon-api --test openapi`. Chosen over a binary
subcommand because it needs no server boot and runs in the existing test lane.
TypeScript client generation is deferred to M11 (the web client); 5a only emits
and pins the spec.

## Consequences

- **Pro:** URLs encode the real `(account_id, room_id)` identity; no ambiguity
  branch; handlers map straight onto store methods; spec drift is a failing
  test; the `AppState`/`FromRef` seam lets 5b add a broadcast sender with no
  churn to existing handlers.
- **Con:** deviates from the literal flat routes in the frozen spec, and commits
  M6/M7 to the nesting convention (recorded here so the next agent doesn't
  re-implement them flat). The aggregate list and the detail routes live in
  different shapes — a deliberate, conventional REST split (cf. a top-level
  collection vs. canonical nested resources).
- **Revisit** if cross-account *detail* reads are ever wanted (today only the
  list spans accounts), or if a client genuinely needs to resolve an event id
  without its account.
