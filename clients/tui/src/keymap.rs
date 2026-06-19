use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::api::AccountDto;
use crate::app::emoji_matches;
use crate::app::{cycle_index, AccountSelection, App, Mode, PopupKind, SearchKind, Status};
use crate::command;
use crate::command::HELP_COMMANDS;

impl App {
    pub(crate) async fn handle_key(&mut self, key: KeyEvent) -> bool {
        if self.shortcuts.quit.matches(key) {
            self.should_quit = true;
        } else if self.shortcuts.focus_next.matches(key) {
            self.cycle_focus();
        } else if self.shortcuts.focus_prev.matches(key) {
            self.cycle_focus_prev();
        } else if self.shortcuts.next_room.matches(key) {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.switch_relative_room(1).await;
        } else if self.shortcuts.previous_room.matches(key) {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.switch_relative_room(-1).await;
        } else if self.shortcuts.next_account.matches(key) && self.accounts_panel_visible() {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.cycle_account(1);
            self.load_selected_timeline().await;
        } else if self.shortcuts.previous_account.matches(key) && self.accounts_panel_visible() {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.cycle_account(-1);
            self.load_selected_timeline().await;
        } else if self.shortcuts.message_down.matches(key) {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.move_selected_message(1);
            self.mode = Mode::MessageList;
        } else if self.shortcuts.message_up.matches(key) {
            self.dismiss_input_help();
            self.abandon_transient_input_mode();
            self.move_selected_message(-1);
            self.mode = Mode::MessageList;
        } else if self.shortcuts.toggle_accounts_panel.matches(key) && !self.is_mid_command() {
            self.toggle_accounts_panel();
        } else if self.shortcuts.toggle_rooms_panel.matches(key) && !self.is_mid_command() {
            self.toggle_rooms_panel();
        } else if self.shortcuts.toggle_unread_filter.matches(key) && !self.is_mid_command() {
            self.toggle_unread_filter();
        } else if self.shortcuts.refresh.matches(key) && !self.is_mid_command() {
            self.refresh_rooms().await;
            self.redraw_requested = true;
        } else {
            match self.mode.clone() {
                Mode::Compose => self.handle_compose_key(key).await,
                Mode::LoginUsername => self.handle_login_username_key(key).await,
                Mode::LoginPassword {
                    username,
                    homeserver,
                } => {
                    self.handle_login_password_key(key, username, homeserver)
                        .await
                }
                Mode::RecoveryKey { account, origin } => {
                    self.handle_recovery_key(key, account, origin)
                }
                Mode::ConfirmLogout { account } => self.handle_confirm_logout_key(key, account),
                Mode::ConfirmDelete { account } => self.handle_confirm_delete_key(key, account),
                Mode::RoomList => self.handle_room_list_key(key).await,
                Mode::AccountList => self.handle_account_list_key(key).await,
                Mode::MessageList => self.handle_message_list_key(key).await,
                Mode::Search(kind, query) => self.handle_search_key(key, kind, query).await,
                Mode::Editing { event_id } => self.handle_editing_key(key, event_id).await,
                Mode::Reacting { event_id } => self.handle_reacting_key(key, event_id).await,
                Mode::Unreacting {
                    target_event_id,
                    choices,
                    selected,
                } => {
                    self.handle_unreacting_key(key, target_event_id, choices, selected)
                        .await
                }
                Mode::Popup(kind) => self.handle_popup_key(key, kind),
            }
        }
        self.should_quit
    }

