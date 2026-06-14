use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::api::RoomDto;
use crate::app::{
    account_localpart, format_time, message_display_lines, AccountSelection, App, Mode, PopupKind,
    RoomKey, SearchKind,
};
use crate::command::HELP_COMMANDS;
use crate::config::Shortcuts;

pub(crate) fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let input_box_height = app.display.input_lines + 2; // content lines + top/bottom borders
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(input_box_height)])
        .split(frame.area());
    const ACCOUNTS_WIDTH: u16 = 25;
    const ROOMS_NARROW_WIDTH: u16 = 32;
    const ROOMS_WIDE_MIN: u16 = 44;
    const ROOMS_WIDE_MAX: u16 = 70;
    const WIDE_THRESHOLD: u16 = 90;
    const ROOMS_WIDE_THRESHOLD: u16 = 110;

    let show_accounts = app.accounts_panel_visible();
    let total_width = frame.area().width;
    let wide_layout = show_accounts && total_width >= WIDE_THRESHOLD;
    let rooms_wide = total_width >= ROOMS_WIDE_THRESHOLD;
    let rooms_width = if rooms_wide {
        (total_width / 3).clamp(ROOMS_WIDE_MIN, ROOMS_WIDE_MAX)
    } else {
        ROOMS_NARROW_WIDTH
    };

    let (accounts_area, rooms_area, messages_area) = if wide_layout {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(ACCOUNTS_WIDTH),
                Constraint::Length(rooms_width),
                Constraint::Min(20),
            ])
            .split(outer[0]);
        (Some(body[0]), body[1], body[2])
    } else if show_accounts {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(rooms_width), Constraint::Min(20)])
            .split(outer[0]);
        let total_acct_items = 1 + app.accounts.accounts.len();
        let acct_height = ((total_acct_items as u16 + 2).min(body[0].height / 3)).max(3);
        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(acct_height), Constraint::Min(1)])
            .split(body[0]);
        (Some(left[0]), left[1], body[1])
    } else {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(rooms_width), Constraint::Min(20)])
            .split(outer[0]);
        (None, body[0], body[1])
    };

    // Accounts panel
    if let Some(accounts_area) = accounts_area {
        let acct_page_size = accounts_area.height.saturating_sub(2).max(1) as usize;
        app.accounts.page_size = acct_page_size;

        let acct_search_query = match &app.mode {
            Mode::Search(SearchKind::Accounts, q) => Some(q.to_lowercase()),
            _ => None,
        };

        let all_acct_entries: Vec<(String, AccountSelection)> = std::iter::once((
            AccountSelection::All.display_label(None),
            AccountSelection::All,
        ))
        .chain(app.accounts.accounts.iter().enumerate().map(|(i, a)| {
            let selection = AccountSelection::Account(i);
            (selection.display_label(Some(&a.user_id)), selection)
        }))
        .filter(|(label, _)| {
            acct_search_query
                .as_ref()
                .is_none_or(|q| label.to_lowercase().contains(q.as_str()))
        })
        .collect();

        let acct_sel_pos = all_acct_entries
            .iter()
            .position(|(_, sel)| *sel == app.accounts.selected)
            .unwrap_or(0);
        let total_acct_items = all_acct_entries.len();

        if acct_sel_pos < app.accounts.scroll {
            app.accounts.scroll = acct_sel_pos;
        } else if acct_page_size > 0 && acct_sel_pos >= app.accounts.scroll + acct_page_size {
            app.accounts.scroll = acct_sel_pos + 1 - acct_page_size;
        }
        let acct_max_scroll = total_acct_items.saturating_sub(acct_page_size);
        app.accounts.scroll = app.accounts.scroll.min(acct_max_scroll);
        let acct_scroll = app.accounts.scroll;

        let acct_items: Vec<ListItem> = all_acct_entries
            .iter()
            .skip(acct_scroll)
            .take(acct_page_size)
            .map(|(label, sel)| {
                let is_sel = app.accounts.selected == *sel;
                let marker = if is_sel { ">" } else { " " };
                let style = if is_sel {
                    Style::default()
                        .fg(app.colors.selected_room)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{marker} ")),
                    Span::styled(label.clone(), style),
                ]))
            })
            .collect();

        let acct_border = if app.mode == Mode::AccountList {
            Style::default()
                .fg(app.colors.selected_room)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.colors.border)
        };
        let acct_title = match &app.mode {
            Mode::Search(SearchKind::Accounts, q) => format!("Accounts  Search: {q}"),
            _ => "Accounts".to_owned(),
        };
        frame.render_widget(
            List::new(acct_items).block(
                Block::default()
                    .title(acct_title)
                    .borders(Borders::ALL)
                    .border_style(acct_border),
            ),
            accounts_area,
        );
    }

    // Room list with account filter
    let visible_indices = app.visible_room_indices();
    let show_account_label = app.active_account_filter().is_none() && app.accounts_panel_visible();
    let rooms_selected_vis = app
        .rooms
        .selected
        .and_then(|sel| visible_indices.iter().position(|&i| i == sel))
        .unwrap_or(0);
    let rows_available = rooms_area.height.saturating_sub(2) as usize;
    let rooms_page_size = if rooms_wide {
        rows_available.max(1)
    } else {
        (rows_available / 2).max(1)
    };
    app.rooms.page_size = rooms_page_size;
    if rooms_selected_vis < app.rooms.scroll {
        app.rooms.scroll = rooms_selected_vis;
    } else if rooms_page_size > 0 && rooms_selected_vis >= app.rooms.scroll + rooms_page_size {
        app.rooms.scroll = rooms_selected_vis + 1 - rooms_page_size;
    }
    let rooms_max_scroll = visible_indices.len().saturating_sub(rooms_page_size);
    app.rooms.scroll = app.rooms.scroll.min(rooms_max_scroll);
    let rooms_scroll = app.rooms.scroll;

    let room_items = visible_indices
        .iter()
        .enumerate()
        .skip(rooms_scroll)
        .take(rooms_page_size)
        .map(|(vis_pos, &full_index)| {
            let room = &app.rooms.rooms[full_index];
            let key = RoomKey::from(room);
            let unread_count = app.rooms.unread.get(&key).copied().unwrap_or_default();
            let is_selected = Some(full_index) == app.rooms.selected;
            let marker = if is_selected { ">" } else { " " };
            let unread_str = if unread_count > 0 {
                format!(" ({unread_count})")
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
            let account_tag = if show_account_label {
                room.account_user_id
                    .as_deref()
                    .map(|uid| {
                        let localpart = account_localpart(uid).unwrap_or(uid);
                        format!(" [{localpart}]")
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let title_style = if is_selected {
                Style::default()
                    .fg(app.colors.selected_room)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            if rooms_wide {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{marker}{} ", room_display_number(vis_pos))),
                    Span::styled(room.title().to_owned(), title_style),
                    Span::raw(account_tag),
                    Span::styled(unread_str, Style::default().fg(app.colors.unread_count)),
                    Span::raw(latest),
                    Span::raw(alias),
                ]))
            } else {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::raw(format!("{marker}{} ", room_display_number(vis_pos))),
                        Span::styled(room.title().to_owned(), title_style),
                        Span::raw(account_tag),
                    ]),
                    Line::from(vec![
                        Span::raw("    "),
                        Span::styled(unread_str, Style::default().fg(app.colors.unread_count)),
                        Span::raw(format!("{latest}{alias}")),
                    ]),
                ])
            }
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
    frame.render_widget(rooms, rooms_area);

    let message_page_size = usize::from(messages_area.height.saturating_sub(2)).max(1);
    let message_width = usize::from(messages_area.width.saturating_sub(2)).max(1);
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
    frame.render_widget(messages, messages_area);

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
        Mode::LoginPassword { .. } => {
            let masked = "•".repeat(app.input.buffer.chars().count());
            let col = 2u16 + app.input.buffer[..app.input.cursor].chars().count() as u16;
            let line = Line::from(vec![
                Span::raw("> "),
                Span::raw(masked),
                Span::raw("  "),
                Span::styled(
                    entry_status_text(app),
                    Style::default()
                        .fg(app.colors.status)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]);
            (line, "Password", Some(col))
        }
        Mode::ConfirmLogout { account } => {
            let line = Line::from(vec![
                Span::raw("> "),
                Span::styled(
                    format!("Log out {}? [y/N]", account.user_id),
                    Style::default()
                        .fg(app.colors.status)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]);
            (line, "Confirm logout", None)
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
                Span::raw(mask_login_command(&app.input.buffer))
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
                Mode::Compose | Mode::LoginUsername | Mode::Editing { .. } | Mode::Reacting { .. }
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
            | Mode::LoginUsername
            | Mode::LoginPassword { .. }
            | Mode::ConfirmLogout { .. }
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
            PopupKind::Status => (
                "Status  (Esc to close)",
                popup_status_lines(app)
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

fn mask_login_command(input: &str) -> String {
    let trimmed = input.trim_start();
    let leading_len = input.len() - trimmed.len();
    let Some(rest) = trimmed.strip_prefix("/login") else {
        return input.to_owned();
    };
    let Some(first_space) = rest.find(char::is_whitespace) else {
        return input.to_owned();
    };
    let after_command = &rest[first_space..];
    let credentials = after_command.trim_start();
    let Some(username_end) = credentials.find(char::is_whitespace) else {
        return input.to_owned();
    };
    let password_start = leading_len + trimmed.len() - credentials.len() + username_end;
    let prefix = &input[..password_start];
    let password = &input[password_start..];
    format!(
        "{prefix}{}",
        password
            .chars()
            .map(|ch| if ch.is_whitespace() { ch } else { '•' })
            .collect::<String>()
    )
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

pub(crate) fn popup_status_lines(app: &App) -> Vec<String> {
    use crate::app::ConnectionState;

    let conn_line = match &app.connection_state {
        ConnectionState::Unknown => "Live WebSocket: not yet connected".to_owned(),
        ConnectionState::Connected => "Live WebSocket: connected".to_owned(),
        ConnectionState::Reconnecting { reason, delay } => {
            format!(
                "Live WebSocket: reconnecting in {}s  ({reason})",
                delay.as_secs()
            )
        }
        ConnectionState::Disconnected(reason) => {
            format!("Live WebSocket: disconnected  ({reason})")
        }
        ConnectionState::ProtocolError(err) => {
            format!("Live WebSocket: protocol error  ({err})")
        }
    };

    let account_filter_line = match app.accounts.selected {
        AccountSelection::All => "Account filter: All Accounts".to_owned(),
        AccountSelection::Account(idx) => {
            let user_id = app
                .accounts
                .accounts
                .get(idx)
                .map(|a| a.user_id.as_str())
                .unwrap_or("?");
            format!("Account filter: {user_id}")
        }
    };

    let mut lines = vec![
        format!("Axon server: {}", app.client.base_url()),
        conn_line,
        "".to_owned(),
        format!("Rooms loaded: {}", app.rooms.rooms.len()),
        account_filter_line,
        "".to_owned(),
        "Accounts:".to_owned(),
    ];

    if app.accounts.accounts.is_empty() {
        lines.push("  (none logged in)".to_owned());
    } else {
        for (idx, account) in app.accounts.accounts.iter().enumerate() {
            let state_label = match account.state {
                crate::api::AccountState::Active => "active",
                crate::api::AccountState::Deactivated => "deactivated",
                crate::api::AccountState::Deleting => "deleting",
            };
            let selected = matches!(
                app.accounts.selected,
                AccountSelection::Account(i) if i == idx
            );
            let marker = if selected { ">" } else { " " };
            let rooms_for_account = app
                .rooms
                .rooms
                .iter()
                .filter(|r| r.account_id == account.account_id)
                .count();
            lines.push(format!(
                "  {marker} {} {}  ({state_label}, {rooms_for_account} rooms)",
                AccountSelection::Account(idx).display_number(),
                account.user_id,
            ));
        }
    }

    lines
}

fn room_display_number(visible_position: usize) -> usize {
    visible_position + 1
}

// IMPORTANT: update this function whenever a keyboard shortcut is added or removed.
pub(crate) fn popup_shortcuts_lines(shortcuts: &Shortcuts) -> Vec<String> {
    vec![
        "Focus:".to_owned(),
        format!(
            "  {}   cycle focus: Input → Accounts → Rooms → Messages (Accounts panel when 2+ accounts)",
            shortcuts.focus_next.label()
        ),
        "".to_owned(),
        "Always active:".to_owned(),
        format!("  {}   next room", shortcuts.next_room.label()),
        format!("  {}   previous room", shortcuts.previous_room.label()),
        format!("  {}   next account (when 2+ accounts logged in)", shortcuts.next_account.label()),
        format!("  {}   previous account (when 2+ accounts logged in)", shortcuts.previous_account.label()),
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
    fn room_numbers_use_absolute_visible_positions() {
        assert_eq!(room_display_number(0), 1);
        assert_eq!(room_display_number(5), 6);
    }

    #[test]
    fn account_localpart_tag_omits_matrix_sigil() {
        assert_eq!(account_localpart("@alice:example.com"), Some("alice"));
    }

    #[test]
    fn masks_inline_login_password_without_hiding_username() {
        assert_eq!(
            mask_login_command("/login @alice:example.com secret phrase"),
            "/login @alice:example.com •••••• ••••••"
        );
        assert_eq!(
            mask_login_command("  /login @alice:example.com secret"),
            "  /login @alice:example.com ••••••"
        );
        assert_eq!(
            mask_login_command("/login @alice:example.com"),
            "/login @alice:example.com"
        );
        assert_eq!(mask_login_command("/logout alice"), "/logout alice");
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
