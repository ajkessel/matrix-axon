# ADR 0025 — S1 smoke testing implementation

## Status

Proposed — S1.

## Context

Axon has strong logic-level and component coverage, but it does not yet have a
PR-blocking test that starts either shipped binary and observes it through its
real external boundary:

- `axon-server` is not currently booted as a process against real Postgres and
  Synapse and exercised through HTTP and `/v1/ws`.
- `axon-tui` is not currently run in a pseudo-terminal, driven with keystrokes,
  and asserted through its rendered terminal screen.

`docs/mvp/smoke-testing-plan.md` defines four smoke-testing milestones. S1 is the
first useful signal: catch failures in server boot/migrations and TUI first
paint, while covering one inbound and one outbound server journey and one TUI
send journey. E2EE remains covered by `scripts/integration-test.sh`; duplicating
that deeper fixture is not an S1 goal.

S1 is substantial enough that landing both harnesses, Docker orchestration,
Matrix provisioning, PTY support, and CI in one PR would produce an unnecessarily
large review. The implementation also needs to preserve the planned repository
split boundary: server and TUI smoke harnesses must be independently movable and
must not import product internals.

One detail in the original plan needs to be made explicit before implementation.
It describes a peer inviting the Axon account to an unencrypted room, but Axon
does not yet expose a join or invite-acceptance endpoint. An invite alone
therefore does not establish the joined-room precondition required for inbound
and outbound message testing.

## Decision

### Two independent black-box Rust harnesses

Add two Rust binary packages as workspace members:

- `smoke/server`, package `axon-server-smoke`
- `smoke/tui`, package `axon-tui-smoke`

They depend on neither product `axon-*` crates nor each other. They may use
workspace-pinned third-party dependencies, but all public API DTOs and protocol
frames used by the harnesses are handwritten from the checked-in API contract.
The harnesses interact with shipped binaries only through process spawning,
HTTP, WebSocket, and terminal input/output.

CI enforces the boundary by inspecting each smoke package's normal, build, and
development dependency graph and rejecting any product `axon-*` package after
excluding the smoke package itself.

Each package is a sequential scenario runner rather than a `#[test]` suite:

```sh
cargo run -p axon-server-smoke -- --profile local [--filter NAME]
cargo run -p axon-tui-smoke    -- --profile stub  [--filter NAME]
```

`--filter` performs case-sensitive substring matching against stable scenario
names and fails when it matches nothing. Every wait is condition-based and
bounded by a deadline. Eventually consistent observations are polled; failed
scenarios are not retried.

### Land S1 in two PRs

S1 is delivered in this order:

1. **TUI smoke:** the deterministic Axum stub, PTY driver, TUI scenarios, and an
   initial Ubuntu smoke workflow running the TUI suite.
2. **Server smoke and final gate:** the managed Postgres/Synapse environment,
   server scenarios, failure artifacts, and expansion of the same workflow into
   the complete S1 gate.

The stable CI job name is `smoke`. After the second PR lands, that job is made a
required branch-protection check.

### PR 1: TUI stub and PTY smoke

`axon-tui-smoke` uses `portable-pty` to spawn the real `axon-tui` binary and
`vt100` to model a fixed-size terminal screen. It runs with an isolated working
directory and `XDG_CONFIG_HOME`, plus pinned `TERM` and locale values. The
driver exposes process spawn, key input, bounded screen predicates, screen text,
termination, and exit-status observation.

`AXON_TUI_BIN` may override the binary path. Otherwise the runner resolves the
workspace binary and builds it when needed.

An in-process Axum server binds an ephemeral loopback port and implements only
the S1 TUI contract:

- `GET /v1/accounts`
- `GET /v1/rooms`
- `GET /v1/accounts/{account_id}/rooms/{room_id}/timeline`
- `POST /v1/accounts/{account_id}/rooms/{room_id}/send`
- `/v1/ws`

The stub returns complete handwritten wire-compatible JSON, records requests in
a journal, and broadcasts a run-marked `timeline.event` echo after a successful
send. Login and the remaining mutation endpoints are deferred to S2.

S1 TUI scenarios are:

- `launch_and_quit`: the initial room, message, panes, status/input line render,
  and `/quit` exits successfully.
- `ctrl_c_exit`: the configured Ctrl-C key exits successfully.
- `send_round_trip`: keystrokes submit a run-marked message, the request journal
  records the expected send request, and the WebSocket echo renders.

The terminal transcript is retained for diagnostics, while assertions use the
parsed terminal screen rather than raw ANSI output. Exit scenarios also require
the alternate-screen leave sequence so a successful process exit cannot hide a
terminal-restoration regression.

### PR 2: managed local server smoke

`axon-server-smoke --profile local` owns its environment:

- Start the existing Compose Postgres and Synapse services under a unique
  Compose project name.