    fn handle_popup_key(&mut self, key: KeyEvent, kind: PopupKind) {
        if self.shortcuts.clear_input.matches(key) {
            self.mode = if kind == PopupKind::MediaPreview {
                Mode::MessageList
            } else {
                Mode::Compose
            };
            self.popup_scroll = 0;
            self.help_selection = 0;
            if kind == PopupKind::CommandResponse {
                self.pending_command_response = None;
            }
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
            self.reset_search_list_scroll(&kind);
            self.clear_search_status();
            self.mode = match kind {
                SearchKind::Rooms => Mode::RoomList,
                SearchKind::Messages => Mode::MessageList,
                SearchKind::Accounts => Mode::AccountList,
            };
        } else if self.shortcuts.submit.matches(key) {
            self.reset_search_list_scroll(&kind);
            self.mode = match kind {
                SearchKind::Rooms => Mode::RoomList,
                SearchKind::Messages => Mode::MessageList,
                SearchKind::Accounts => Mode::AccountList,
            };
            match kind {
                SearchKind::Rooms => self.commit_room_search(query).await,
                SearchKind::Messages => self.commit_message_search(query),
                SearchKind::Accounts => {
                    if self.commit_account_search(query) {
                        let search_status =
                            std::mem::replace(&mut self.status, Status::Info(String::new()));
                        self.load_selected_timeline().await;
                        self.status = search_status;
                    }
                }
            }
        } else if self.shortcuts.backspace.matches(key) || key.code == KeyCode::Delete {
            query.pop();
            self.reset_search_list_scroll(&kind);
            self.mode = Mode::Search(kind, query);
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                query.push(ch);
                self.reset_search_list_scroll(&kind);
                self.mode = Mode::Search(kind, query);
            }
        }
    }

    fn reset_search_list_scroll(&mut self, kind: &SearchKind) {
        match kind {
            SearchKind::Rooms => self.rooms.scroll = 0,
            SearchKind::Accounts => self.accounts.scroll = 0,
            SearchKind::Messages => {}
        }
    }

    fn clear_search_status(&mut self) {
        if self.last_search.is_some() {
            self.status = Status::Info(String::new());
        }
    }

    async fn handle_room_list_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Left && key.modifiers == KeyModifiers::ALT {
            self.adjust_rooms_width(-2);
        } else if key.code == KeyCode::Right && key.modifiers == KeyModifiers::ALT {
            self.adjust_rooms_width(2);
        } else if key.code == KeyCode::Up {
            self.switch_relative_room(-1).await;
        } else if key.code == KeyCode::Down {
            self.switch_relative_room(1).await;
        } else if key.code == KeyCode::PageUp || self.shortcuts.message_page_up.matches(key) {
            let page = self.rooms.page_size.max(1) as isize;
            self.switch_relative_room(-page).await;
        } else if key.code == KeyCode::PageDown || self.shortcuts.message_page_down.matches(key) {
            let page = self.rooms.page_size.max(1) as isize;
            self.switch_relative_room(page).await;
        } else if key.code == KeyCode::Home {
            let visible = self.visible_room_indices();
            if let Some(&first) = visible.first() {
                self.rooms.selected = Some(first);
                self.load_selected_timeline().await;
            }
        } else if key.code == KeyCode::End {
            let visible = self.visible_room_indices();
            if let Some(&last) = visible.last() {
                self.rooms.selected = Some(last);
                self.load_selected_timeline().await;
            }
        } else if self.shortcuts.find.matches(key) {
            self.rooms.scroll = 0;
            self.mode = Mode::Search(SearchKind::Rooms, String::new());
        } else if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
            self.clear_input_buffer();
            self.insert_char('/');
            self.mode = Mode::Compose;
        } else if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_room(&q, true).await;
            }
        } else if key.code == KeyCode::Char('N') && key.modifiers == KeyModifiers::SHIFT {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_room(&q, false).await;
            }
        } else if self.shortcuts.submit.matches(key) || self.shortcuts.clear_input.matches(key) {
            self.clear_search_status();
            self.mode = Mode::Compose;
        }
    }

    async fn handle_message_list_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Up || self.shortcuts.message_up.matches(key) {
            self.dismiss_input_help();
            self.move_selected_message(-1);
        } else if key.code == KeyCode::Down || self.shortcuts.message_down.matches(key) {
            self.dismiss_input_help();
            self.move_selected_message(1);
        } else if key.code == KeyCode::PageUp || self.shortcuts.message_page_up.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(-1);
        } else if key.code == KeyCode::PageDown || self.shortcuts.message_page_down.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(1);
        } else if key.code == KeyCode::Home {
            self.dismiss_input_help();
            self.jump_to_first_message();
        } else if key.code == KeyCode::End {
            self.dismiss_input_help();
            self.jump_to_last_message();
        } else if self.shortcuts.find.matches(key) {
            self.mode = Mode::Search(SearchKind::Messages, String::new());
        } else if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
            self.clear_input_buffer();
            self.insert_char('/');
            self.mode = Mode::Compose;
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
        } else if self.shortcuts.unreact_message.matches(key) {
            self.dismiss_input_help();
            self.start_unreact_from_selected_message().await;
        } else if self.shortcuts.media_preview.matches(key) {
            self.dismiss_input_help();
            self.open_selected_media_preview();
        } else if self.shortcuts.clear_input.matches(key) {
            self.clear_search_status();
            self.mode = Mode::Compose;
        }
    }

    async fn handle_compose_key(&mut self, key: KeyEvent) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        if key.code == KeyCode::Up && key.modifiers == KeyModifiers::ALT {
            self.adjust_input_lines(1);
        } else if key.code == KeyCode::Down && key.modifiers == KeyModifiers::ALT {
            self.adjust_input_lines(-1);
        } else if self.shortcuts.edit_previous.matches(key) {
            self.dismiss_input_help();
            self.move_selected_message(-1);
            if self.messages.selection.is_some() {
                self.mode = Mode::MessageList;
            }
        } else if self.shortcuts.edit_next.matches(key) {
            self.dismiss_input_help();
            self.move_selected_message(1);
            if self.messages.selection.is_some() {
                self.mode = Mode::MessageList;
            }
        } else if self.shortcuts.message_page_up.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(-1);
            if self.messages.selection.is_some() {
                self.mode = Mode::MessageList;
            }
        } else if self.shortcuts.message_page_down.matches(key) {
            self.dismiss_input_help();
            self.page_selected_message(1);
            if self.messages.selection.is_some() {
                self.mode = Mode::MessageList;
            }
        } else if self.shortcuts.submit.matches(key) {
            self.dismiss_input_help();
            if let Some(completions) = self.input.partial_room_completions.as_ref() {
                self.status = format!(
                    "room completion is partial: {} - type more or press Tab",
                    completions.join(", ")
                )
                .into();
                return;
            }
            let input = self.take_input_for_submit();
            self.handle_command(command::parse(&input)).await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.clear_input_and_selection();
        } else if self.shortcuts.complete.matches(key) {
            self.dismiss_input_help();
            self.complete_input();
        } else if key.code == KeyCode::BackTab {
            // BackTab is the fixed reverse gesture for the configurable completion key.
            self.dismiss_input_help();
            self.complete_input_reverse();
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.dismiss_input_help();
                self.insert_char(ch);
            }
        }
    }

    async fn handle_login_username_key(&mut self, key: KeyEvent) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        if self.shortcuts.submit.matches(key) {
            self.submit_login_username().await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.cancel_lifecycle_input();
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.insert_char(ch);
            }
        }
    }

    async fn handle_login_password_key(
        &mut self,
        key: KeyEvent,
        username: String,
        homeserver: Option<String>,
    ) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        if self.shortcuts.submit.matches(key) {
            self.submit_login_password(username, homeserver).await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.cancel_lifecycle_input();
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.insert_char(ch);
            }
        }
    }

    fn handle_recovery_key(
        &mut self,
        key: KeyEvent,
        account: AccountDto,
        origin: crate::app::RecoveryOrigin,
    ) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        if self.shortcuts.submit.matches(key) {
            self.submit_recovery_key(account, origin);
        } else if self.shortcuts.clear_input.matches(key) {
            self.cancel_recovery_input(account, origin);
        } else if key.code == KeyCode::Char('u') && key.modifiers == KeyModifiers::CONTROL {
            self.clear_input_buffer();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                self.insert_char(ch);
            }
        }
    }

    fn handle_confirm_logout_key(&mut self, key: KeyEvent, account: AccountDto) {
        // Safe default: only an explicit "y" confirms; "n", Esc, or the
        // clear-input shortcut cancel; every other key is ignored so a stray
        // press can't log anyone out.
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.perform_logout(account);
        } else if matches!(key.code, KeyCode::Char('n') | KeyCode::Char('N'))
            || self.shortcuts.clear_input.matches(key)
        {
            self.cancel_logout_confirmation();
        }
    }

    fn handle_confirm_delete_key(&mut self, key: KeyEvent, account: AccountDto) {
        if self.handle_input_navigation_key(key) {
            return;
        }
        // "YES" confirms; "yes" (wrong case) clears the buffer and stays in
        // this mode with a hint so the user can retry; anything else cancels.
        if self.shortcuts.submit.matches(key) {
            let input = self.input.buffer.trim().to_owned();
            if input == "YES" {
                self.perform_delete(account);
            } else if input.eq_ignore_ascii_case("yes") {
                self.clear_input_buffer();
                self.status = Status::Info("type YES in all caps to confirm".to_owned());
                self.mode = Mode::ConfirmDelete { account };
            } else {
                self.cancel_delete_confirmation();
            }
        } else if self.shortcuts.clear_input.matches(key) {
            self.cancel_delete_confirmation();
        } else if key.code == KeyCode::Backspace {
            self.backspace();
        } else if let KeyCode::Char(ch) = key.code {
            if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
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
            let input = self.input.buffer.clone();
            let Some(reaction_key) = self.take_reaction_key(&input) else {
                self.input.react_tab = None;
                self.update_react_status(&event_id);
                return;
            };
            self.take_input_for_submit();
            self.mode = Mode::Compose;
            self.send_react(&event_id, &reaction_key).await;
        } else if self.shortcuts.clear_input.matches(key) {
            self.clear_input_and_selection();
        } else if self.shortcuts.complete.matches(key) {
            self.dismiss_input_help();
            self.complete_react_input(false);
        } else if key.code == KeyCode::BackTab {
            // BackTab is the fixed reverse gesture for the configurable completion key.
            self.dismiss_input_help();
            self.complete_react_input(true);
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

    async fn handle_unreacting_key(
        &mut self,
        key: KeyEvent,
        target_event_id: String,
        choices: Vec<crate::app::OwnReaction>,
        selected: usize,
    ) {
        if self.shortcuts.clear_input.matches(key) {
            self.mode = Mode::Compose;
            self.status = "unreact canceled".into();
        } else if self.shortcuts.complete.matches(key) || key.code == KeyCode::BackTab {
            // BackTab is the fixed reverse gesture for the configurable completion key.
            let next = cycle_index(selected, choices.len(), key.code == KeyCode::BackTab);
            self.status = crate::app::unreact_selection_status(&choices, next);
            self.mode = Mode::Unreacting {
                target_event_id,
                choices,
                selected: next,
            };
        } else if self.shortcuts.submit.matches(key) {
            let reaction = choices[selected].clone();
            self.mode = Mode::Compose;
            self.withdraw_reaction(reaction).await;
        }
    }

    fn handle_input_navigation_key(&mut self, key: KeyEvent) -> bool {
        if self.shortcuts.cursor_start.matches(key) || matches!(key.code, KeyCode::Home) {
            self.dismiss_input_help();
            self.move_cursor_to_start();
        } else if self.shortcuts.cursor_end.matches(key) || matches!(key.code, KeyCode::End) {
            self.dismiss_input_help();
            self.move_cursor_to_end();
        } else if key.code == KeyCode::Left && key.modifiers == KeyModifiers::CONTROL {
            self.dismiss_input_help();
            self.move_cursor_word_left();
        } else if key.code == KeyCode::Right && key.modifiers == KeyModifiers::CONTROL {
            self.dismiss_input_help();
            self.move_cursor_word_right();
        } else if self.shortcuts.cursor_left.matches(key) {
            self.dismiss_input_help();
            self.move_cursor_left();
        } else if self.shortcuts.cursor_right.matches(key) {
            self.dismiss_input_help();
            self.move_cursor_right();
        } else if key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Char('w') | KeyCode::Backspace)
        {
            self.dismiss_input_help();
            self.delete_word_back();
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

    pub(crate) fn take_input_for_submit(&mut self) -> String {
        let input = std::mem::take(&mut self.input.buffer);
        self.input.cursor = 0;
        self.input.react_command_completion = None;
        self.input.partial_room_completions = None;
        self.input.room_command_completion = None;
        self.input.logout_command_completion = None;
        self.input.recover_command_completion = None;
        self.input.delete_command_completion = None;
        self.input.account_command_completion = None;
        input
    }

    pub(crate) fn take_reaction_key(&mut self, input: &str) -> Option<String> {
        let input = input.trim();
        if let Some(emoji) = emojis::get(input).or_else(|| emojis::get_by_shortcode(input)) {
            self.input.react_tab = None;
            return Some(emoji.as_str().to_owned());
        }
        if let Some(idx) = self.input.react_tab.take() {
            return emoji_matches(input).get(idx).map(|e| e.as_str().to_owned());
        }
        let matches = emoji_matches(input);
        match matches.as_slice() {
            [single] => Some(single.as_str().to_owned()),
            _ => None,
        }
    }

    pub(crate) fn clear_input_buffer(&mut self) {
        self.dismiss_input_help();
        self.input.buffer.clear();
        self.input.cursor = 0;
        self.input.react_command_completion = None;
        self.input.partial_room_completions = None;
        self.input.room_command_completion = None;
        self.input.logout_command_completion = None;
        self.input.recover_command_completion = None;
        self.input.delete_command_completion = None;
        self.input.account_command_completion = None;
    }

    fn clear_input_and_selection(&mut self) {
        self.clear_input_buffer();
        self.input.react_tab = None;
        self.messages.selection = None;
        self.messages.scroll = usize::MAX;
        self.status = Status::Info(String::new());
        self.mode = Mode::Compose;
    }

    fn abandon_transient_input_mode(&mut self) {
        if matches!(
            self.mode,
            Mode::LoginUsername
                | Mode::LoginPassword { .. }
                | Mode::RecoveryKey { .. }
                | Mode::ConfirmLogout { .. }
                | Mode::ConfirmDelete { .. }
                | Mode::Editing { .. }
                | Mode::Reacting { .. }
                | Mode::Unreacting { .. }
        ) {
            self.clear_input_buffer();
            self.input.react_tab = None;
            self.mode = Mode::Compose;
        }
    }

    async fn handle_account_list_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Left && key.modifiers == KeyModifiers::ALT {
            self.adjust_accounts_width(-2);
        } else if key.code == KeyCode::Right && key.modifiers == KeyModifiers::ALT {
            self.adjust_accounts_width(2);
        } else if key.code == KeyCode::Up {
            self.accounts.selected = match self.accounts.selected {
                AccountSelection::All => AccountSelection::All,
                AccountSelection::Account(0) => AccountSelection::All,
                AccountSelection::Account(i) => AccountSelection::Account(i - 1),
            };
            self.sync_room_selection_to_account_filter();
            self.load_selected_timeline().await;
        } else if key.code == KeyCode::Down {
            self.accounts.selected = match self.accounts.selected {
                AccountSelection::All if self.accounts.accounts.is_empty() => AccountSelection::All,
                AccountSelection::All => AccountSelection::Account(0),
                AccountSelection::Account(i) => {
                    AccountSelection::Account((i + 1).min(self.accounts.accounts.len() - 1))
                }
            };
            self.sync_room_selection_to_account_filter();
            self.load_selected_timeline().await;
        } else if key.code == KeyCode::PageUp || self.shortcuts.message_page_up.matches(key) {
            let page = self.accounts.page_size.max(1) as isize;
            self.cycle_account(-page);
            self.load_selected_timeline().await;
        } else if key.code == KeyCode::PageDown || self.shortcuts.message_page_down.matches(key) {
            let page = self.accounts.page_size.max(1) as isize;
            self.cycle_account(page);
            self.load_selected_timeline().await;
        } else if key.code == KeyCode::Home {
            self.accounts.selected = AccountSelection::All;
            self.sync_room_selection_to_account_filter();
            self.load_selected_timeline().await;
        } else if key.code == KeyCode::End {
            let n = self.accounts.accounts.len();
            if n > 0 {
                self.accounts.selected = AccountSelection::Account(n - 1);
                self.sync_room_selection_to_account_filter();
                self.load_selected_timeline().await;
            }
        } else if self.shortcuts.find.matches(key) {
            self.accounts.scroll = 0;
            self.mode = Mode::Search(SearchKind::Accounts, String::new());
        } else if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
            self.clear_input_buffer();
            self.insert_char('/');
            self.mode = Mode::Compose;
        } else if key.code == KeyCode::Char('n') && key.modifiers.is_empty() {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_account(&q, true);
                let search_status =
                    std::mem::replace(&mut self.status, Status::Info(String::new()));
                self.load_selected_timeline().await;
                self.status = search_status;
            }
        } else if key.code == KeyCode::Char('N') && key.modifiers == KeyModifiers::SHIFT {
            if let Some(q) = self.last_search.clone() {
                self.search_adjacent_account(&q, false);
                let search_status =
                    std::mem::replace(&mut self.status, Status::Info(String::new()));
                self.load_selected_timeline().await;
                self.status = search_status;
            }
        } else if self.shortcuts.submit.matches(key) || self.shortcuts.clear_input.matches(key) {
            self.clear_search_status();
            self.mode = Mode::Compose;
        }
    }

    fn cycle_focus(&mut self) {
        if matches!(
            self.mode,
            Mode::LoginUsername
                | Mode::LoginPassword { .. }
                | Mode::RecoveryKey { .. }
                | Mode::ConfirmLogout { .. }
                | Mode::ConfirmDelete { .. }
                | Mode::Editing { .. }
                | Mode::Reacting { .. }
                | Mode::Unreacting { .. }
        ) {
            self.abandon_transient_input_mode();
            return;
        }
        let show_accounts = self.accounts_panel_visible();
        let show_rooms = self.rooms_panel_visible();
        let next = match (show_accounts, show_rooms) {
            (true, true) => match self.mode {
                Mode::Compose => Mode::AccountList,
                Mode::AccountList | Mode::Search(SearchKind::Accounts, _) => Mode::RoomList,
                Mode::RoomList | Mode::Search(SearchKind::Rooms, _) => Mode::MessageList,
                Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                    Mode::Compose
                }
                _ => Mode::Compose,
            },
            (true, false) => match self.mode {
                Mode::Compose => Mode::AccountList,
                Mode::AccountList | Mode::Search(SearchKind::Accounts, _) => Mode::MessageList,
                Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                    Mode::Compose
                }
                _ => Mode::Compose,
            },
            (false, true) => match self.mode {
                Mode::Compose => Mode::RoomList,
                Mode::RoomList | Mode::Search(SearchKind::Rooms, _) => Mode::MessageList,
                Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                    Mode::Compose
                }
                _ => Mode::Compose,
            },
            (false, false) => match self.mode {
                Mode::Compose | Mode::Popup(_) => Mode::MessageList,
                _ => Mode::Compose,
            },
        };
        match next {
            Mode::RoomList => self.rooms.scroll = 0,
            Mode::AccountList => self.accounts.scroll = 0,
            _ => {}
        }
        self.mode = next;
    }

    fn cycle_focus_prev(&mut self) {
        if matches!(
            self.mode,
            Mode::LoginUsername
                | Mode::LoginPassword { .. }
                | Mode::RecoveryKey { .. }
                | Mode::ConfirmLogout { .. }
                | Mode::ConfirmDelete { .. }
                | Mode::Editing { .. }
                | Mode::Reacting { .. }
                | Mode::Unreacting { .. }
        ) {
            self.abandon_transient_input_mode();
            return;
        }
        let show_accounts = self.accounts_panel_visible();
        let show_rooms = self.rooms_panel_visible();
        let prev = match (show_accounts, show_rooms) {
            (true, true) => match self.mode {
                Mode::Compose => Mode::MessageList,
                Mode::AccountList | Mode::Search(SearchKind::Accounts, _) => Mode::Compose,
                Mode::RoomList | Mode::Search(SearchKind::Rooms, _) => Mode::AccountList,
                Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                    Mode::RoomList
                }
                _ => Mode::Compose,
            },
            (true, false) => match self.mode {
                Mode::Compose => Mode::MessageList,
                Mode::AccountList | Mode::Search(SearchKind::Accounts, _) => Mode::Compose,
                Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                    Mode::AccountList
                }
                _ => Mode::Compose,
            },
            (false, true) => match self.mode {
                Mode::Compose => Mode::MessageList,
                Mode::RoomList | Mode::Search(SearchKind::Rooms, _) => Mode::Compose,
                Mode::MessageList | Mode::Search(SearchKind::Messages, _) | Mode::Popup(_) => {
                    Mode::RoomList
                }
                _ => Mode::Compose,
            },
            (false, false) => match self.mode {
                Mode::Compose | Mode::Popup(_) => Mode::MessageList,
                _ => Mode::Compose,
            },
        };
        match prev {
            Mode::RoomList => self.rooms.scroll = 0,
            Mode::AccountList => self.accounts.scroll = 0,
            _ => {}
        }
        self.mode = prev;
    }
}
