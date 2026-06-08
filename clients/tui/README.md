# axon-tui

Terminal client for the Axon API.

## Run

Start Axon first, then run:

```bash
cargo run -p axon-tui -- --base-url http://127.0.0.1:8080
```

Options:

```bash
--base-url URL      Axon server URL, default http://127.0.0.1:8080
--account-id UUID  Optional Axon account filter for the room list
```

## What works today

- Lists rooms returned by `GET /v1/rooms`.
- Shows the latest timeline page for the selected room.
- Appends live events from `/v1/ws` for the selected room.
- Tracks unread counts for live events in other rooms.
- Hides most state events by default, while still showing room membership changes such as joins and leaves.
- Sends messages to the selected room (`POST /v1/.../send`).
- Edits the selected message (`PUT /v1/.../events/{event_id}`).
- Redacts the selected message (`DELETE /v1/.../events/{event_id}`).
- Reacts to the selected message with an emoji (`POST /v1/.../events/{event_id}/reactions`).
- Withdraws the current user's reactions by redacting their reaction events.
- Three-pane focus system: Input, Room List, and Message List, with keyboard navigation and search in each list.
- Own messages appear in a distinct configurable color.
- Renders Matrix `formatted_body` HTML for timeline messages when present, with sanitized support for common inline and block formatting.

## Not Yet Implemented

- Sending replies and threads is waiting on Axon API support.
- Rendering reply/thread relationships is still pending.
- Complete `/whereami` room details, including full alias and member lists, are waiting on Axon API support.

## Commands

Type `/help` or `/?` in the entry line to show a popup with available commands. Type `/shortcuts` to see all keyboard shortcuts. Popups are dismissed with `Esc`.

| Command | Behavior |
| --- | --- |
| plain text | Send a message to the current room. |
| `/switch <room>` | Switch rooms by list number, room id, canonical alias, display name, or shortened alias. |
| `/rooms` | Refresh the room list. |
| `/event <event_id>` | Show a compact status-line summary of one event in the selected account. |
| `/whoami` | Show your Matrix ID and display name for the selected room's account. |
| `/whereami` | Show a room information popup for the selected room. Up/Down/PageUp/PageDown scroll the popup. |
| `/react [emoji]` | React to the selected message, or the most recent displayed message when none is selected. With an emoji or shortcode such as `/react +1`, send immediately; without one, open the selector. |
| `/unreact` | Withdraw one of your reactions from the selected or most recent displayed message. A sole reaction is withdrawn immediately; Tab cycles when several exist. |
| `/reply` | Reply to the selected or most recent displayed message; pending Axon API support. |
| `/thread` | Start a thread from the selected or most recent displayed message; pending Axon API support. |
| `/shortcuts` | Show active keyboard shortcuts from the config file. |
| `/help`, `/?` | Show available slash commands. |
| `/refresh` | Clear and redraw the terminal display. |
| `/quit` | Exit. |
| `/join <room>` | Known command for joining a room; pending Axon API support. |
| `/leave`, `/part` | Known commands for leaving the current room; pending Axon API support. |

Room switching is forgiving. For a room with canonical alias
`#test:example.com`, all of these can match:

```text
/switch 1
/switch test
/switch #test
/switch test:example.com
/switch #test:example.com
```

Use Tab to complete slash commands, `/switch` room names, and emoji names after
`/react`; use Shift-Tab to cycle backward through matching options. When several
rooms match `/switch`, completion advances to their
longest common prefix and lists the remaining suffixes. Enter reports an
ambiguity until the text identifies one room. While Tab completion is partial,
Enter keeps the command open instead of submitting it. A unique Tab match is
replaced with that room's canonical alias or room ID.

## Keyboard Shortcuts

Defaults:

| Shortcut | Behavior |
| --- | --- |
| `Ctrl-Space` | Cycle focus: Input → Room List → Message List. |
| `Ctrl-N` | Next room (always active). |
| `Ctrl-P` | Previous room (always active). |
| `Ctrl-J` | Next displayed message (always active). |
| `Ctrl-K` | Previous displayed message (always active). |
| `Ctrl-C` | Quit. |

When focus is on the **Room List** or **Message List**, the focused pane border is highlighted:

| Shortcut | Behavior |
| --- | --- |
| `Up` / `Down` | Navigate items in the list. |
| `PageUp` / `PageDown` | Page through items. |
| `/` | Start a search; type a query and press `Enter`. |
| `n` | Next search match (no wrap). |
| `N` | Previous search match (no wrap). |
| `Enter` or `Esc` | Return focus to Input. |

