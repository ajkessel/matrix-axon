# ADR 0060 — Device-list / discovery endpoint

## Context

The MVP's SAS verification endpoint (`POST /v1/accounts/{account_id}/verify`,
ADR 0011/0027) takes a bare `device_id` or `user_id` string with nothing to
look it up against. ADR 0053 ("iOS client-server prerequisites") named this
gap explicitly as item 2, "Device-listing / discovery endpoint" — needed so a
client can offer a real device picker instead of requiring the operator to
already know a target device id blind, which is what `axon-tui` (ADR 0028)
and the draft web client both do today (tracked as issue #84, ADR 0028/0034).

ADR 0054's Open Questions section anticipated exactly this: if the
device-listing endpoint proceeded, it should claim its own milestone number
rather than interleave with M14's (OAuth) lettered sub-PRs. M15 is already
claimed by media-send (ADR 0059), so this is **M16**, fully independent of
M14 — M14 continues separately, unaffected by this ADR.

## Decision

### Scope: own user by default, arbitrary user via query param; server-only

One endpoint: `GET /v1/accounts/{account_id}/devices?user_id=<optional>`.
Omitting `user_id` lists the account's own devices (the self-verification
picker case); supplying it lists another Matrix user's devices (the
cross-user picker case, ADR 0040 parity). A single optional query parameter
was chosen over two separate routes or a `POST`-style mutually-exclusive-body
shape (as `StartVerifyRequest` uses) because this is a `GET` — parameters
belong in the query string — and there is no ambiguity to reject: the
parameter's absence is a well-defined default (self), not an error, unlike
`verify`'s two-mutually-exclusive-targets shape.

This milestone is the `/v1/` endpoint only. The `axon-tui`/web-client device
picker that consumes it is separate follow-on client work — the same
backend/client split ADR 0053 itself drew, and consistent with 7a-6 and 7c
both having scoped "backend API only" in their own ADRs.

### No new storage — read the SDK's own crypto-store cache on every request

Unlike ADR 0058's sender-trust bundle (which has a durable, immutable
at-decrypt snapshot *plus* live evidence), a device list has no snapshot half
at all. `client.encryption().get_user_devices(user_id)` is a **local
crypto-store read**, not a homeserver round-trip: it calls
`OlmMachine::get_user_devices(user_id, None)`, and the `None` timeout makes
its internal `wait_if_user_pending` check a no-op, so it never issues or waits
on a `/keys/query`. Freshness is therefore bounded by however current the
SDK's own crypto store is — kept current by the account's background sync
loop, not by this endpoint — so a device added moments ago can be briefly
absent until the next sync. The "no new table" decision holds regardless: the
SDK already maintains this cache, so an axon-owned materialization on top
would just be a second cache to keep in sync with the first, not a freshness
improvement.

### Same three-crate port/engine/adapter split as 7a-6 and 7c

- `axon-api` defines the `DeviceListService` port (async trait, `DeviceListError`
  enum, `DeviceList`/`DeviceInfo` domain structs) — no `axon-sync` or
  `matrix-sdk` dependency, matching `crate::trust::SenderTrustService`'s shape.
- `axon-sync` owns the concrete `DeviceListEngine` (needs a live `Client`),
  exposed via a `SyncEngine::devices()` accessor alongside the existing
  `sender_trust()`.
- `axon-server` adapts one onto the other at startup (`DeviceAdapter`), wired
  into `AppState` beside `trust`.

Naming note: `DeviceListService`, not `DeviceDiscoveryService` — this reads
what the SDK already tracks post-`/keys/query`; it performs no new discovery
protocol of its own. "Discovery" in ADR 0053's title describes the
client-facing use case, not a new server capability, so the port name
describes the mechanism.

### Read-only, no per-identity lock, explicit `active` gate

Mirrors `SenderTrustEngine::bundle`'s shape: this is a read that tolerates a
client a concurrent teardown is severing. Since `get_user_devices` is a local
crypto-store read (see above), there's no homeserver round-trip that could
get stuck behind a held lock either way — staying lock-free here is mostly
about keeping this engine's shape consistent with the bundle precedent, not
avoiding a specific stall. `ClientManager::get_or_connect`'s cache-hit fast
path does not re-check `accounts.state`, so the engine reads the `accounts`
row itself first and returns `NotActive` for any non-`active` row before
touching the client.

