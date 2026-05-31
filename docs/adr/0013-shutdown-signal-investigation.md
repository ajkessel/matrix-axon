# ADR 0013: Shutdown signal investigation — inherited blocked mask, no code change needed

## Status

Accepted — post-3b investigation.

## Context

After merging 3b, Ctrl-C appeared to hang the axon process on the developer's
machine. The `shutdown_signal()` function — standard tokio `ctrl_c()` future
combined with a SIGTERM watcher via `tokio::select!`, wired into
`axum::serve`'s `with_graceful_shutdown` — never resolved on that machine even
when Ctrl-C was pressed repeatedly.

Three hypotheses were evaluated:

**H1: `with_graceful_shutdown` was draining in-flight HTTP connections.**
Ruled out. The diagnostic probe showed `shutdown_signal()` itself never
resolved, so drain was never reached. The hang was upstream of axum.

**H2: A compiled C dependency (matrix-sdk crypto stack) installed its own
`sigaction(SIGINT)` handler, clobbering tokio's.**
Ruled out. Static analysis of every crate in the dependency tree with a
compiled C component — `aws-lc-sys`, `vodozemac`, `matrix-sdk-crypto`,
`rusqlite` — turned up zero `SIGINT` signal calls in any code path axon
exercises. The one hit in `aws-lc-sys/crypto/console/console.c` is inside
passphrase-prompt logic that is never reached at runtime.

**H3: The signal mask inherited from the developer's shell had SIGINT blocked.**
Confirmed. A diagnostic build that called `pthread_sigmask(SIG_BLOCK, NULL,
&old)` and logged the old set printed `SIGINT blocked=true` on every thread,
from process start, across two distinct runs in the same terminal. Running in a
fresh terminal window printed `SIGINT blocked=false` and Ctrl-C shut the
process down cleanly with the original code, zero changes.

### Why a shell session can have SIGINT blocked

`pthread_sigmask` state is inherited across `fork(2)` and `exec(2)`. If any
ancestor process in the session called `pthread_sigmask(SIG_BLOCK, SIGINT, …)`
and did not restore the mask before spawning children, every descendant
inherits the blocked set. A *blocked* signal is held in the kernel's per-thread
pending queue and is never delivered to any handler — not `SA_SIGACTION`, not
tokio's `ctrl_c()` future, not any userspace code — until the signal is
unblocked. This is distinct from `SIG_IGN` (which discards the signal at
delivery but does not block it).

The developer's session was in this broken state. `kill -INT <pid>` also failed
(INT is signal 2; it was in the blocked set). `kill -TERM <pid>` worked because
SIGTERM (signal 15) was not blocked.

### Why the earlier sigwait "fix" appeared to work

The intermediate experiment on `claude/shutdown-signal-experiment` tried
replacing `ctrl_c()` with a raw `libc::sigwait`. `sigwait(2)` dequeues signals
from the *blocked pending* set — it is specifically designed for threads that
keep a signal blocked and call `sigwait` to consume it synchronously. In the
broken terminal it happened to drain the pending SIGINT because Ctrl-C had
queued a signal into the pending set. This was accidentally correct for the
broken environment only; in a normal terminal with an unblocked SIGINT it would
race against tokio's runtime-installed handler and produce undefined behavior.
It was not a principled fix.

## Decision

No change to axon's shutdown code. The original `with_graceful_shutdown` +
`tokio::signal::ctrl_c()` + SIGTERM combination is correct and idiomatic. The
hang was entirely environmental — the developer's terminal session had SIGINT
blocked in its inherited signal mask, which is unrelated to axon.

The experimental commits (`38dbf8e`, `ad8bf87`, `96cd3e6`) are reverted by
`69286da`, restoring the code to the 3b baseline (`5305573`).

## Optional hardening (not implemented)

A defensive `pthread_sigmask(SIG_UNBLOCK, {SIGINT, SIGTERM, SIGQUIT}, NULL)`
call at the top of `main`, before the tokio runtime is built, would make axon
robust against this class of broken environment. It is deliberately not added
because:

1. The POSIX `signal(7)` spec says signal masks should be clean at process
   start; any tool that leaves them dirty is the bug.
2. Adding libc to `axon-server` for a workaround to a broken shell session
   adds dependency weight for zero production benefit.
3. Adding an `SIG_UNBLOCK` call silences a real operational misconfig rather
   than surfacing it.

If this becomes a recurring pain point it can be added later as a single
one-liner with a doc comment explaining the rationale.

## Consequences

- `axon-server` has no `libc` dependency.
- `shutdown_signal()` is the original tokio implementation. Ctrl-C or SIGTERM
  both resolve it cleanly in a normal terminal.
- The `with_graceful_shutdown` wrapper means in-flight HTTP requests drain
  before the process exits — which is the correct behavior under a container
  orchestrator.
- Developers who see Ctrl-C not working should check their terminal's signal
  mask:
  ```
  python3 -c "import signal; print(signal.pthread_sigmask(signal.SIG_BLOCK, []))"
  ```
  If `2` appears in the output, SIGINT is blocked in that session. Opening a
  new terminal window resolves it.
