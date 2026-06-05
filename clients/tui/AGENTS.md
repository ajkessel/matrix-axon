# axon-tui — Contributor Notes

`axon-tui` is a terminal client for the Axon `/v1/` HTTP + WebSocket API. It is a client, not a Matrix SDK application: do not bypass Axon by talking directly to a homeserver or Matrix SDK from this crate.

## Scope

- Slash commands and keyboard shortcuts should reflect the Axon API surface. Unsupported Matrix actions should report that the current Axon API does not support them yet.
- The client reads rooms and timeline history over HTTP, live events over `/v1/ws`, and sends mutations (send, edit, redact, react) over HTTP.
- Preserve the future path for reply threading, search, scrolling back, and terminal media rendering, but do not add server-side assumptions before endpoints exist.

## Terminal UX

- Treat the entry line like a small readline-style prompt: visible cursor, editable text, Tab completion, familiar terminal shortcuts (`Ctrl-A`, `Ctrl-E`, `Ctrl-U`), and `Up`/`Down` navigation through timeline messages for quick editing.
- The three-pane layout (Room List, Message List, Input) uses a `Focus` enum state machine. Ctrl-Space cycles focus; the focused pane border is highlighted. In Room List and Message List focus, arrow keys navigate items, `/` enters a search sub-mode, and `n`/`N` move to adjacent matches.
- Keep keyboard shortcuts configurable through the config layer. When adding a new shortcut: add a default key to `RawConfig::default_values()` and all related structs (`RawShortcuts`, `PartialRawShortcuts`, `Shortcuts`), wire it through `into_shortcuts()`, `merge()`, and `to_toml()`, add it to the `DEFAULT_CONFIG` constant, include it in `popup_shortcuts_lines()`, and write a test.
- Keep slash commands discoverable through `/help`, `/?`, and Tab completion, including commands the TUI knows about but the current Axon API does not support yet. `/help` and `/shortcuts` open popup overlays dismissed with `Esc`.
- `/whereami` shows the current room summary from Axon and any members learned from loaded timeline membership events. Do not present that derived member list as complete until Axon exposes a room-info or room-state API with full aliases, members, power levels, encryption, and access settings.
- Room switching should remain forgiving: list number, room id, canonical alias, display name, and shortened Matrix alias forms should continue to work.

## Mutations

The four write operations map to these Axon API endpoints:

- Send: `POST /v1/accounts/{account_id}/rooms/{room_id}/send`
- Edit: `PUT /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}`
- Redact: `DELETE /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}`
- React: `POST /v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}/reactions`

All return `{ "data": { "event_id": "..." } }`.

A `PendingAction` enum in `App` tracks whether the next `Enter` should send, edit, or react. Redact fires immediately with no pending state.

## Own-message identification

The TUI can seed the user's own Matrix ID from `RoomDto.account_user_id` when the server provides it, so own-message coloring works on first render. Keep that field optional in the client DTO for compatibility with older Axon servers. As a fallback, after `send_message_to_room` succeeds, the TUI stores the returned event_id as `pending_own_event_id`; when the echo arrives via the live WebSocket, it records `account_id → sender` in `own_senders`. Messages from that sender/account pair render with `colors.own_message_sender` instead of `colors.message_sender`.

## Formatted Messages

Matrix `formatted_body` is HTML, not Markdown. Render it only when `content.format == "org.matrix.custom.html"`, sanitize it before display, and keep support deliberately small and terminal-friendly. The current renderer handles common tags such as bold, italic, inline code, links, block quotes, lists, paragraphs, line breaks, and preformatted code blocks, then falls back to plain `body` when formatted content is absent or empty after sanitization. Do not render arbitrary HTML or add browser-like layout behavior.

## Config

- The config file lives at `$XDG_CONFIG_HOME/axon-tui/config.toml`, falling back to `~/.config/axon-tui/config.toml`.
- On first run, create a default config file with all default shortcuts, colors, and display options.
- Existing config files must be backward-compatible. If new default keys are added, load older configs by filling missing defaults and rewrite the repaired file instead of failing startup.
- Invalid user-provided key names or color names may remain errors; missing fields should not.

## Verification

For TUI-only changes, run:

```bash
cargo fmt --all --check
cargo test -p axon-tui
cargo clippy -p axon-tui --all-targets --all-features -- -D warnings
```

Broaden to workspace checks when changing workspace dependencies, shared crates, or Axon API contracts.
