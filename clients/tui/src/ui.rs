use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::api::RoomDto;
use crate::app::{format_time, message_display_lines, App, Mode, PopupKind, RoomKey, SearchKind};
use crate::command::HELP_COMMANDS;
use crate::config::Shortcuts;

pub(crate) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let input_box_height = app.display.input_lines + 2; // content lines + top/bottom borders
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(input_box_height)])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(20)])
        .split(outer[0]);

    let room_items = app.rooms.rooms.iter().enumerate().map(|(index, room)| {
        let key = RoomKey::from(room);
        let unread = app.rooms.unread.get(&key).copied().unwrap_or_default();
        let marker = if Some(index) == app.rooms.selected {
            ">"
        } else {
            " "
        };
        let unread = if unread > 0 {
            format!(" ({unread})")
        } else {
            String::new()
        };
        let latest = room
            .last_event_id
            .as_deref()
            .map(|_| format!(" {}", format_time(room.last_activity_ts)))
            .unwrap_or_default();
        let alias = room
            .canonical_alias
            .as_deref()
            .or(room.topic.as_deref())
            .map(|value| format!(" {value}"))
            .unwrap_or_default();
        let media = if room.avatar_url.is_some() {
            " img"
        } else {
            ""
        };
        let title_style = if Some(index) == app.rooms.selected {
            Style::default()
                .fg(app.colors.selected_room)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        ListItem::new(Line::from(vec![
            Span::raw(format!("{marker}{} ", index + 1)),
            Span::styled(room.title().to_owned(), title_style),
            Span::styled(unread, Style::default().fg(app.colors.unread_count)),
            Span::raw(latest),
            Span::raw(media),
            Span::raw(alias),
        ]))
    });
    let rooms_border = if app.mode == Mode::RoomList {
        Style::default()
            .fg(app.colors.selected_room)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.colors.border)
    };
    let rooms_title = if let Mode::Search(SearchKind::Rooms, q) = &app.mode {
        format!("Rooms  Search: {q}")
    } else {
        "Rooms".to_owned()
    };
    let rooms = List::new(room_items).block(
        Block::default()
            .title(rooms_title.as_str())
            .borders(Borders::ALL)
            .border_style(rooms_border),
    );
    frame.render_widget(rooms, body[0]);

    let message_page_size = usize::from(body[1].height.saturating_sub(2)).max(1);
    let message_width = usize::from(body[1].width.saturating_sub(2)).max(1);
    app.set_message_viewport(message_page_size, message_width);
    let selected_events = app.selected_events();
    let sender_labels = selected_events
        .iter()
        .map(|event| app.sender_label(event))
        .collect::<Vec<_>>();
    let reactions = app.selected_reactions();
    let message_rows = message_display_lines(
        selected_events.as_slice(),
        sender_labels.as_slice(),
        app.selected_message_id(),
        &app.colors,
        message_width,
        &reactions,
        &app.live.own_senders,
    );
    let message_scroll = app.messages.scroll.min(message_rows.len());
    let message_lines = message_rows
        .into_iter()
        .skip(message_scroll)
        .take(message_page_size)
        .collect::<Vec<_>>();
    let title = app
        .selected_room()
        .map(RoomDto::title)
        .unwrap_or("No room selected");
    let messages_border = if app.mode == Mode::MessageList {
        Style::default()
            .fg(app.colors.selected_room)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.colors.border)
    };
    let messages_title = if let Mode::Search(SearchKind::Messages, q) = &app.mode {
        format!("{title}  Search: {q}")
    } else {
        title.to_owned()
    };
    let messages = Paragraph::new(message_lines).block(
        Block::default()
            .title(messages_title.as_str())
            .borders(Borders::ALL)
            .border_style(messages_border),
    );
    frame.render_widget(messages, body[1]);

    let (command_line, command_title, cursor_col) = match &app.mode {
        Mode::Search(_, q) => {
            let hint = "  n: next match  N: prev match".to_owned();
            let q = q.clone();
            let col = 2u16 + q.chars().count() as u16;
            let line = Line::from(vec![
                Span::styled("/ ", Style::default().fg(app.colors.input_hint)),
                Span::raw(q),
                Span::raw("  "),
                Span::styled(
                    entry_status_text(app),
                    Style::default()
                        .fg(app.colors.status)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::styled(hint, Style::default().fg(app.colors.input_hint)),
            ]);
            (line, "Search", Some(col))
        }
        _ => {
            let input_text = if app.show_input_help && app.input.buffer.is_empty() {
                Span::styled(
                    "Type /help or /? for help",
                    Style::default()
                        .fg(app.colors.input_hint)
                        .add_modifier(Modifier::ITALIC),
                )
            } else {
                Span::raw(app.input.buffer.clone())
            };
            let line = Line::from(vec![
                Span::raw("> "),
                input_text,
                Span::raw("  "),
                Span::styled(
                    entry_status_text(app),
                    Style::default()
                        .fg(app.colors.status)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]);
            let col = if matches!(
                app.mode,
                Mode::Compose | Mode::Editing { .. } | Mode::Reacting { .. }
            ) && !(app.show_input_help && app.input.buffer.is_empty())
            {
                Some(2u16 + app.input.buffer[..app.input.cursor].chars().count() as u16)
            } else {
                None
            };
            (line, "", col)
        }
    };
    let input_border = if matches!(
        app.mode,
        Mode::Compose
            | Mode::Editing { .. }
            | Mode::Reacting { .. }
            | Mode::Unreacting { .. }
            | Mode::Search(_, _)
    ) {
        Style::default()
            .fg(app.colors.selected_room)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.colors.border)
    };
    let input = Paragraph::new(command_line)
        .block(
            Block::default()
                .title(command_title)
                .borders(Borders::ALL)
                .border_style(input_border),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(input, outer[1]);
    if let Some(col) = cursor_col {
        // col = prefix_len + chars_before_cursor; compute visual row/col for wrapping
        let inner_width = outer[1].width.saturating_sub(2) as usize;
        let (vis_row, vis_col) = if inner_width > 0 && col as usize > inner_width {
            let overflow = col as usize - inner_width;
            (1 + overflow / inner_width, overflow % inner_width)
        } else {
            (0, col as usize)
        };
        frame.set_cursor_position((
            outer[1].x.saturating_add(1).saturating_add(vis_col as u16),
            outer[1].y.saturating_add(1).saturating_add(vis_row as u16),
        ));
    }

    if let Mode::Popup(kind) = app.mode {
        let area = centered_rect(72, 80, frame.area());
        frame.render_widget(Clear, area);
        let page_size = usize::from(area.height.saturating_sub(2)).max(1);
        let (popup_title, lines) = match kind {
            PopupKind::Help => {
                let lines = popup_help_lines(app);
                if app.help_selection < app.popup_scroll {
                    app.popup_scroll = app.help_selection;
                } else if app.help_selection >= app.popup_scroll.saturating_add(page_size) {
                    app.popup_scroll = app
                        .help_selection
                        .saturating_add(1)
                        .saturating_sub(page_size);
                }
                ("Help  (Enter to select, Esc to close)", lines)
            }
            PopupKind::Shortcuts => (
                "Shortcuts  (Esc to close)",
                popup_shortcuts_lines(&app.shortcuts)
                    .into_iter()
                    .map(Line::from)
                    .collect(),
            ),
            PopupKind::RoomInfo => (
                "Room Info  (Esc to close, Up/Down scroll)",
                popup_room_info_lines(app)
                    .into_iter()
                    .map(Line::from)
                    .collect(),
            ),
        };
        let max_scroll = lines.len().saturating_sub(page_size);
        app.popup_scroll = app.popup_scroll.min(max_scroll);
        let visible_lines = lines
            .into_iter()
            .skip(app.popup_scroll)
            .take(page_size)
            .collect::<Vec<_>>();
        let popup = Paragraph::new(visible_lines)
            .block(
                Block::default()
                    .title(popup_title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(app.colors.selected_room)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(popup, area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn popup_help_lines(app: &App) -> Vec<Line<'static>> {
    HELP_COMMANDS
        .iter()
        .enumerate()
        .map(|(index, command)| {
            let marker = if index == app.help_selection {
                ">"
            } else {
                " "
            };
            let style = if index == app.help_selection {
                Style::default()
                    .fg(app.colors.selected_room)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!("{marker} {:<16}", command.label), style),
                Span::raw(command.description),
            ])
        })
        .collect()
}

pub(crate) fn popup_room_info_lines(app: &App) -> Vec<String> {
    let Some(room) = app.selected_room() else {
        return vec!["No room selected.".to_owned()];
    };
    let aliases = room
        .canonical_alias
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| "unavailable (API support needed for alias list)".to_owned());
    let account_user_id = room
        .account_user_id
        .as_deref()
        .unwrap_or("unavailable from room summary");
    let avatar = room.avatar_url.as_deref().unwrap_or("none");
    let topic = room.topic.as_deref().unwrap_or("none");
    let last_event = room.last_event_id.as_deref().unwrap_or("none");
    let mut lines = vec![
        format!("Name: {}", room.title()),
        format!("Matrix ID: {}", room.room_id),
        format!("Account ID: {}", room.account_id),
        format!("Your Matrix ID: {account_user_id}"),
        format!("Aliases: {aliases}"),
        format!("Topic: {topic}"),
        format!("Avatar: {avatar}"),
        format!("Last activity: {}", format_time(room.last_activity_ts)),
        format!("Last event: {last_event}"),
        "Encryption: unavailable (API support needed)".to_owned(),
        "Access: unavailable (API support needed)".to_owned(),
        "Room type/version: unavailable (API support needed)".to_owned(),
        "".to_owned(),
        "Members from loaded timeline:".to_owned(),
    ];

    let members = known_room_members(app);
    if members.is_empty() {
        lines.push("  unavailable (API support needed for complete room members)".to_owned());
    } else {
        lines.extend(members.into_iter().map(|member| {
            let display_name = member
                .display_name
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "unknown".to_owned());
            format!(
                "  {display_name}  {}  ({})",
                member.user_id, member.membership
            )
        }));
        lines.push("".to_owned());
        lines.push(
            "Complete member list requires API support; this list only reflects loaded timeline state."
                .to_owned(),
        );
    }
    lines
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownRoomMember {
    user_id: String,
    display_name: Option<String>,
    membership: String,
}

fn known_room_members(app: &App) -> Vec<KnownRoomMember> {
    let mut by_user = HashMap::<String, KnownRoomMember>::new();
    for member in app
        .selected_raw_events()
        .iter()
        .filter(|event| event.event_type == "m.room.member")
        .filter_map(|event| {
            let user_id = event.state_key().unwrap_or(&event.sender);
            let membership = event.membership_change()?;
            Some(KnownRoomMember {
                user_id: user_id.to_owned(),
                display_name: event.membership_display_name().map(str::to_owned),
                membership,
            })
        })
    {
        by_user.insert(member.user_id.clone(), member);
    }
    let mut members = by_user.into_values().collect::<Vec<_>>();
    members.sort_by(|left, right| {
        left.display_name
            .as_deref()
            .unwrap_or(left.user_id.as_str())
            .to_ascii_lowercase()
            .cmp(
                &right
                    .display_name
                    .as_deref()
                    .unwrap_or(right.user_id.as_str())
                    .to_ascii_lowercase(),
            )
    });
    members
}

pub(crate) fn popup_shortcuts_lines(shortcuts: &Shortcuts) -> Vec<String> {
    vec![
        "Focus:".to_owned(),
        format!(
            "  {}   cycle focus: Input → Rooms → Messages",
            shortcuts.focus_next.label()
        ),
        "".to_owned(),
        "Always active:".to_owned(),
        format!("  {}   next room", shortcuts.next_room.label()),
        format!("  {}   previous room", shortcuts.previous_room.label()),
        format!("  {}   next message", shortcuts.message_down.label()),
        format!("  {}   previous message", shortcuts.message_up.label()),
        format!("  {}   quit", shortcuts.quit.label()),
        "".to_owned(),
        "Room list / Message list focus (Up/Down/PageUp/PageDown navigate):".to_owned(),
        "  /            start search".to_owned(),
        "  n            next search match (no wrap)".to_owned(),
        "  N            previous search match (no wrap)".to_owned(),
        format!("  {}   page up", shortcuts.message_page_up.label()),
        format!("  {}   page down", shortcuts.message_page_down.label()),
        format!(
            "  Enter or {}   return to Input",
            shortcuts.clear_input.label()
        ),
        "".to_owned(),
        "Message actions (select a message first with Ctrl-J/K or arrow keys):".to_owned(),
        format!("  {}   edit message", shortcuts.edit_message.label()),
        format!("  {}   redact message", shortcuts.redact_message.label()),
        format!(
            "  {}   react to message (type emoji name, Tab to cycle, Enter to send)",
            shortcuts.react_message.label()
        ),
        format!(
            "  {}   withdraw one of your reactions",
            shortcuts.unreact_message.label()
        ),
        format!(
            "  {}   reply (pending API support)",
            shortcuts.reply.label()
        ),
        format!(
            "  {}   thread (pending API support)",
            shortcuts.thread.label()
        ),
        "".to_owned(),
        "Input:".to_owned(),
        format!("  {}   submit / send", shortcuts.submit.label()),
        format!(
            "  {}   clear input / cancel / deselect",
            shortcuts.clear_input.label()
        ),
        format!(
            "  {} / Shift-Tab   complete forward / backward",
            shortcuts.complete.label()
        ),
        format!("  {}   backspace", shortcuts.backspace.label()),
        "  Delete       delete forward".to_owned(),
        "  Ctrl-U       kill line (erase typed text)".to_owned(),
        format!(
            "  {}   cursor to start of line",
            shortcuts.cursor_start.label()
        ),
        format!("  {}   cursor to end of line", shortcuts.cursor_end.label()),
        format!("  {}   cursor left", shortcuts.cursor_left.label()),
        format!("  {}   cursor right", shortcuts.cursor_right.label()),
        format!(
            "  {}   edit previous message in timeline",
            shortcuts.edit_previous.label()
        ),
        format!(
            "  {}   edit next message in timeline",
            shortcuts.edit_next.label()
        ),
    ]
}

pub(crate) fn entry_status_text(app: &App) -> String {
    app.status.text(app.display.debug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EventDto, RoomDto};
    use crate::config::TuiConfig;
    use uuid::Uuid;

    #[test]
    fn shortcuts_popup_lists_configured_navigation_and_actions() {
        let config = TuiConfig::test_default();
        let text = popup_shortcuts_lines(&config.shortcuts).join("\n");

        assert!(text.contains("Ctrl-Space"));
        assert!(text.contains("Ctrl-J"));
        assert!(text.contains("Ctrl-K"));
        assert!(text.contains("edit message"));
        assert!(text.contains("react to message"));
        assert!(text.contains("withdraw one of your reactions"));
    }

    #[test]
    fn room_info_popup_lists_summary_and_known_members() {
        let mut app = App::new(
            crate::api::AxonClient::new("http://127.0.0.1:8080".to_owned()),
            None,
            TuiConfig::test_default(),
        );
        let room = RoomDto {
            account_id: Uuid::nil(),
            account_user_id: Some("@me:example.com".to_owned()),
            room_id: "!room:example.com".to_owned(),
            name: Some("Ops".to_owned()),
            topic: Some("Daily operations".to_owned()),
            avatar_url: Some("mxc://example/avatar".to_owned()),
            canonical_alias: Some("#ops:example.com".to_owned()),
            last_activity_ts: 0,
            last_event_id: Some("$last:example.com".to_owned()),
        };
        app.rooms.rooms = vec![room.clone()];
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            crate::app::RoomKey::from(&room),
            vec![EventDto {
                account_id: Uuid::nil(),
                event_id: "$member:example.com".to_owned(),
                room_id: "!room:example.com".to_owned(),
                sender: "@alice:example.com".to_owned(),
                state_key: Some("@alice:example.com".to_owned()),
                origin_ts: 0,
                event_type: "m.room.member".to_owned(),
                content: Some(serde_json::json!({
                    "membership": "join",
                    "displayname": "Alice"
                })),
                body: None,
                relates_to: None,
                redacted: false,
                redaction_event_id: None,
            }],
        );

        let text = popup_room_info_lines(&app).join("\n");

        assert!(text.contains("Name: Ops"));
        assert!(text.contains("Matrix ID: !room:example.com"));
        assert!(text.contains("Aliases: #ops:example.com"));
        assert!(text.contains("Topic: Daily operations"));
        assert!(text.contains("Alice  @alice:example.com  (join)"));
        assert!(text.contains("Encryption: unavailable"));
    }
}