When focus is on the **Input** pane:

| Shortcut | Behavior |
| --- | --- |
| `Enter` | Submit the entry line (sends a message or runs a slash command). |
| `Esc` | Clear the entry line and abort any pending action or message selection. |
| `PageUp` / `PageDown` | Page through messages without changing focus. |
| `e` | Edit the selected message (pre-fills the input; `Esc` to cancel). |
| `d` | Redact the selected message immediately. |
| `Shift-R` | React to the selected message: type an emoji name, `Tab` to cycle matches, `Enter` to send. |
| `Shift-U` | Withdraw one of your reactions from the selected message; `Tab` cycles when several exist. |
| `r` | Reply to the selected message (pending Axon API support). |
| `t` | Start a thread (pending Axon API support). |
| `Ctrl-A`, `Home` | Move to start of the entry line. |
| `Ctrl-E`, `End` | Move to end of the entry line. |
| `Left` / `Right` | Move within the entry line. |
| `Up` / `Down` | Select the previous or next timeline message for editing. |
| `Backspace` | Delete before the cursor. |
| `Delete` | Delete after the cursor. |
| `Ctrl-U` | Kill line (erase all typed text). |
| `Tab` | Complete a slash command, room name, or emoji (during reaction entry). |

## Configuration

On first run, `axon-tui` creates a default config file at:

```text
$XDG_CONFIG_HOME/axon-tui/config.toml
```

If `XDG_CONFIG_HOME` is unset, it uses:

```text
~/.config/axon-tui/config.toml
```

The app repairs older config files by adding missing default keys on startup.

Example:

```toml
[shortcuts]
next_room = "ctrl-n"
previous_room = "ctrl-p"
quit = "ctrl-c"
complete = "tab"
submit = "enter"
clear_input = "esc"
backspace = "backspace"
cursor_start = "ctrl-a"
cursor_end = "ctrl-e"
cursor_left = "left"
cursor_right = "right"
edit_previous = "up"
edit_next = "down"
message_down = "ctrl-j"
message_up = "ctrl-k"
message_page_up = "pageup"
message_page_down = "pagedown"
reply = "r"
thread = "t"
edit_message = "e"
redact_message = "d"
react_message = "shift-r"
unreact_message = "shift-u"
focus_next = "ctrl-space"

[colors]
border = "gray"
selected_room = "cyan"
unread_count = "yellow"
message_sender = "green"
own_message_sender = "light-cyan"
input_hint = "dark-gray"
status = "cyan"

[display]
debug = false
show_state_events = false
sender_name = "display_name"
input_lines = 1
```

Supported key forms include `ctrl-n`, `ctrl-j`, `ctrl-k`, `ctrl-space`, `tab`,
`enter`, `esc`, `backspace`, `home`, `end`, `up`, `down`, `left`, `right`,
`pageup`, `pagedown`, `space`, `r`, `t`, `shift-r`, `shift-u`.

Supported color names are `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
`cyan`, `gray`, `dark-gray`, `light-red`, `light-green`, `light-yellow`,
`light-blue`, `light-magenta`, `light-cyan`, and `white`.

Set `display.show_state_events = true` to show all state events in room
timelines. When it is `false`, membership events such as joins, leaves, bans,
and invites are still shown.

Set `display.sender_name = "matrix_address"` to show Matrix user IDs such as
`@alice:example.com` instead of display names. The default is
`"display_name"`, with Matrix IDs used as a fallback when no display name is
known.

Set `display.input_lines` to control the height of the command/entry box.
The default is `1`; set it higher for composing multi-line messages.

Set `display.debug = true` to show Matrix event IDs in the command/entry box
status text. The default is `false`, which hides those event codes.

Set `colors.own_message_sender` to control the color used for the sender label
on messages you sent. Defaults to `"light-cyan"` to distinguish them from other
senders (controlled by `colors.message_sender`).

## Formatted Messages

When an event includes Matrix HTML formatting (`content.format =
"org.matrix.custom.html"` and `content.formatted_body`), the TUI sanitizes it
and renders a small terminal-friendly subset: bold, italic, inline code, links,
block quotes, lists, paragraphs, line breaks, and preformatted code blocks.
Unsupported HTML is stripped, and the TUI falls back to plain `body` if the
formatted content produces no displayable text.
