# axon-tui — Contributor Notes

`axon-tui` is a terminal client for the Axon `/v1/` HTTP + WebSocket API. It is a client, not a Matrix SDK application: do not bypass Axon by talking directly to a homeserver or Matrix SDK from this crate.

## Scope

- Slash commands and keyboard shortcuts should reflect the Axon API surface. Unsupported Matrix actions should report that the current Axon API does not support them yet.
- The client reads accounts, rooms, and timeline history over HTTP, live events over `/v1/ws`, and sends lifecycle and message mutations over HTTP.
- Preserve the future path for reply threading, search, and scrolling back, but do not add server-side assumptions before endpoints exist.

## Terminal UX

- Treat the entry line like a small readline-style prompt: visible cursor, editable text, Tab completion, familiar terminal shortcuts (`Ctrl-A`, `Ctrl-E`, `Ctrl-U`), and `Up`/`Down` navigation through timeline messages for quick editing.
- Treat every multi-step TUI process as one uninterrupted interaction. From the first prompt until completion or cancellation, unrelated asynchronous updates must not replace its entry-line instructions, progress, validation errors, confirmation text, or user input. Background WebSocket, refresh, and timeline statuses may update their underlying state, but must defer visible entry-line status changes until the process ends; only outcomes belonging to the active process may advance its message.
- The layout uses an explicit `Mode` state machine for Accounts (when multiple accounts are active), Room List, Message List, and Input. Ctrl-Tab cycles focus; the focused pane border is highlighted. In list modes, arrow keys navigate items, `/` enters a search sub-mode, and `n`/`N` move to adjacent matches after the search is committed. Editing, reacting, unreacting, search, and popup interactions each have explicit modes.
- Keep keyboard shortcuts configurable through the config layer. When adding a new shortcut: add a default key to `RawConfig::default_values()` and all related structs (`RawShortcuts`, `PartialRawShortcuts`, `Shortcuts`), wire it through `into_shortcuts()`, `merge()`, and `to_toml()`, add it to the `DEFAULT_CONFIG` constant, include it in `popup_shortcuts_lines()`, and write a test.
- Keep slash commands discoverable through `/help`, `/?`, and Tab completion, including commands the TUI knows about but the current Axon API does not support yet. A text entry beginning with `//` sends a message beginning with a literal `/` instead of running a command, and this escape must remain documented in `/help`. `/help` and `/shortcuts` open popup overlays dismissed with `Esc`.
- Keep short slash-command responses in the entry box. When a completed response would exceed the configured entry-box height at the current terminal width, show the full response in a scrollable popup dismissed with `Esc`.
- Keep argument completion consistent with command resolution. `/room` (legacy alias `/switch`) accepts visible-list numbers, room IDs, aliases, display names, and unique prefixes; matching and completion must honor the active account filter. Ambiguous Tab completion advances only to the longest common prefix and blocks Enter until the target identifies one room. `/account` filters active accounts using the panel's displayed numbers (`0` means all accounts), user IDs, or localparts. `/react` completes known emoji names and never sends arbitrary reaction text. `/logout` and `/recover` resolve only active accounts and cycle ambiguous targets with Tab/Shift-Tab; if duplicate rows share one Matrix ID, completion uses the account UUID so either row remains selectable. `/recover` accepts the recovery key only through its masked prompt, never inline. `/delete` uses the same Matrix-ID/localpart targeting and Tab/Shift-Tab cycling, including UUID disambiguation for duplicate rows, but allows both active and deactivated accounts and requires typing `YES` in all caps at the confirmation prompt.
- Login credentials are transient. Normalize `user:domain` and `user@domain` to canonical `@user:domain`, validate with `ruma`, mask prompted and inline passwords, and clear password state on submission, failure, cancellation, or focus changes. Use the same normalization for logout targeting. Never send the password anywhere except Axon's login endpoint, and never talk to a homeserver directly: homeserver discovery is Axon's job (ADR 0023) — login forwards the Matrix ID and password, and `homeserver_url` only when the user supplies the optional `/login` third argument to override resolution (a bare host is given `https://`; an explicit scheme is preserved for loopback). A space-bearing password can't be given inline (the inline password is a single token); such users reach the hidden prompt via `/login` or `/login <user> [homeserver]`, where the username step also accepts an optional homeserver. Axon rejects an MXID written with the homeserver's hostname (`@user:matrix.domain`) with a 400 whose message suggests the canonical Matrix ID; the TUI shows API error messages verbatim, so no client-side handling is needed.
- `/whereami` shows the current room summary from Axon and any members learned from loaded timeline membership events. Do not present that derived member list as complete until Axon exposes a room-info or room-state API with full aliases, members, power levels, encryption, and access settings.
- `/status` uses the cached `GET /v1/accounts` response and lists every client-visible account as `logged in` (`active`) or `logged out` (`deactivated`). Keep the account panel and `/account` navigation active-only.
- Room switching should remain forgiving within the active account filter: visible-list number, room id, canonical alias, display name, and shortened Matrix alias forms should continue to work.

## Media

