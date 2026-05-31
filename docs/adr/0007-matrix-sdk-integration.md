# ADR 0007 — matrix-rust-sdk integration: SyncService, per-account supervision

## Context

Milestone 3 connects Axon to homeservers. We need one authenticated Matrix
client per account, running Simplified Sliding Sync (MSC4186, no legacy
`/sync`), with persistent crypto state so E2EE survives restarts. We build on
`matrix-sdk` rather than implementing the Matrix client/crypto stack ourselves.

## Decision

**Pin `matrix-sdk` and `matrix-sdk-ui` to `0.17` together.** They are released in
lockstep and version skew breaks compilation. `matrix-sdk` features:
`e2e-encryption` (Olm/Megolm), `bundled-sqlite` (vendors SQLite — no system
`libsqlite3` needed). TLS is whatever the SDK's default reqwest stack provides
(0.17 exposes no standalone TLS feature).

**Run Simplified Sliding Sync via `matrix_sdk_ui::sync_service::SyncService`,
not the low-level `SlidingSync` builder.** `SyncService::builder(client).build()`
owns a `RoomListService` → `SlidingSync` internally and is the SDK's maintained,
recommended path. We get internal retry for transient errors, plus a `state()`
stream (`Idle` / `Running` / `Terminated` / `Error` / `Offline`) for supervision.
We chose `SyncService` over the lower-level `Client::sync()` (legacy long-poll)
deliberately: both yield full E2EE handling, but `SyncService` gives the
windowed-but-forward sync model and lifecycle signals we want. For a backend
archiver this behaves as "subscribe to all rooms, receive events forward".

**One SQLite store per account** under `sync.data_dir/<account_id>`, via
`Client::builder().sqlite_store(dir, Some(passphrase))`, passphrased with
`sync.store_key`. This holds SDK state + crypto material and is separate from the
Postgres archive. It must be durable: losing it forces re-login and loss of
historical Megolm sessions.

**Authenticate login-once, restore-thereafter.** First boot logs in with the
configured credential (`matrix_auth().login_username(..).send()`); we persist the
returned access token (encrypted — ADR 0008) and device ID. Later boots build a
`MatrixSession` from the stored token and call `restore_session`, so the password
is consumed exactly once and never stored.

**Supervise one task per account with exponential backoff.** Each task builds the
client, starts the `SyncService`, and watches `state()`. On `Error`/`Terminated`
(or a closed stream) the task restarts with backoff (1s → 60s). A
`CancellationToken` drives graceful shutdown: the task calls `sync_service.stop()`
so the SDK store flushes before the process exits.

## Consequences

**Pros**
- E2EE, key management, and sliding sync are handled by a maintained library.
- Per-account isolation (own client, own store, own task) matches the
  "N accounts inside one process" model and prepares for true multi-account.
- Supervision means a transient homeserver outage self-heals without crashing.

**Cons / risks**
- The SDK's persistent store is load-bearing and not in Postgres; backup/restore
  of `sync.data_dir` is an **operational concern for production deployments** —
  losing it forces re-login and permanent loss of historical Megolm session keys
  (UTDs for any message the new device never received a key for). For local dev
  the directory can simply be recreated; for production it must be included in
  backup/restore procedures alongside the Postgres volume. This is addressed in
  the M12 self-hosting docs.
- `SyncService` rebuild-per-restart is slightly heavier than reusing one client,
  but keeps the failure/restart path simple.
- Confirmed against 0.17 source: `subscribe_to_all_room_updates` (the archive
  firehose, used in 3b) and `EncryptionInfo` accessors still need a doc pass when
  3b lands.
- The 60s backoff cap means a sustained homeserver outage triggers a reconnect
  attempt every 60s indefinitely. For long outages this is acceptable noise, but
  a future improvement is a circuit-breaker pattern: pause retries (and surface an
  alert) after a configurable number of consecutive failures, resuming only on a
  health probe or operator signal. Deferred until we have real production
  deployments to reason about failure rates.

## Alternatives considered

- **Low-level `SlidingSync` builder.** More control, far more surface to maintain;
  `SyncService` exists precisely to wrap it.
- **Legacy `Client::sync()`.** Simpler mental model for a backend, but the SDK is
  steering toward sliding sync and `SyncService` gives better lifecycle signals.
- **One shared client for all accounts.** Not supported by the SDK's
  single-session client model; conflicts with per-account crypto stores.
