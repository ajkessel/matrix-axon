use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap};
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
    const ROOMS_NARROW_WIDTH: u16 = 32;
    const ROOMS_WIDE_MIN: u16 = 44;
    const ROOMS_WIDE_MAX: u16 = 70;
    const WIDE_THRESHOLD: u16 = 90;
    const ROOMS_WIDE_THRESHOLD: u16 = 110;
    const MIN_ROOMS_WIDTH: u16 = 15;

    let show_accounts = app.accounts_panel_visible();
    let show_rooms = app.rooms_panel_visible();
    let total_width = frame.area().width;
    let wide_enough = total_width >= WIDE_THRESHOLD;
    let rooms_wide = total_width >= ROOMS_WIDE_THRESHOLD;
    let accounts_width = app.display.accounts_panel_width;
    let base_rooms_width = if rooms_wide {
        (total_width / 3).clamp(ROOMS_WIDE_MIN, ROOMS_WIDE_MAX)
    } else {
        ROOMS_NARROW_WIDTH
    };
    let rooms_width = (base_rooms_width as i16 + app.display.rooms_panel_width_adj)
        .max(MIN_ROOMS_WIDTH as i16) as u16;

    let (accounts_area, rooms_area, messages_area) = match (show_accounts, show_rooms, wide_enough)
    {
        (true, true, true) => {
            // Three-column: [Accounts][Rooms][Messages]
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(accounts_width),
                    Constraint::Length(rooms_width),
                    Constraint::Min(20),
                ])
                .split(outer[0]);
            (Some(body[0]), Some(body[1]), body[2])
        }
        (true, true, false) => {
            // Narrow: [Accounts stacked on Rooms][Messages]
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
            (Some(left[0]), Some(left[1]), body[1])
        }
        (true, false, _) => {
            // Rooms hidden: [Accounts][Messages]
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(accounts_width), Constraint::Min(20)])
                .split(outer[0]);
            (Some(body[0]), None, body[1])
        }
        (false, true, _) => {
            // No accounts panel: [Rooms][Messages]
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(rooms_width), Constraint::Min(20)])
                .split(outer[0]);
            (None, Some(body[0]), body[1])
        }
        (false, false, _) => {
            // Messages only
            (None, None, outer[0])
        }
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

        let acct_active = app.mode == Mode::AccountList;
        let acct_border = if acct_active {
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
                    .style(
                        Style::default()
                            .fg(app.colors.accounts_foreground)
                            .bg(app.colors.accounts_background),
                    )
                    .title(acct_title)
                    .borders(Borders::ALL)
                    .border_type(if acct_active {
                        BorderType::Double
                    } else {
                        BorderType::Plain
                    })
                    .border_style(acct_border),
            ),
            accounts_area,
        );
    }

    // Room list with account filter
    if let Some(rooms_area) = rooms_area {
        let visible_indices = app.visible_room_indices();
        let show_account_label =
            app.active_account_filter().is_none() && app.accounts_panel_visible();
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
        let rooms_active = app.mode == Mode::RoomList;
        let rooms_border = if rooms_active {
            Style::default()
                .fg(app.colors.selected_room)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.colors.border)
        };
        let rooms_title = if let Mode::Search(SearchKind::Rooms, q) = &app.mode {
            format!("Rooms  Search: {q}")
        } else if app.unread_filter {
            "Rooms (Unread)".to_owned()
        } else {
            "Rooms".to_owned()
        };
        let rooms = List::new(room_items).block(
            Block::default()
                .style(
                    Style::default()
                        .fg(app.colors.rooms_foreground)
                        .bg(app.colors.rooms_background),
                )
                .title(rooms_title.as_str())
                .borders(Borders::ALL)
                .border_type(if rooms_active {
                    BorderType::Double
                } else {
                    BorderType::Plain
                })
                .border_style(rooms_border),
        );
        frame.render_widget(rooms, rooms_area);
    }

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
    let messages_active = app.mode == Mode::MessageList;
    let messages_border = if messages_active {
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
            .style(
                Style::default()
                    .fg(app.colors.messages_foreground)
                    .bg(app.colors.messages_background),
            )
            .title(messages_title.as_str())
            .borders(Borders::ALL)
            .border_type(if messages_active {
                BorderType::Double
            } else {
                BorderType::Plain
            })
            .border_style(messages_border),
    );
    frame.render_widget(messages, messages_area);

    let (command_line, command_title, mut cursor_col) = match &app.mode {
        Mode::Search(kind, q) => {
            let kind_label = match kind {
                SearchKind::Rooms => "Rooms",
                SearchKind::Messages => "Messages",
                SearchKind::Accounts => "Accounts",
            };
            let hint = "  n: next match  N: prev match";
            let q = q.clone();
            let col = 3u16 + q.chars().count() as u16;
            let line = Line::from(vec![
                Span::styled("-> ", Style::default().fg(app.colors.input_hint)),
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
            (line, format!("Search: {kind_label}"), Some(col))
        }
        Mode::LoginPassword { .. } | Mode::RecoveryKey { .. } => {
            let masked = mask_secret_input(&app.input.buffer);
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
            let title = if matches!(app.mode, Mode::LoginPassword { .. }) {
                "Password".to_owned()
            } else {
                "Recovery key".to_owned()
            };
            (line, title, Some(col))
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
            (line, "Confirm logout".to_owned(), None)
        }
        _ => {
            let in_search_list = matches!(
                app.mode,
                Mode::RoomList | Mode::MessageList | Mode::AccountList
            ) && app.last_search.is_some();
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
            let mut spans = vec![
                Span::raw("> "),
                input_text,
                Span::raw("  "),
                Span::styled(
                    entry_status_text(app),
                    Style::default()
                        .fg(app.colors.status)
                        .add_modifier(Modifier::ITALIC),
                ),
            ];
            if in_search_list {
                spans.push(Span::styled(
                    "  n: next match  N: prev match",
                    Style::default().fg(app.colors.input_hint),
                ));
            }
            let line = Line::from(spans);
            let col = if matches!(
                app.mode,
                Mode::Compose | Mode::LoginUsername | Mode::Editing { .. } | Mode::Reacting { .. }
            ) && !(app.show_input_help && app.input.buffer.is_empty())
            {
                Some(2u16 + app.input.buffer[..app.input.cursor].chars().count() as u16)
            } else {
                None
            };
            let title = match &app.mode {
                Mode::RoomList if app.last_search.is_some() => "Search: Rooms".to_owned(),
                Mode::MessageList if app.last_search.is_some() => "Search: Messages".to_owned(),
                Mode::AccountList if app.last_search.is_some() => "Search: Accounts".to_owned(),
                _ => String::new(),
            };
            (line, title, col)
        }
    };
    let input_active = matches!(
        app.mode,
        Mode::Compose
            | Mode::LoginUsername
            | Mode::LoginPassword { .. }
            | Mode::RecoveryKey { .. }
            | Mode::ConfirmLogout { .. }
            | Mode::Editing { .. }
            | Mode::Reacting { .. }
            | Mode::Unreacting { .. }
            | Mode::Search(_, _)
    );
    let input_border = if input_active {
        Style::default()
            .fg(app.colors.selected_room)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.colors.border)
    };
    let input = Paragraph::new(command_line)
        .block(
            Block::default()
                .style(
                    Style::default()
                        .fg(app.colors.input_foreground)
                        .bg(app.colors.input_background),
                )
                .title(command_title)
                .borders(Borders::ALL)
                .border_type(if input_active {
                    BorderType::Double
                } else {
                    BorderType::Plain
                })
                .border_style(input_border),
        )
        .wrap(Wrap { trim: false });
    if app.mode == Mode::Compose {
        if let Some(response) = app.pending_command_response.as_deref() {
            let inner_width = outer[1].width.saturating_sub(2);
            let prefix_width = command_response_prefix_width(app);
            let response_overflows =
                command_response_line_count(response, inner_width, prefix_width)
                    > usize::from(app.display.input_lines);
            if response_overflows {
                app.mode = Mode::Popup(PopupKind::CommandResponse);
                app.popup_scroll = 0;
                cursor_col = None;
            } else {
                app.pending_command_response = None;
            }
        }
    }
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
        let command_response = app.pending_command_response.as_deref().unwrap_or_default();
        let area = if kind == PopupKind::CommandResponse {
            command_response_popup_area(command_response, frame.area())
        } else {
            centered_rect(72, 80, frame.area())
        };
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
            PopupKind::CommandResponse => (
                "Command Response  (Esc to close)",
                wrap_command_response(command_response, area.width.saturating_sub(2))
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
                    .style(Style::default().bg(app.colors.popup_background))
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

fn mask_secret_input(input: &str) -> String {
    "•".repeat(input.chars().count())
}

#[cfg(test)]
mod recovery_tests {
    use super::mask_secret_input;

    #[test]
    fn secret_prompt_masks_every_character() {
        assert_eq!(
            mask_secret_input("secret recovery key"),
            "•••••••••••••••••••"
        );
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

fn centered_size(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
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
                Span::styled(format!("{marker} {:<40}", command.label), style),
                Span::raw(format!("  {}", command.description)),
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

    let auth_line = if app.client.has_bearer_token() {
        "Auth: bearer-token".to_owned()
    } else {
        "Auth: none (insecure, unauthenticated)".to_owned()
    };

    let version = format!(
        "Version: {} ({})",
        env!("CARGO_PKG_VERSION"),
        env!("GIT_HASH"),
    );

    let mut lines = vec![
        format!("Axon server: {}", app.client.base_url()),
        auth_line,
        version,
        conn_line,
        "".to_owned(),
        format!("Rooms loaded: {}", app.rooms.rooms.len()),
        account_filter_line,
        "".to_owned(),
        "Accounts:".to_owned(),
    ];

    if app.accounts.client_visible.is_empty() {
        lines.push("  (none)".to_owned());
    } else {
        for account in &app.accounts.client_visible {
            let state_label = match account.state {
                crate::api::AccountState::Active => "logged in",
                crate::api::AccountState::Deactivated => "logged out",
                crate::api::AccountState::Deleting => "deleting",
            };
            let selected = app.active_account_filter() == Some(account.account_id);
            let marker = if selected { ">" } else { " " };
            let rooms_for_account = app
                .rooms
                .rooms
                .iter()
                .filter(|r| r.account_id == account.account_id)
                .count();
            let duplicate = app
                .accounts
                .client_visible
                .iter()
                .filter(|candidate| candidate.user_id == account.user_id)
                .count()
                > 1;
            let identity = if duplicate {
                format!("{}  [{}]", account.user_id, account.account_id)
            } else {
                account.user_id.clone()
            };
            lines.push(format!(
                "  {marker} {identity}  ({state_label}, {rooms_for_account} rooms)",
            ));
        }
    }

    lines
}

fn room_display_number(visible_position: usize) -> usize {
    visible_position + 1
}

// IMPORTANT: update this function whenever a keyboard shortcut is added or removed.
// The shortcuts listed here should be the ones that are discoverable by users through the UI (e.g. not necessarily every single keybinding, but at least all the ones mentioned in the help text or error messages).
pub(crate) fn popup_shortcuts_lines(shortcuts: &Shortcuts) -> Vec<String> {
    fn kv(key: impl std::fmt::Display, desc: &str) -> String {
        format!("  {:<22}  {}", key, desc)
    }
    vec![
        "Focus:".to_owned(),
        kv(
            shortcuts.focus_next.label(),
            "cycle focus: Input → Accounts → Rooms → Messages",
        ),
        // kv(
        //     shortcuts.focus_prev.label(),
        //     "cycle focus backward: Input → Messages → Rooms → Accounts",
        // ),
        "".to_owned(),
        "Always active:".to_owned(),
        kv(shortcuts.next_room.label(), "next room"),
        kv(shortcuts.previous_room.label(), "previous room"),
        kv(
            shortcuts.next_account.label(),
            "next account (when 2+ accounts logged in)",
        ),
        kv(
            shortcuts.previous_account.label(),
            "previous account (when 2+ accounts logged in)",
        ),
        kv(shortcuts.message_down.label(), "next message"),
        kv(shortcuts.message_up.label(), "previous message"),
        kv(shortcuts.quit.label(), "quit"),
        kv(
            shortcuts.toggle_accounts_panel.label(),
            "show/hide Accounts panel",
        ),
        kv(
            shortcuts.toggle_rooms_panel.label(),
            "show/hide Rooms panel",
        ),
        kv(
            shortcuts.toggle_unread_filter.label(),
            "toggle Rooms filter: show only rooms with unread messages",
        ),
        kv(
            shortcuts.refresh.label(),
            "refresh rooms and redraw (/refresh)",
        ),
        "".to_owned(),
        "Panel resizing:".to_owned(),
        kv(
            "Alt-Left / Alt-Right",
            "narrow / widen focused Accounts or Rooms panel",
        ),
        kv(
            "Alt-Up / Alt-Down",
            "add / remove a line from the message entry pane (Input focus)",
        ),
        "".to_owned(),
        "Room list / Message list focus (Up/Down/PageUp/PageDown navigate):".to_owned(),
        kv(shortcuts.find.label(), "find (search) in focused list"),
        kv("n", "next search match (no wrap)"),
        kv("N", "previous search match (no wrap)"),
        kv("/", "start /command (returns to Input)"),
        // kv(shortcuts.message_page_up.label(), "page up"),
        // kv(shortcuts.message_page_down.label(), "page down"),
        //kv(
        //    format!("Enter or {}", shortcuts.clear_input.label()),
        //    "return to Input",
        //),
        "".to_owned(),
        "Message actions (select a message first with Ctrl-J/K or arrow keys):".to_owned(),
        kv(shortcuts.edit_message.label(), "edit message"),
        kv(shortcuts.redact_message.label(), "redact message"),
        kv(
            shortcuts.react_message.label(),
            "react to message (type emoji name, Tab to cycle, Enter to send)",
        ),
        kv(
            shortcuts.unreact_message.label(),
            "withdraw one of your reactions",
        ),
        kv(shortcuts.reply.label(), "reply (pending API support)"),
        kv(shortcuts.thread.label(), "thread (pending API support)"),
        "".to_owned(),
        "Input:".to_owned(),
        kv(shortcuts.submit.label(), "submit / send"),
        kv(
            shortcuts.clear_input.label(),
            "clear input / cancel / deselect",
        ),
        kv(
            format!("{} / Shift-Tab", shortcuts.complete.label()),
            "complete forward / backward",
        ),
        //kv(shortcuts.backspace.label(), "backspace"),
        //kv("Delete", "delete forward"),
        kv("Ctrl-U", "kill line (erase typed text)"),
        //kv(shortcuts.cursor_start.label(), "cursor to start of line"),
        //kv(shortcuts.cursor_end.label(), "cursor to end of line"),
        //kv(shortcuts.cursor_left.label(), "cursor left"),
        //kv(shortcuts.cursor_right.label(), "cursor right"),
        kv(
            shortcuts.edit_previous.label(),
            "select previous message in timeline",
        ),
        kv(
            shortcuts.edit_next.label(),
            "select next message in timeline",
        ),
    ]
}

pub(crate) fn entry_status_text(app: &App) -> String {
    app.status.text(app.display.debug)
}

fn command_response_prefix_width(app: &App) -> usize {
    let input = if app.show_input_help && app.input.buffer.is_empty() {
        "Type /help or /? for help".to_owned()
    } else {
        mask_login_command(&app.input.buffer)
    };
    4 + Line::from(input).width()
}

fn command_response_line_count(response: &str, width: u16, prefix_width: usize) -> usize {
    let width = usize::from(width);
    if width == 0 {
        return usize::MAX;
    }

    let mut total = 0;
    for (line_index, line) in response.split('\n').enumerate() {
        let mut lines = 1;
        let mut used = if line_index == 0 {
            prefix_width.min(width)
        } else {
            0
        };
        for word in line.split_whitespace() {
            let word_width = Line::from(word).width();
            let separator = usize::from(used > 0);
            if used + separator + word_width <= width {
                used += separator + word_width;
                continue;
            }

            if used > 0 {
                lines += 1;
            }
            lines += word_width.saturating_sub(1) / width;
            used = word_width % width;
            if used == 0 && word_width > 0 {
                used = width;
            }
        }
        total += lines;
    }
    total.max(1)
}

fn wrap_command_response(response: &str, width: u16) -> Vec<String> {
    let width = usize::from(width).max(1);
    let mut wrapped = Vec::new();
    for line in response.split('\n') {
        let mut current = String::new();
        let mut current_width = 0;
        for ch in line.chars() {
            let ch_width = Line::from(ch.to_string()).width();
            if current_width > 0 && current_width + ch_width > width {
                wrapped.push(std::mem::take(&mut current));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        wrapped.push(current);
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

fn command_response_popup_area(response: &str, terminal: Rect) -> Rect {
    const TITLE_WIDTH: u16 = 34;
    const MAX_WIDTH: u16 = 80;

    let available_width = terminal.width.saturating_sub(2).max(1);
    let content_width = response
        .split('\n')
        .map(|line| Line::from(line).width())
        .max()
        .unwrap_or(0)
        .saturating_add(2);
    let width = u16::try_from(content_width)
        .unwrap_or(u16::MAX)
        .clamp(TITLE_WIDTH, MAX_WIDTH)
        .min(available_width);
    let wrapped_height = wrap_command_response(response, width.saturating_sub(2)).len();
    let desired_height = u16::try_from(wrapped_height)
        .unwrap_or(u16::MAX)
        .saturating_add(2);
    let max_height = terminal.height.saturating_mul(4) / 5;
    let height = desired_height.min(max_height.max(3)).min(terminal.height);

    centered_size(width, height, terminal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{EventDto, RoomDto};
    use crate::app::Status;
    use crate::config::TuiConfig;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use uuid::Uuid;

    #[test]
    fn shortcuts_popup_lists_configured_navigation_and_actions() {
        let config = TuiConfig::test_default();
        let text = popup_shortcuts_lines(&config.shortcuts).join("\n");

        assert!(text.contains("F6"));
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
    fn overflowing_command_response_opens_popup() {
        let mut app = App::new(
            crate::api::AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
        );
        let response =
            "This command response is long enough to wrap beyond the one-line entry box.";
        app.show_input_help = false;
        app.status = Status::Info(response.to_owned());
        app.pending_command_response = Some(response.to_owned());
        let mut terminal = Terminal::new(TestBackend::new(40, 20)).expect("terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw succeeds");

        assert_eq!(app.mode, Mode::Popup(PopupKind::CommandResponse));
        assert_eq!(app.pending_command_response.as_deref(), Some(response));
    }

    #[test]
    fn fitting_command_response_stays_in_entry_box() {
        let mut app = App::new(
            crate::api::AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
        );
        app.show_input_help = false;
        app.status = Status::Info("done".to_owned());
        app.pending_command_response = Some("done".to_owned());
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).expect("terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw succeeds");

        assert_eq!(app.mode, Mode::Compose);
        assert!(app.pending_command_response.is_none());
    }

    #[test]
    fn restored_command_input_reduces_available_response_width() {
        let mut app = App::new(
            crate::api::AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
        );
        app.show_input_help = false;
        app.input.buffer = "/recover alice".to_owned();
        app.input.cursor = app.input.buffer.len();
        app.status = Status::Info("recovery failed".to_owned());
        app.pending_command_response = Some("recovery failed".to_owned());
        let mut terminal = Terminal::new(TestBackend::new(30, 20)).expect("terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("draw succeeds");

        assert_eq!(app.mode, Mode::Popup(PopupKind::CommandResponse));
    }

    #[test]
    fn command_response_wrap_count_honors_words_and_newlines() {
        assert_eq!(command_response_line_count("done", 20, 4), 1);
        assert_eq!(command_response_line_count("12345 12345 12345", 10, 4), 3);
        assert_eq!(command_response_line_count("first\nsecond", 20, 4), 2);
    }

    #[test]
    fn command_response_popup_wraps_long_lines_for_scrolling() {
        let lines = wrap_command_response(&"x".repeat(25), 10);

        assert_eq!(lines, vec!["x".repeat(10), "x".repeat(10), "x".repeat(5)]);
    }

    #[test]
    fn command_response_popup_height_fits_short_content() {
        let area = command_response_popup_area("recovery failed", Rect::new(0, 0, 120, 40));

        assert_eq!(area.height, 3);
        assert_eq!(area.width, 34);
        assert_eq!(area.x, 43);
        assert_eq!(area.y, 18);
    }

    #[test]
    fn command_response_popup_is_clamped_for_small_terminals() {
        let area = command_response_popup_area(&"x".repeat(200), Rect::new(0, 0, 30, 10));

        assert_eq!(area.width, 28);
        assert_eq!(area.height, 8);
        assert_eq!(area.x, 1);
        assert_eq!(area.y, 1);
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
            crate::api::AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
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
