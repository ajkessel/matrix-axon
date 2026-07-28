# ADR 0082 — TUI media send (`/send`)

## Context

ADR 0059 (M15, PRs 252/254) adds the write-side media API — staged upload
(`POST …/media/uploads`) then send (`POST …/rooms/{room_id}/send-media`) — but
explicitly scopes out client UX: "no TUI/web UX ... those remain outside M15b."
This ADR is that follow-on client work for `axon-tui`: a `/send <path>
[caption]` slash command with filesystem tab completion, wired to the two new
endpoints, plus real terminal drag-and-drop support.

`axon-tui` has no upload code today (`AxonClient` only has `get_media`, for
downloads) and no bracketed-paste support at all — its input loop only reads
plain `crossterm::Event::Key`. A drag-and-drop today "works" by accident only:
the terminal types the dropped path as a burst of ordinary keystrokes (often
quoted or backslash-escaped), and the TUI appends it character-by-character
with no unquoting.

## Decision

### A new `Command::SendMedia`, not a reuse of `Command::Send`

`Command::Send(String)` already exists and means "send this plain-text
message" (the bare, non-slash input path) — a literal `/send` slash command
does not exist yet. `Command::SendMedia { path: String, caption:
Option<String> }` is added instead of overloading the existing variant, to
keep "plain text send" and "upload and send a file" as distinct dispatch
targets.

### Shared, quote-aware path/caption tokenizer

`/send`'s argument line is split by one tokenizer function used by both
parsing and tab completion (single source of truth, per the root AGENTS.md
"No duplicate code" rule): the first token is the path — either a
`'...'`/`"..."`-quoted run, or characters up to the first unescaped
whitespace with `\ ` / `\\` / `\'` / `\"` unescaped — and everything after,
trimmed, is an optional caption. This matches what terminal emulators
actually produce when a dropped file's path is typed out (quoted, or
backslash-escaped), so a drag-and-drop composes correctly with `/send`
whether the drop happens before or after the command word.

Quote/escape stripping is scoped to this tokenizer only — it is *not* applied
generically to every paste, which would mangle intentionally-quoted plain
message text.

### Filesystem tab completion follows the existing per-command completer shape

`clients/tui/src/app/completion.rs` already has one completer function per
argument-taking command (react, logout, recover, delete, account, verify,
filter, room), each following the same prefix/candidate/cycle shape and
reusing `longest_common_prefix` for prefix-advance before falling back to
Tab/Shift-Tab cycling on full ambiguity. `/send` gets one more: list
`std::fs::read_dir` on the token's parent directory, filter by the partial
basename, and append `/` to directory candidates. No new completion
architecture — this is the same pattern as `/room`, applied to the
filesystem instead of the room list.

### Real bracketed-paste support, not just typed-keystroke tolerance

Rather than leave drag-and-drop as "the terminal types characters, the app
tolerates it," this adds genuine `crossterm` bracketed-paste handling:
`EnableBracketedPaste`/`DisableBracketedPaste` alongside the existing raw-mode
guard, a new input-event enum carrying `Key` and `Paste(String)` variants
through the existing input-thread channel, and a bulk `insert_str` (one
atomic buffer edit) alongside the existing per-character `insert_char`. This
is the same mechanism a well-behaved terminal client is expected to use for
paste in general (and what Claude Code's own CLI relies on for its
drag-and-drop) — treating a drop as a single atomic edit rather than a burst
of hundreds of individual key events, which also means a large dropped path
or a large paste can't accidentally trigger per-keystroke side effects (draft
debounce, completion-state churn) hundreds of times over.

### Client/upload flow reuses the existing off-loop-mutation pattern

Per the root AGENTS.md rule ("never `await` an API call from key handling"),
the upload is a `tokio::spawn`ed task reporting back through the existing
`lifecycle_tx`/`LifecycleOutcome` channel — the same shape
`send_message_to_room` already uses for plain sends — with a new
`LifecycleOutcome::MediaSent` variant. `reply_to`/`thread_root` are picked up
from the existing `pending_reply`/`pending_thread` state exactly as plain
sends do, so `/reply` and `/thread` compose identically for media. `kind`
(image vs. file) and `Content-Type` are inferred from the file extension
client-side — good enough to satisfy the server's `kind=image` ⇒ `image/*`
validation, not attempting to be exhaustive.

No optimistic local echo for the first cut (unlike plain-text send's
temp-id-swap echo): the sent event arrives over `/v1/ws` like any other
mutation. Revisit if upload latency makes this feel laggy in practice.

## Consequences

- `axon-tui` gains its first client-side upload code path and its first
  general paste-handling path (bracketed paste benefits any future paste of
  plain text too, not just `/send`).
- This PR is scoped to `clients/tui` only (root AGENTS.md "Component
  separation") and is written against PR 252/254's contract before they've
  merged; live end-to-end testing depends on a local build of
  `m15b-send-media` until then.
- No thumbnail/preview generation and no encrypted-room-specific client logic
  — matches ADR 0059's own exclusions; the SDK-side attachment path already
  handles encrypted-room upload/encrypted-file metadata transparently.
- Closes the TUI-UX gap ADR 0059 explicitly left open for M15.