### Field selection

Per device: `device_id`, `display_name`, `is_verified` (the SDK's combined
locally-or-cross-signing-trusted predicate), `is_cross_signed_by_owner`
(the finer-grained signal), `local_trust_state`, `algorithms`. Deliberately
excluded: raw curve25519/ed25519 key material (a SAS picker needs no raw
keys — the trust bundle, ADR 0058, is where forensic key material belongs,
for a different consumer); `last_seen_ts`/`last_seen_ip` (not exposed by
matrix-sdk 0.18's `Device` type — a smaller surface than the full Matrix C-S
`GET /_matrix/client/v3/devices` shape, which axon does not implement anyway,
since axon is a client of upstream homeservers, not a homeserver — no
federation, per `AGENTS.md` "what not to build"). Deleted devices
(`Device::is_deleted()`) are filtered out rather than surfaced with a flag —
a picker has no use for an already-gone device.

### Errors and HTTP status

Since `get_user_devices` is a local read, `Upstream`/502 is reserved for
`get_or_connect` genuinely failing to establish or restore the SDK
connection; a corrupt stored `user_id` or a failure inside the local
crypto-store read itself are internal-consistency problems, not homeserver
failures, so they're `Store`/500 (logged, not detailed to the caller) instead:

| Condition | HTTP |
|---|---|
| Unknown `account_id` | 404 |
| Account `deactivated`/`deleting` | 409 |
| `?user_id=` fails Matrix user-id parse | 400 |
| `get_or_connect` fails to establish/restore the SDK connection | 502 |
| Corrupt stored `account.user_id`, or `get_user_devices`'s local crypto-store read fails (`NoOlmMachine`, a wrapped `CryptoStoreError`) | 500 (logged, not detailed to caller) |
| `axon-store` Postgres read failure | 500 (logged, not detailed to caller) |

**A `user_id` axon has never tracked returns `200` with `"devices": []`, not
404 or an error** — matching the house "empty, not 404" convention
(established in M8b for reactions/replies/edits/threads). This is worth
calling out explicitly for the cross-user case: `get_user_devices` only ever
returns what's in the local crypto store, and axon only tracks a user's
identity/devices once it shares an encrypted room with them (or otherwise
already downloaded their keys) — so for the *typical* cross-user picker
call, "no devices" and "this user was never tracked" are the same `200 []`
response, indistinguishable to the caller. This is not a rare edge case; for
someone not yet in a shared encrypted room with the account, it's the normal
outcome. A client driving the cross-user picker should treat an empty list as
"nothing to verify yet" rather than assuming device enumeration definitively
failed. Proactively populating an untracked user (e.g. via
`Encryption::request_user_identity`, which does hit the homeserver) is a
reasonable follow-up if this proves too limiting in practice, but is out of
scope for v1 — this endpoint stays a pure local read with no request-time
network calls of its own.

### No pagination or cap

`get_user_devices` returns an SDK-cached, bounded set from `/keys/query`, not
a paginated homeserver query — unlike `RELATION_READ_CAP`-guarded reads
elsewhere, no result cap is added. Flagged here as a deliberate non-issue for
v1, revisit only if it proves wrong in practice.

## Consequences

- `AppState::new` gains a `devices: Arc<dyn DeviceListService>` argument —
  one composition-root wiring plus updating every test call site that
  constructs `AppState` (the same mechanical cost ADR 0058 incurred for
  `trust`).
- A client can now build a real device picker; the picker UI itself remains
  separate follow-on client work (issue #84 stays open until a client
  consumes this).
- Closes ADR 0053 item 2. Item 1 (OAuth) continues as M14; item 3 (ADR 0030
  `sync_state`) remains unclaimed.
- Single PR, no schema change: this is scoped like 7c (sender-trust bundle,
  itself a one-PR milestone), not like M14/M15's multi-PR sequences — there
  is no genuinely separable risky sub-piece (no new staging substrate, no
  multi-provider fan-out) worth isolating behind its own review.
