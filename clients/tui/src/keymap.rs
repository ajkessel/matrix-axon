use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::emoji_matches;
use crate::app::{App, Mode, PopupKind, SearchKind};
use crate::command;
use crate::command::HELP_COMMANDS;

impl App {
    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.shortcuts.quit.matches(key) {
            self.should_quit = true;
        } else if self.shortcuts.focus_next.matches(key) {
            self.cycle_focus();
        } else if self.shortcuts.next_room.matches(key) {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.switch_relative_room(1).await;
        } else if self.shortcuts.previous_room.matches(key) {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.switch_relative_room(-1).await;
        } else if self.shortcuts.message_down.matches(key) {
            // Ctrl+J always navigates messages regardless of focus
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.move_selected_message(1);
        } else if self.shortcuts.message_up.matches(key) {
            // Ctrl+K always navigates messages regardless of focus
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.move_selected_message(-1);
        } else {
            match self.mode.clone() {
                Mode::Compose => self.handle_compose_key(key).await,
                Mode::RoomList => self.handle_room_list_key(key).await,
                Mode::MessageList => self.handle_message_list_key(key).await,
                Mode::Search(kind, query) => self.handle_search_key(key, kind, query).await,
                Mode::Editing { event_id } => self.handle_editing_key(key, event_id).await,
                Mode::Reacting { event_id } => self.handle_reacting_key(key, event_id).await,
                Mode::Popup(kind) => self.handle_popup_key(key, kind),
            }
        }
        self.should_quit
    }

    fn handle_popup_key(&mut self, key: KeyEvent, kind: PopupKind) {
        if self.shortcuts.clear_input.matches(key) {
            self.mode = Mode::Compose;
            self.popup_scroll = 0;
            self.help_selection = 0;
        } else if kind == PopupKind::Help {
            self.handle_help_popup_key(key);
        } else if key.code == KeyCode::Up {
            self.popup_scroll = self.popup_scroll.saturating_sub(1);
        } else if key.code == KeyCode::Down {
            self.popup_scroll = self.popup_scroll.saturating_add(1);
        } else if key.code == KeyCode::PageUp || self.shortcuts.message_page_up.matches(key) {
            self.popup_scroll = self.popup_scroll.saturating_sub(8);
        } else if key.code == KeyCode::PageDown || self.shortcuts.message_page_down.matches(key) {
            self.popup_scroll = self.popup_scroll.saturating_add(8);
        }
    }

    fn handle_help_popup_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Up {
            self.help_selection = self
                .help_selection
                .checked_sub(1)
                .unwrap_or_else(|| HELP_COMMANDS.len().saturating_sub(1));
        } else if key.code == KeyCode::Down {
            self.help_selection = (self.help_selection + 1) % HELP_COMMANDS.len().max(1);
        } else if self.shortcuts.submit.matches(key) {
            let command = HELP_COMMANDS
                .get(self.help_selection)
                .unwrap_or(&HELP_COMMANDS[0]);
            self.input.buffer = command.insert_text.to_owned();
            self.input.cursor = self.input.buffer.len();
            self.input.react_tab = None;
            self.show_input_help = false;
            self.mode = Mode::Compose;
            self.popup_scroll = 0;
            self.status = format!("selected command: {}", command.label).into();
        }
    }

    async fn handle_search_key(&mut self, key: KeyEvent, kind: SearchKind, mut query: String) {
        if self.shortcuts.clear_input.matches(key) {
            self.mode = match kind {
                SearchKind::Rooms => Mode::RoomList,
                SearchKind::Messages => Mode::MessageList,
            };
        } else if self.shortcuts.submit.matches(key) {
            self.mode = match kind {
                SearchKind::Rooms => Mode::RoomList,
                SearchKind::Messages => Mode::MessageList,
            };
            match kind {
                SearchKind::Rooms => self.commit_room_search(query).await,
                SearchKind::Messages => self.commit_message_search(query),
            }
        } else if self.shortcuts.backspace.matches(key) || key.code == KeyCode::Delete {
            query.pop();
            self.mode = Mode::Search(kind, query);
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                query.push(ch);
                self.mode = Mode::Search(kind, query);
            }
        }
    }

    async fn handle_room_list_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Up {
            self.switch_relative_room(-1).await;
        } else if key.code == KeyCode::Down {
            self.switch_relative_room(1).await;
        } else if key.code == KeyCode::PageUp || self.shortcuts.message_page_up.matches(key) {
            self.switch_relative_room(-5).await;
        } else if key.code == KeyCode::PageDown || self.shortcuts.message_page_down.matches(key) {
            self.switch_relative_room(5).await;
        } else if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
            self.mode = Mode::Search(SearchKind::Rooms, String::new());
        } else if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_room(&q, true).await;
            }
        } else if key.code == KeyCode::Char('N') && key.modifiers == KeyModifiers::SHIFT {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_room(&q, false).await;
            }
        } else if self.shortcuts.submit.matches(key) || self.shortcuts.clear_input.matches(key) {
            self.mode = Mode::Compose;
        }
    }

    async fn handle_message_list_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Up {
            self.dismiss_input_help();
            self.move_selected_message(-1);
        } else if key.code == KeyCode::Down {
            self.dismiss_input_help();
            self.move_selected_message(1);
        } else if key.code == KeyCode::PageUp || self.shortcuts.message_page_up.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(-1);
        } else if key.code == KeyCode::PageDown || self.shortcuts.message_page_down.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(1);
        } else if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
            self.mode = Mode::Search(SearchKind::Messages, String::new());
        } else if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_message(&q, true);
            }
        } else if key.code == KeyCode::Char('N') && key.modifiers == KeyModifiers::SHIFT {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_message(&q, false);
            }
        } else if self.shortcuts.reply.matches(key) {
            self.dismiss_input_help();
            self.start_reply_to_selected_message();
        } else if self.shortcuts.thread.matches(key) {
            self.dismiss_input_help();
            self.start_thread_from_selected_message();
        } else if self.shortcuts.edit_message.matches(key) {
            self.dismiss_input_help();
            self.start_edit_selected_message();
        } else if self.shortcuts.redact_message.matches(key) {
            self.dismiss_input_help();
            self.redact_selected_message().await;
        } else if self.shortcuts.react_message.matches(key) {
            self.dismiss_input_help();
            self.start_react_to_selected_message();
        } else if self.shortcuts.clear_input.matches(key) {
            self.mode = Mode::Compose;
        }
    }

    async fn handle_compose_key(&mut self, key: KeyEvent) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        if self.shortcuts.edit_previous.matches(key) {
            self.dismiss_input_help();
            self.edit_previous();
        } else if self.shortcuts.edit_next.matches(key) {
            self.dismiss_input_help();
            self.edit_next();
        } else if self.shortcuts.message_page_up.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(-1);
        } else if self.shortcuts.message_page_down.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(1);
        } else if self.shortcuts.submit.matches(key) {
            self.dismiss_input_help();
            let input = self.take_input_for_submit();
            self.handle_command(command::parse(&input)).await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.clear_input_and_selection();
        } else if self.shortcuts.complete.matches(key) {
            self.dismiss_input_help();
            self.complete_input();
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.dismiss_input_help();
                self.insert_char(ch);
            }
        }
    }

    async fn handle_editing_key(&mut self, key: KeyEvent, event_id: String) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        if self.shortcuts.submit.matches(key) {
            self.dismiss_input_help();
            let input = self.take_input_for_submit();
            self.mode = Mode::Compose;
            self.send_edit(&event_id, &input).await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.clear_input_and_selection();
        } else if self.shortcuts.edit_previous.matches(key) {
            self.dismiss_input_help();
            self.edit_previous();
        } else if self.shortcuts.edit_next.matches(key) {
            self.dismiss_input_help();
            self.edit_next();
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.dismiss_input_help();
                self.insert_char(ch);
            }
        }
    }

    async fn handle_reacting_key(&mut self, key: KeyEvent, event_id: String) {
        if self.shortcuts.backspace.matches(key) {
            self.dismiss_input_help();
            self.backspace();
            self.input.react_tab = None;
            self.update_react_status(&event_id);
            return;
        }
        if key.code == KeyCode::Delete {
            self.dismiss_input_help();
            self.delete_forward();
            self.input.react_tab = None;
            self.update_react_status(&event_id);
            return;
        }
        if self.handle_input_navigation_key(key) {
            return;
        }
        if self.shortcuts.submit.matches(key) {
            self.dismiss_input_help();
            let input = self.take_input_for_submit();
            let reaction_key = self.reaction_key_from_input(&input);
            self.mode = Mode::Compose;
            self.send_react(&event_id, &reaction_key).await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.clear_input_and_selection();
        } else if self.shortcuts.complete.matches(key) {
            self.dismiss_input_help();
            self.complete_react_input(&event_id);
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
            self.input.react_tab = None;
            self.update_react_status(&event_id);
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.dismiss_input_help();
                self.insert_char(ch);
                self.input.react_tab = None;
                self.update_react_status(&event_id);
            }
        }
    }

    fn handle_input_navigation_key(&mut self, key: KeyEvent) -> bool {
        if self.shortcuts.cursor_start.matches(key) || matches!(key.code, KeyCode::Home) {
            self.dismiss_input_help();
            self.move_cursor_to_start();
        } else if self.shortcuts.cursor_end.matches(key) || matches!(key.code, KeyCode::End) {
            self.dismiss_input_help();
            self.move_cursor_to_end();
        } else if self.shortcuts.cursor_left.matches(key) {
            self.dismiss_input_help();
            self.move_cursor_left();
        } else if self.shortcuts.cursor_right.matches(key) {
            self.dismiss_input_help();
            self.move_cursor_right();
        } else if self.shortcuts.backspace.matches(key) {
            self.dismiss_input_help();
            self.backspace();
        } else if key.code == KeyCode::Delete {
            self.dismiss_input_help();
            self.delete_forward();
        } else {
            return false;
        }
        true
    }

    fn take_input_for_submit(&mut self) -> String {
        let input = std::mem::take(&mut self.input.buffer);
        self.input.cursor = 0;
        input
    }

    fn reaction_key_from_input(&mut self, input: &str) -> String {
        if let Some(idx) = self.input.react_tab.take() {
            emoji_matches(input)
                .get(idx)
                .map(|e| e.as_str().to_owned())
                .unwrap_or_else(|| input.to_owned())
        } else {
            let matches = emoji_matches(input);
            if matches.len() == 1 {
                matches[0].as_str().to_owned()
            } else {
                input.to_owned()
            }
        }
    }

    fn clear_input_buffer(&mut self) {
        self.dismiss_input_help();
        self.input.buffer.clear();
        self.input.cursor = 0;
    }

    fn clear_input_and_selection(&mut self) {
        self.clear_input_buffer();
        self.input.react_tab = None;
        self.messages.selection = None;
        self.messages.scroll = usize::MAX;
        self.mode = Mode::Compose;
    }

    fn abandon_transient_input_mode(&mut self) {
        if matches!(self.mode, Mode::Editing { .. } | Mode::Reacting { .. }) {
            self.clear_input_buffer();
            self.input.react_tab = None;
            self.mode = Mode::Compose;
        }
    }

    fn cycle_focus(&mut self) {
        if matches!(self.mode, Mode::Editing { .. } | Mode::Reacting { .. }) {
            self.abandon_transient_input_mode();
            return;
        }
        self.mode = match self.mode {
            Mode::Compose => Mode::RoomList,
            Mode::RoomList | Mode::Search(SearchKind::Rooms, _) => Mode::MessageList,
            Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                Mode::Compose
            }
            Mode::Editing { .. } | Mode::Reacting { .. } => Mode::Compose,
        };
    }
}
