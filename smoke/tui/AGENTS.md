# axon-smoke-tui — Contributor Notes

Black-box PTY smoke harness for the shipped `axon-tui` binary. It spawns the
real binary under a pseudo-terminal, points it at an in-process Axum stub of the
Axon `/v1/` API, and asserts on the rendered terminal screen and the stub's
request journal. See ADR 0025 and `docs/mvp/smoke-testing-plan.md`.

## Hard rule: black-box boundary

This crate must depend on **no `axon-*` product crate** (CI enforces it via
`scripts/check-smoke-isolation.sh`). All wire types are handwritten in `wire.rs`
from the checked-in `openapi/` contract. The harness only ever interacts with
the binary through process spawn, HTTP, WebSocket, and terminal I/O — never by
importing product code. This keeps the harness independently movable across the
planned repository split, and means a green stub run proves process and
rendering behavior, not contract conformance (that is the `live` profile in S2).

## Running

```sh
cargo run -p axon-smoke-tui -- --profile stub [--filter NAME]
```

- `--profile stub` is the only S1 profile. `--filter` is a case-sensitive
  substring match over scenario names and fails if it matches nothing.
- `AXON_TUI_BIN` overrides the binary path (otherwise the runner builds
  `axon-tui` and resolves it under the target dir).
- `SMOKE_TIMEOUT` (seconds) bounds each wait; default 20.

## Layout

| File | Role |
|---|---|
| `main.rs` | arg parsing, profile dispatch, exit code |
| `runner.rs` | sequential runner: run ID, per-scenario isolation, artifacts |
| `pty.rs` | `PtyDriver` — spawn under `portable-pty`, model the screen with `vt100` |
| `stub.rs` | in-process Axum stub + request journal + WS echo broadcast |
| `scenarios.rs` | the S1 scenarios |
| `wire.rs` | handwritten `/v1/` wire types |

## Conventions

- Every wait is condition-based and bounded by a deadline. Eventually-consistent
  observations are polled; failed scenarios are not retried.
- Each scenario gets a fresh stub (ephemeral loopback port) and a fresh
  isolated config/home/working directory, so journals never bleed across
  scenarios and the developer's real `~/.config` is never touched.
- Exit scenarios assert on the alternate-screen leave sequence in the raw
  transcript, so a clean process exit cannot mask a terminal-restoration
  regression.
- On failure, the runner writes the PTY transcript and final rendered screen
  under `smoke-artifacts/tui/<run-id>/<scenario>/` (gitignored, removed after a
  passing run).

## Scenarios (S1)

- `launch_and_quit` — first paint renders the panes; `/quit` exits cleanly.
- `ctrl_c_exit` — the configured Ctrl-C shortcut exits cleanly.
- `send_round_trip` — keystrokes submit a run-marked message, the journal
  records the send, and the WebSocket echo renders in the open room.

Login, navigation, message actions, resilience, and the live-stack journey are
S2 (see the plan).