- Honor `POSTGRES_PORT`, `SYNAPSE_PORT`, `KEEP_UP`, `SMOKE_TIMEOUT`, and
  `AXON_SERVER_BIN`.
- Create a uniquely named empty database and isolated Axon data directory.
- Require Synapse to advertise MSC4186 before starting Axon.
- Start the real `axon` binary from a throwaway directory so repository `.env`
  files cannot affect the run.

A small Synapse provisioning adapter uses process execution for shared-secret
registration and the Matrix Client-Server API for fixture setup. It registers a
target Axon user and a peer, logs both in for provisioning, creates an
unencrypted room, and ensures both users are joined before Axon performs the
login under test. The target user's provisioning access token is then
invalidated so the test does not leave an unrelated live device session.

This joined-room provisioning replaces the plan's literal invite-only wording.
It establishes a test prerequisite through the homeserver without adding a
product join endpoint or importing Matrix SDK/product code into the harness.

S1 server scenarios are sequential, with prerequisite helpers so each can also
run under `--filter`:

- `boot_health`: Axon runs migrations on the empty database and `/healthz`
  returns `{"status":"ok"}`.
- `login`: `POST /v1/accounts/login` returns an active account that appears in
  account-list and by-ID reads.
- `inbound_timeline_ws`: connect `/v1/ws` before the peer sends a marked
  message, then verify the room list, timeline, event lookup, and matching
  `timeline.event` frame.
- `outbound_send`: send through Axon's public mutation endpoint, assert its
  response envelope, then observe the exact event ID and body through the
  peer's Matrix `/sync`.
- `graceful_shutdown`: send SIGTERM and require a zero exit within the deadline.
  Forced termination is cleanup after a failed assertion, never success.

### CI, artifacts, and secrets

`.github/workflows/smoke.yml` contains one required Ubuntu job named `smoke`.
The first PR runs the TUI suite. The second PR builds both shipped binaries and
smoke packages, runs both dependency guards, then runs TUI smoke followed by
server smoke. Existing lint/unit and E2EE integration workflows remain
unchanged.

Each run mints a run ID used in identities, database names, transaction IDs, and
message bodies. Failure artifacts go under
`smoke-artifacts/<suite>/<run-id>` and include, as applicable:

- Axon and Compose log tails
- a redacted HTTP request/response journal
- captured WebSocket frames
- the PTY transcript and final rendered screen

Artifacts are removed after successful local runs and ignored by Git. Passwords,
access tokens, recovery keys, and authorization headers are redacted before any
artifact is written.

### S1 acceptance criteria

S1 is complete when the Ubuntu `smoke` job runs both real binaries and proves:

- empty-database server boot and migrations
- health and runtime login
- inbound persistence through room list, timeline, event lookup, and WebSocket
- outbound send observed by a Matrix peer
- TUI first paint
- TUI send request plus rendered live echo
- clean TUI terminal teardown and graceful server process shutdown

The job must fail when the server exits during migrations/startup or when the
TUI exits or panics before its first draw.

## Alternatives considered

1. **One S1 PR:** gives one atomic feature branch, but combines two new packages,
   Docker/Matrix orchestration, PTY behavior, and CI into a review surface too
   large to validate comfortably.
2. **Three S1 PRs (TUI, server, CI):** produces smaller diffs, but leaves the
   server harness landed without its intended gate and adds coordination without
   enough review benefit over two PRs.
3. **A shared smoke support crate:** reduces some runner duplication, but couples
   server and TUI harnesses across the repository-split boundary and makes either
   package harder to move independently.
4. **Extend `scripts/integration-test.sh`:** reuses existing orchestration, but
   that script is intentionally an E2EE re-decryption test with direct SQL
   assertions. Expanding it into HTTP, WebSocket, and PTY journeys would mix
   distinct responsibilities and retain shell as the main harness language.
5. **Invite the target user and wait for Axon to accept:** impossible with the
   current product API; invite acceptance would require new product behavior
   outside S1's test-infrastructure scope.

## Consequences

- S1 adds a required Docker-backed Ubuntu job, increasing PR latency and image
  download cost. Cargo and Docker caching should be tuned only after real timing
  data exists.
- The two harnesses remain black-box and independently movable, at the cost of
  handwritten wire types and limited duplication in runner/reporting code.
- The TUI suite is deterministic and cheap enough to become cross-platform in
  S3 because its server is an in-process stub.
- The server suite validates real process boot, networking, Matrix sync, and
  public contracts without duplicating the existing E2EE fixture.
- Synapse-specific registration and fixture setup stay behind a small adapter so
  S4 can add Dendrite without changing scenario logic.
- Full mutations, logout/re-login, pagination, error contracts, restart
  persistence, TUI lifecycle/navigation/actions/resilience, and the live TUI
  journey remain S2 work. Native Windows/macOS TUI jobs remain S3, and attached
  external environments remain S4.