- Fetch media only through Axon's account-scoped `/v1/media` proxy. Cache keys must include `account_id`; an `mxc://` URL alone is not an Axon resource identity.
- Keep media work demand-driven and bounded. Request only visible thumbnails or an explicitly opened preview, cap response size, bound decoded-image and encoded-protocol caches, and limit concurrent workers.
- Never download, decode, apply EXIF orientation, resize, or encode a terminal image on the input or draw loop. Those operations belong in background work, and late results for evicted entries must be discarded.
- Do not probe terminal image capabilities by reading stdin before launch; unsupported terminals can leave a detached reader that steals keystrokes. Use safe environment hints, an explicit `AXON_IMAGE_PROTOCOL` override, and halfblocks as the fallback.
- Inline images own fixed rows in the message flow. Render a terminal graphic only when its complete reserved region is visible so scrolling cannot place it over neighboring text.
- Keep the larger image view explicit and modal rather than automatically changing the message-pane layout when selection moves.

## Rendering robustness

The terminal is a boundary too — terminal size, content width, and remote media are all adversarial.

- **Budget the whole area before allocating to one element.** Reserve space for captions, borders, status, and prompts first; never let a single element (a tall image preview, a long line) claim the full width/height and squeeze a sibling to zero.
- **Survive degenerate sizes.** Handle a 1-row / 1-column terminal and a very large one; use saturating arithmetic so layout math like `width - n` never underflows.
- **Measure display width, not bytes or `char` count** — grapheme / East-Asian width drives wrapping and truncation (see `wrap.rs`).
- **Every fetch the TUI makes to axon has a timeout, media included.** `api.rs` already buckets HTTP timeouts; any new path (image/media fetch, a worker pool) must adopt the same — a hung fetch must not block input or permanently consume a bounded fetch pool.

## Mutations

Account lifecycle operations map to:

- Login: `POST /v1/accounts/login`
- Logout: `POST /v1/accounts/{account_id}/logout`
- Recover: `POST /v1/accounts/{account_id}/recover`
- Delete: `DELETE /v1/accounts/{account_id}`

Login, logout, and recover return `{ "data": <AccountDto> }`. Recover requires an active account and consumes a transient `recovery_key` without persisting it. Logout is non-destructive: the returned account is `deactivated` and its archive remains available for a later login. Delete returns `204 No Content` and permanently removes the account and its local Axon data.

Login, logout, recover, and delete run off the event loop: secret-bearing calls are spawned as tasks that own and then drop the password or recovery key, and results land back through a channel the main loop drains, so the UI keeps redrawing and a second lifecycle verb is refused while one is in flight. Login is idempotent server-side — an already-`active` account is returned unchanged with the password never consulted — so the client reports that no-op distinctly (`already logged in: … (no changes)`) by comparing the returned `account_id` against the accounts that were active before the attempt. After a real new/reactivated login, prompt for a recovery key only when the returned account is not verified; empty Enter or `Esc` skips. Logout asks for `[y/N]` confirmation unless `display.confirm_logout = false`. Delete always asks for an explicit `YES` confirmation.

Login, logout, recover, and delete follow the general multi-step interaction rule above: while a lifecycle prompt is open or a request is in flight, background statuses must not overwrite the entry line. Lifecycle outcomes may replace their own status when the operation advances or completes.

The four write operations map to these Axon API endpoints:

- Send: `POST /v1/accounts/{account_id}/rooms/{room_id}/send`
- Edit: `PUT /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}`
- Redact: `DELETE /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}`
- React: `POST /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}/reactions`

All return `{ "data": { "event_id": "..." } }`.

`Mode::Editing`, `Mode::Reacting`, and `Mode::Unreacting` own their respective input flows. Redact fires immediately with no pending state. Unreact uses the redaction endpoint against the current user's reaction event ID; when several distinct reactions exist, Tab cycles the choices before Enter confirms.

## Own-message identification

The TUI can seed the user's own Matrix ID from `RoomDto.account_user_id` when the server provides it, so own-message coloring works on first render. Keep that field optional in the client DTO for compatibility with older Axon servers. As a fallback, after `send_message_to_room` succeeds, the TUI stores the returned event_id as `pending_own_event_id`; when the echo arrives via the live WebSocket, it records `account_id → sender` in `own_senders`. Messages from that sender/account pair render with `colors.own_message_sender` instead of `colors.message_sender`.

## Formatted Messages

Matrix `formatted_body` is HTML, not Markdown. Render it only when `content.format == "org.matrix.custom.html"`, sanitize it before display, and keep support deliberately small and terminal-friendly. The current renderer handles common tags such as bold, italic, inline code, links, block quotes, lists, paragraphs, line breaks, and preformatted code blocks, then falls back to plain `body` when formatted content is absent or empty after sanitization. Do not render arbitrary HTML or add browser-like layout behavior.

## Config

- The config file lives at `$XDG_CONFIG_HOME/axon-tui/config.toml`, falling back to `~/.config/axon-tui/config.toml`.
- On first run, create a default config file with all default shortcuts, colors, and display options.
- Existing config files must be backward-compatible. If new default keys are added, load older configs by filling missing defaults and rewrite the repaired file instead of failing startup.
- Every config rewrite must preserve supported settings and user comments. If an unsupported option would otherwise be removed, retain it as a commented-out line with an explanatory comment immediately above it.
- Invalid user-provided key names or color names may remain errors; missing fields should not.

## Verification

For TUI-only changes, run:

```bash
cargo fmt --all --check
cargo test -p axon-tui
cargo clippy -p axon-tui --all-targets --all-features -- -D warnings
```

Broaden to workspace checks when changing workspace dependencies, shared crates, or Axon API contracts.
