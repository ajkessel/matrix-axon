# ADR 0030 — Account sync-state readiness signal

## Context

On server startup, each account's matrix-rust-sdk `SyncService` must complete
its first sync cycle before send mutations work reliably for encrypted rooms.
Until that first sync completes, `room.send()` blocks inside the SDK while it
waits for the megolm sessions needed to encrypt outgoing messages. The block is
typically a few seconds on a warm session but can exceed 60 s on a fresh device
with a large key-backup import (see ADR 0026 — recovery-key derivation runs
synchronously before the sync service starts).

Three things are currently missing:

1. **No client-visible readiness signal.** `GET /v1/accounts` returns the Matrix
   lifecycle state (`active` / `deactivated` / `deleting`, per ADR 0022), which
   has nothing to say about whether the sync engine is ready to send. `GET
   /healthz` is a process-liveness probe, not a sync-readiness probe.

2. **No cap on the blocking window.** The API send routes (`POST …/send`,
   `PUT …/events/{id}`, `DELETE …/events/{id}`, `POST …/reactions`) call into the
   `SdkGateway`, which calls `room.send()` / `room.redact()` / etc. with no
   timeout. A slow SDK hangs the HTTP handler and, transitively, freezes any
   synchronous TUI caller that has not added its own timeout.

3. **No spec guidance.** `docs/mvp/tech-spec.md` and
   `docs/mvp/implementation.md` do not address the startup transient at all.

This ADR records the decisions made to address all three gaps.

## Decision

### 1. Expose `sync_state` on the account DTO

Add a `sync_state` string field to `AccountDto` (and to the response of every
route that returns an account: `GET /v1/accounts`, `GET /v1/accounts/{id}`,
`POST /v1/accounts/login`, `POST /v1/accounts/{id}/logout`).

The field carries one of four values:

| Value | Meaning |
|---|---|
| `"connecting"` | SDK client not yet built or session not yet restored |
| `"syncing"` | Sync service running; first sync not yet complete — mutations may block |
| `"ready"` | At least one successful sync cycle complete; mutations are reliable |
| `"offline"` | Sync service has lost the homeserver connection and is retrying |

`"error"` is deliberately omitted: a sync error triggers a supervised restart
(ADR 0022); the brief window between the error and the restart is
indistinguishable from `"connecting"` from the client's perspective.

The value is derived from the matrix-rust-sdk `SyncService::State` stream
(`Running` / `Idle` → eventually `"ready"`, `Offline` → `"offline"`, not-yet-
started → `"connecting"`) plus a one-shot flag set on the first successful sync
cycle. The flag is held in memory only (it resets on server restart, which is
correct — a restart starts a fresh sync).

The sync engine exposes this via a new `SyncStateSnapshot` port (a cheap
`Clone`-able handle backed by `Arc<RwLock<…>>`), returned from `SyncEngine` and
wired into `AppState` alongside the existing `AccountLifecycle` and
`MessageSender` ports.

### 2. Emit `account.sync_state` frames on the WebSocket

When an account's `sync_state` transitions, the sync supervisor pushes an
`account.sync_state` frame on the existing best-effort `/v1/ws` broadcast bus:

```json
{
  "type": "account.sync_state",
  "account_id": "<uuid>",
  "payload": { "sync_state": "ready" }
}
```

This lets a connected TUI update per-account status in real time without
polling. The delivery guarantee is the same as timeline events — best-effort,
no replay on reconnect. A client that connects after a transition has already
fired recovers the current state from `GET /v1/accounts`.

### 3. TUI shows per-account sync state; disables send with explanation while `syncing`

`axon-tui` reads `sync_state` from the initial `GET /v1/accounts` response and
updates it on each incoming `account.sync_state` frame.

- **`"connecting"` / `"syncing"`**: the accounts panel annotates the account
  with a brief indicator (e.g. `[syncing]`); attempting to send shows a status
  message — "server is still syncing for this account, please wait" — rather
  than submitting the request. This prevents the 60 s timeout from firing at all.
- **`"offline"`**: similar annotation; sends are allowed (the gateway may
  succeed if the homeserver is reachable despite the sync outage) but the user
  is informed.
- **`"ready"`**: no annotation; send behavior unchanged.

### 4. Add a server-side timeout on mutation routes as defense-in-depth

Regardless of the client-side guard, the API mutation handlers wrap the
`SdkGateway` call in a `tokio::time::timeout` (30 s). A timeout surfaces as a
`504 Gateway Timeout` with an enveloped error body rather than an indefinitely
hung HTTP connection. This protects any client that does not implement the
`sync_state` guard (e.g. a future third-party client, or a `curl` invocation).

The TUI already has a 60 s reqwest-level timeout (added alongside this ADR);
the server-side 30 s timeout is intentionally shorter so the server always
responds before the client times out.

## Consequences

- `AccountDto` gains a `sync_state` field. Existing clients that ignore unknown
  fields are unaffected; clients that deserialize strictly will need updating.
- The sync engine grows a `SyncStateSnapshot` port (read-only handle), which is
  cheap and does not change the supervision or lifecycle model.
- The TUI's accounts panel and send path have new logic conditioned on
  `sync_state`; both are confined to the existing `refresh_accounts` /
  `handle_live_frame` / `handle_compose_key` paths.
- The startup freeze bug (no timeout on send) has two independent fixes after
  this ADR: the TUI-side `sync_state` guard prevents the request from being
  sent at all, and the server-side 30 s timeout caps the hang if the guard is
  bypassed.
