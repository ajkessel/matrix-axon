use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, UNIX_EPOCH};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use uuid::Uuid;

use crate::api::{AxonClient, EventDto, LiveFrame, RoomDto};
use crate::command::{Command, SlashCommand, SLASH_COMMANDS};
use crate::config::{ColorScheme, DisplayOptions, SenderNameStyle, Shortcuts, TuiConfig};
use crate::html::formatted_message_body_lines;
use crate::wrap::{plain_rich_lines, rich_lines_to_spans, wrap_rich_lines};

const TIMELINE_LIMIT: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Compose,
    RoomList,
    MessageList,
    Search(SearchKind, String),
    Editing { event_id: String },
    Reacting { event_id: String },
    Popup(PopupKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchKind {
    Rooms,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PopupKind {
    Help,
    Shortcuts,
    RoomInfo,
}

#[derive(Debug, Clone)]
pub(crate) enum Status {
    Info(String),
    Debug(String),
    EventAction {
        debug: String,
        redacted: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveFrameAction {
    None,
    RefreshRooms,
}

impl Status {
    pub(crate) fn text(&self, debug_enabled: bool) -> String {
        match self {
            Self::Info(text) => text.clone(),
            Self::Debug(text) => {
                if debug_enabled {
                    text.clone()
                } else {
                    String::new()
                }
            }
            Self::EventAction { debug, redacted } => {
                if debug_enabled {
                    debug.clone()
                } else {
                    (*redacted).to_owned()
                }
            }
        }
    }
}

impl From<String> for Status {
    fn from(value: String) -> Self {
        Self::Info(value)
    }
}

impl From<&str> for Status {
    fn from(value: &str) -> Self {
        Self::Info(value.to_owned())
    }
}

impl PartialEq<&str> for Status {
    fn eq(&self, other: &&str) -> bool {
        self.text(true) == *other || self.text(false) == *other
    }
}

impl PartialEq<Status> for &str {
    fn eq(&self, other: &Status) -> bool {
        other == self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RoomKey {
    pub(crate) account_id: Uuid,
    pub(crate) room_id: String,
}

impl From<&RoomDto> for RoomKey {
    fn from(room: &RoomDto) -> Self {
        Self {
            account_id: room.account_id,
            room_id: room.room_id.clone(),
        }
    }
}

pub(crate) struct App {
    pub(crate) client: AxonClient,
    pub(crate) account_filter: Option<Uuid>,
    pub(crate) shortcuts: Shortcuts,
    pub(crate) colors: ColorScheme,
    pub(crate) display: DisplayOptions,
    pub(crate) rooms: RoomsState,
    pub(crate) messages: MessagePane,
    pub(crate) input: InputState,
    pub(crate) live: LiveState,
    pub(crate) mode: Mode,
    pub(crate) popup_scroll: usize,
    pub(crate) help_selection: usize,
    pub(crate) last_search: Option<String>,
    pub(crate) show_input_help: bool,
    pub(crate) status: Status,
    pub(crate) should_quit: bool,
    redraw_requested: bool,
}

#[derive(Default)]
pub(crate) struct RoomsState {
    pub(crate) rooms: Vec<RoomDto>,
    pub(crate) selected: Option<usize>,
    pub(crate) display_names: HashMap<RoomKey, HashMap<String, String>>,
    pub(crate) unread: HashMap<RoomKey, usize>,
}

pub(crate) struct MessagePane {
    pub(crate) events: HashMap<RoomKey, Vec<EventDto>>,
    pub(crate) selection: Option<String>,
    pub(crate) scroll: usize,
    pub(crate) page_size: usize,
    pub(crate) width: usize,
}

impl Default for MessagePane {
    fn default() -> Self {
        Self {
            events: HashMap::new(),
            selection: None,
            scroll: usize::MAX,
            page_size: 1,
            width: 80,
        }
    }
}

#[derive(Default)]
pub(crate) struct InputState {
    pub(crate) buffer: String,
    pub(crate) cursor: usize,
    pub(crate) react_tab: Option<usize>,
}

#[derive(Default)]
pub(crate) struct LiveState {
    pub(crate) own_senders: HashMap<Uuid, String>,
    pub(crate) pending_own_event_id: Option<String>,
}

impl App {
    pub(crate) fn new(client: AxonClient, account_filter: Option<Uuid>, config: TuiConfig) -> Self {
        let config_status = if config.created_default {
            format!("created default config at {}", config.path.display())
        } else {
            "connecting to Axon".to_owned()
        };
        Self {
            client,
            account_filter,
            shortcuts: config.shortcuts,
            colors: config.colors,
            display: config.display,
            rooms: RoomsState::default(),
            messages: MessagePane::default(),
            input: InputState::default(),
            live: LiveState::default(),
            mode: Mode::Compose,
            popup_scroll: 0,
            help_selection: 0,
            last_search: None,
            show_input_help: true,
            status: Status::Info(config_status),
            should_quit: false,
            redraw_requested: false,
        }
    }

    pub(crate) fn take_redraw_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw_requested)
    }

    pub(crate) async fn refresh_rooms(&mut self) {
        match self.client.list_rooms(self.account_filter).await {
            Ok(rooms) => {
                self.apply_room_refresh(rooms);
            }
            Err(err) => {
                self.status = Status::from(format!("room refresh failed: {err}"));
            }
        }
    }

    pub(crate) fn apply_room_refresh(&mut self, rooms: Vec<RoomDto>) {
        let selected_key = self.selected_room().map(RoomKey::from);
        self.rooms.rooms = rooms;
        self.rooms.unread.retain(|key, _| {
            self.rooms
                .rooms
                .iter()
                .any(|room| RoomKey::from(room) == *key)
        });
        self.rooms.selected = selected_key
            .and_then(|key| {
                self.rooms
                    .rooms
                    .iter()
                    .position(|room| RoomKey::from(room) == key)
            })
            .or_else(|| {
                self.rooms
                    .selected
                    .filter(|index| *index < self.rooms.rooms.len())
            });
        self.seed_own_senders_from_rooms();
        if self.rooms.rooms.is_empty() {
            self.rooms.selected = None;
            self.status = Status::from("no rooms returned by Axon".to_owned());
        } else if self.rooms.selected.is_none() {
            self.rooms.selected = Some(0);
            self.status = Status::from(format!("loaded {} rooms", self.rooms.rooms.len()));
        } else {
            self.status = Status::from(format!("refreshed {} rooms", self.rooms.rooms.len()));
        }
    }

    pub(crate) fn seed_own_senders_from_rooms(&mut self) {
        self.live
            .own_senders
            .extend(self.rooms.rooms.iter().filter_map(|room| {
                room.account_user_id
                    .as_ref()
                    .map(|user_id| (room.account_id, user_id.clone()))
            }));
    }

    pub(crate) async fn load_selected_timeline(&mut self) {
        let Some(room) = self.selected_room().cloned() else {
            return;
        };
        self.messages.selection = None;
        self.messages.scroll = usize::MAX;
        match self
            .client
            .room_timeline(room.account_id, &room.room_id, None, TIMELINE_LIMIT)
            .await
        {
            Ok(mut page) => {
                page.events.reverse();
                apply_edits(&mut page.events);
                let has_more = page.next_cursor.is_some();
                self.rebuild_display_names(&room, &page.events);
                self.messages
                    .events
                    .insert(RoomKey::from(&room), page.events);
                self.rooms.unread.remove(&RoomKey::from(&room));
                self.status = Status::Info(if has_more {
                    format!("showing {} (older history available later)", room.title())
                } else {
                    format!("showing {}", room.title())
                });
            }
            Err(err) => {
                self.status = Status::from(format!("timeline load failed: {err}"));
            }
        }
    }

    pub(crate) fn dismiss_input_help(&mut self) {
        self.show_input_help = false;
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        self.input.buffer.insert(self.input.cursor, ch);
        self.input.cursor += ch.len_utf8();
    }

    pub(crate) fn backspace(&mut self) {
        if self.input.cursor == 0 {
            return;
        }
        let previous = self.input.buffer[..self.input.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.input
            .buffer
            .replace_range(previous..self.input.cursor, "");
        self.input.cursor = previous;
    }

    pub(crate) fn delete_forward(&mut self) {
        if self.input.cursor >= self.input.buffer.len() {
            return;
        }
        let next = self.input.buffer[self.input.cursor..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| self.input.cursor + i)
            .unwrap_or(self.input.buffer.len());
        self.input.buffer.replace_range(self.input.cursor..next, "");
    }

    pub(crate) fn move_cursor_to_start(&mut self) {
        self.input.cursor = 0;
    }

    pub(crate) fn move_cursor_to_end(&mut self) {
        self.input.cursor = self.input.buffer.len();
    }

    pub(crate) fn move_cursor_left(&mut self) {
        if self.input.cursor == 0 {
            return;
        }
        self.input.cursor = self.input.buffer[..self.input.cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0);
    }

    pub(crate) fn move_cursor_right(&mut self) {
        if self.input.cursor >= self.input.buffer.len() {
            return;
        }
        self.input.cursor += self.input.buffer[self.input.cursor..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(0);
    }

    pub(crate) fn edit_previous(&mut self) {
        let (target, event_id, body) = {
            let events = self.selected_events();
            if events.is_empty() {
                return;
            }
            let current_pos = self
                .messages
                .selection
                .as_deref()
                .and_then(|id| events.iter().position(|e| e.event_id == id));
            let target = match current_pos {
                None => events.len() - 1,
                Some(0) => return,
                Some(pos) => pos - 1,
            };
            (
                target,
                events[target].event_id.clone(),
                events[target].display_body(),
            )
        };
        self.messages.selection = Some(event_id.clone());
        self.input.buffer = body;
        self.move_cursor_to_end();
        self.mode = Mode::Editing {
            event_id: event_id.clone(),
        };
        self.status = Status::EventAction {
            debug: format!("editing {} - Esc to cancel", event_id),
            redacted: "editing message - Esc to cancel",
        };
        self.ensure_message_index_visible(target);
    }

    pub(crate) fn edit_next(&mut self) {
        let result = {
            let events = self.selected_events();
            let current_pos = self
                .messages
                .selection
                .as_deref()
                .and_then(|id| events.iter().position(|e| e.event_id == id));
            let Some(pos) = current_pos else {
                return;
            };
            if pos + 1 >= events.len() {
                None
            } else {
                let target = pos + 1;
                Some((
                    target,
                    events[target].event_id.clone(),
                    events[target].display_body(),
                ))
            }
        };
        match result {
            None => {
                self.input.buffer.clear();
                self.input.cursor = 0;
                self.messages.selection = None;
                self.mode = Mode::Compose;
            }
            Some((target, event_id, body)) => {
                self.messages.selection = Some(event_id.clone());
                self.input.buffer = body;
                self.move_cursor_to_end();
                self.mode = Mode::Editing {
                    event_id: event_id.clone(),
                };
                self.status = Status::EventAction {
                    debug: format!("editing {} - Esc to cancel", event_id),
                    redacted: "editing message - Esc to cancel",
                };
                self.ensure_message_index_visible(target);
            }
        }
    }

    pub(crate) async fn handle_command(&mut self, command: Command) {
        match command {
            Command::Switch(target) => self.switch_room(&target).await,
            Command::Rooms => {
                self.refresh_rooms().await;
            }
            Command::Event(event_id) => self.show_event(&event_id).await,
            Command::Whoami => self.show_whoami(),
            Command::Whereami => self.show_whereami(),
            Command::Help => self.open_popup(PopupKind::Help),
            Command::Shortcuts => self.open_popup(PopupKind::Shortcuts),
            Command::Refresh => {
                self.redraw_requested = true;
                self.status = Status::Info("display refresh requested".to_owned());
            }
            Command::Quit => self.should_quit = true,
            Command::Send(body) => self.send_message_to_room(&body).await,
            Command::Invalid(message)
            | Command::ApiUnsupported(message)
            | Command::Unknown(message) => {
                self.status = Status::Info(message);
            }
            Command::Empty => {}
        }
    }

    fn open_popup(&mut self, kind: PopupKind) {
        self.popup_scroll = 0;
        if kind == PopupKind::Help {
            self.help_selection = 0;
        }
        self.mode = Mode::Popup(kind);
    }

    fn show_whereami(&mut self) {
        if self.selected_room().is_none() {
            self.status = Status::Info("select a room before using /whereami".to_owned());
            return;
        }
        self.open_popup(PopupKind::RoomInfo);
    }

    fn show_whoami(&mut self) {
        let Some(room) = self.selected_room() else {
            self.status = Status::Info("select a room before using /whoami".to_owned());
            return;
        };
        let Some(user_id) = room.account_user_id.as_deref() else {
            self.status = Status::Info("current user is unavailable for this room".to_owned());
            return;
        };

        let key = RoomKey::from(room);
        let display_name = self
            .rooms
            .display_names
            .get(&key)
            .and_then(|names| names.get(user_id))
            .filter(|name| !name.trim().is_empty())
            .map(String::as_str)
            .unwrap_or("unknown");
        self.status = Status::Info(format!(
            "Matrix ID: {user_id}; Display Name: {display_name}"
        ));
    }

    async fn switch_room(&mut self, target: &str) {
        let Some(index) = self.find_room(target) else {
            self.status = Status::from(format!("room not found: {target}"));
            return;
        };
        self.rooms.selected = Some(index);
        self.load_selected_timeline().await;
    }

    pub(crate) async fn switch_relative_room(&mut self, offset: isize) {
        if self.rooms.rooms.is_empty() {
            self.status = Status::from("no rooms to switch".to_owned());
            return;
        }
        let current = self.rooms.selected.unwrap_or(0);
        let len = self.rooms.rooms.len();
        let next = relative_room_index(current, len, offset);
        self.rooms.selected = Some(next);
        self.load_selected_timeline().await;
    }

    async fn show_event(&mut self, event_id: &str) {
        let Some(room) = self.selected_room() else {
            self.status = Status::from("select a room before using /event".to_owned());
            return;
        };
        match self.client.get_event(room.account_id, event_id).await {
            Ok(event) => {
                let sender = self.sender_label(&event);
                let relation = if event.relates_to.is_some() {
                    " related"
                } else {
                    ""
                };
                let redaction = event
                    .redaction_event_id
                    .as_deref()
                    .map(|id| format!(" redacted_by={id}"))
                    .unwrap_or_default();
                self.status = Status::Info(format!(
                    "{} {} {} {}{}{}",
                    format_time(event.origin_ts),
                    sender,
                    event.event_id,
                    display_body_with_sender(&event, &sender)
                        .chars()
                        .take(120)
                        .collect::<String>(),
                    relation,
                    redaction
                ));
            }
            Err(err) => self.status = Status::Info(format!("event read failed: {err}")),
        }
    }

    pub(crate) fn complete_input(&mut self) {
        if self.complete_command_input() {
            return;
        }
        self.complete_switch_input();
    }

    pub(crate) fn complete_command_input(&mut self) -> bool {
        let Some(prefix) = slash_command_prefix(&self.input.buffer) else {
            return false;
        };
        let candidates = slash_command_candidates(prefix);
        match candidates.as_slice() {
            [] => {
                self.status = Status::from(format!("no command matches: /{prefix}"));
            }
            [command] => {
                self.input.buffer = if command.takes_argument {
                    format!("{} ", command.name)
                } else {
                    command.name.to_owned()
                };
                self.move_cursor_to_end();
                self.status = Status::from(format!("completed command: {}", command.name));
            }
            _ => {
                self.status = Status::Info(format!(
                    "command matches: {}",
                    candidates
                        .iter()
                        .map(|command| command.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        true
    }

    pub(crate) fn complete_switch_input(&mut self) {
        let Some(target) = switch_target_prefix(&self.input.buffer) else {
            return;
        };
        let candidates = self.switch_completion_candidates(target);
        match candidates.as_slice() {
            [] => self.status = Status::Info(format!("no room matches: {target}")),
            [completion] => {
                self.input.buffer = format!("/switch {completion}");
                self.move_cursor_to_end();
                self.status = Status::from(format!("completed room: {completion}"));
            }
            _ => {
                let shown = candidates
                    .iter()
                    .take(5)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = candidates.len().saturating_sub(5);
                self.status = Status::Info(if more == 0 {
                    format!("room matches: {shown}")
                } else {
                    format!("room matches: {shown}, +{more} more")
                });
            }
        }
    }

    fn switch_completion_candidates(&self, target: &str) -> Vec<String> {
        self.rooms
            .rooms
            .iter()
            .filter(|room| room_matches_completion(room, target))
            .map(room_completion_value)
            .collect()
    }

    pub(crate) fn find_room(&self, target: &str) -> Option<usize> {
        if let Ok(index) = target.parse::<usize>() {
            return index
                .checked_sub(1)
                .filter(|index| *index < self.rooms.rooms.len());
        }
        let target_lower = target.to_lowercase();
        if let Some(index) = self.rooms.rooms.iter().position(|room| {
            room.room_id == target
                || room.canonical_alias.as_deref() == Some(target)
                || room.name.as_deref().map(str::to_lowercase).as_deref()
                    == Some(target_lower.as_str())
        }) {
            return Some(index);
        }

        if let Some(alias) = room_alias_with_hash(target) {
            if let Some(index) = self
                .rooms
                .rooms
                .iter()
                .position(|room| room.canonical_alias.as_deref() == Some(alias.as_str()))
            {
                return Some(index);
            }
        }

        let target_local = incomplete_matrix_room_name(target)?;
        self.rooms.rooms.iter().position(|room| {
            room.canonical_alias
                .as_deref()
                .and_then(matrix_room_local_name)
                .is_some_and(|local| local.eq_ignore_ascii_case(target_local))
                || matrix_room_local_name(&room.room_id)
                    .is_some_and(|local| local.eq_ignore_ascii_case(target_local))
        })
    }

    pub(crate) fn handle_live_frame(&mut self, frame: LiveFrame) -> LiveFrameAction {
        match frame {
            LiveFrame::Connected => {
                self.status = Status::Debug("live WebSocket connected".to_owned());
                LiveFrameAction::None
            }
            LiveFrame::Reconnecting { reason, delay } => {
                self.status = Status::Info(format!(
                    "live WebSocket reconnecting in {}s: {reason}",
                    delay.as_secs()
                ));
                LiveFrameAction::None
            }
            LiveFrame::Disconnected(reason) => {
                self.status = Status::Debug(format!("live WebSocket disconnected: {reason}"));
                LiveFrameAction::None
            }
            LiveFrame::ProtocolError(err) => {
                self.status = Status::Debug(format!("ignored malformed live frame: {err}"));
                LiveFrameAction::None
            }
            LiveFrame::Timeline(event) => self.append_live_event(*event),
        }
    }

    fn append_live_event(&mut self, event: EventDto) -> LiveFrameAction {
        let key = RoomKey {
            account_id: event.account_id,
            room_id: event.room_id.clone(),
        };
        if let Some((target_id, new_body)) = event.edit_relation() {
            let target_id = target_id.to_owned();
            let new_body = new_body.to_owned();
            if let Some(events) = self.messages.events.get_mut(&key) {
                if let Some(e) = events.iter_mut().find(|e| e.event_id == target_id) {
                    e.body = Some(new_body);
                }
            }
            return LiveFrameAction::None;
        }
        let known_room = self
            .rooms
            .rooms
            .iter()
            .any(|room| RoomKey::from(room) == key);
        if self
            .selected_room()
            .is_some_and(|room| RoomKey::from(room) == key)
        {
            let visible_before = self.selected_display_line_count();
            let old_scroll_bottom = visible_before.saturating_sub(self.messages.page_size);
            let should_follow_tail =
                self.messages.scroll == usize::MAX || self.messages.scroll >= old_scroll_bottom;
            let should_select =
                self.messages.selection.is_none() && should_show_event(&event, &self.display);
            let event_id = event.event_id.clone();
            if self.live.pending_own_event_id.as_deref() == Some(&event_id) {
                self.live
                    .own_senders
                    .insert(event.account_id, event.sender.clone());
                self.live.pending_own_event_id = None;
            }
            self.remember_display_name_from_event(&key, &event);
            self.messages
                .events
                .entry(key.clone())
                .or_default()
                .push(event);
            if should_follow_tail {
                self.messages.scroll = usize::MAX;
            }
            if should_select {
                self.messages.selection = Some(event_id);
            }
            self.rooms.unread.remove(&key);
            LiveFrameAction::None
        } else {
            if should_show_event(&event, &self.display) {
                *self.rooms.unread.entry(key).or_default() += 1;
            }
            if known_room {
                LiveFrameAction::None
            } else {
                LiveFrameAction::RefreshRooms
            }
        }
    }

    pub(crate) fn rebuild_display_names(&mut self, room: &RoomDto, events: &[EventDto]) {
        let key = RoomKey::from(room);
        self.rooms.display_names.remove(&key);
        for event in events {
            self.remember_display_name_from_event(&key, event);
        }
    }

    fn remember_display_name_from_event(&mut self, key: &RoomKey, event: &EventDto) {
        if event.event_type != "m.room.member" {
            return;
        }
        let user_id = event.state_key().unwrap_or(&event.sender);
        let Some(display_name) = event.membership_display_name() else {
            return;
        };
        self.rooms
            .display_names
            .entry(key.clone())
            .or_default()
            .insert(user_id.to_owned(), display_name.to_owned());
    }

    pub(crate) fn sender_label(&self, event: &EventDto) -> String {
        if self.display.sender_name == SenderNameStyle::MatrixAddress {
            return event.sender.clone();
        }
        let key = RoomKey {
            account_id: event.account_id,
            room_id: event.room_id.clone(),
        };
        self.rooms
            .display_names
            .get(&key)
            .and_then(|names| names.get(&event.sender))
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| event.sender.clone())
    }

    pub(crate) fn selected_room(&self) -> Option<&RoomDto> {
        self.rooms
            .selected
            .and_then(|index| self.rooms.rooms.get(index))
    }

    pub(crate) fn selected_raw_events(&self) -> &[EventDto] {
        self.selected_room()
            .and_then(|room| self.messages.events.get(&RoomKey::from(room)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn selected_reactions(&self) -> HashMap<String, Vec<(String, usize)>> {
        collect_reactions(self.selected_raw_events())
    }

    pub(crate) fn selected_events(&self) -> Vec<&EventDto> {
        self.selected_room()
            .and_then(|room| self.messages.events.get(&RoomKey::from(room)))
            .map(|events| {
                events
                    .iter()
                    .filter(|event| should_show_event(event, &self.display))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn selected_message_id(&self) -> Option<&str> {
        self.messages.selection.as_deref()
    }

    fn selected_message_event(&self) -> Option<&EventDto> {
        let selected_message = self.messages.selection.as_deref()?;
        self.selected_events()
            .into_iter()
            .find(|event| event.event_id == selected_message)
    }

    pub(crate) fn move_selected_message(&mut self, offset: isize) {
        let Some((event_id, next, event_count)) = ({
            let events = self.selected_events();
            if events.is_empty() {
                None
            } else {
                let next = selected_message_target_index(
                    events.as_slice(),
                    self.messages.selection.as_deref(),
                    offset,
                );
                Some((events[next].event_id.clone(), next, events.len()))
            }
        }) else {
            self.messages.selection = None;
            self.status = Status::from("no displayed messages".to_owned());
            return;
        };
        self.messages.selection = Some(event_id);
        self.ensure_message_index_visible(next);
        self.status = Status::from(format!("selected message {} of {}", next + 1, event_count));
    }

    pub(crate) fn page_selected_message(&mut self, direction: isize) {
        let page = self.messages.page_size.max(1);
        let Some((event_id, next, event_count)) = ({
            let events = self.selected_events();
            if events.is_empty() {
                None
            } else {
                let sender_labels = self.sender_labels(events.as_slice());
                let reactions = self.selected_reactions();
                let ranges = message_line_ranges(
                    events.as_slice(),
                    sender_labels.as_slice(),
                    self.messages.width,
                    &reactions,
                    &self.colors,
                );
                let total_lines = ranges
                    .last()
                    .map(|range| range.end)
                    .unwrap_or_default()
                    .max(1);
                let current_index = self
                    .messages
                    .selection
                    .as_deref()
                    .and_then(|event_id| events.iter().position(|event| event.event_id == event_id))
                    .unwrap_or_else(|| {
                        if direction.is_negative() {
                            message_index_at_line(
                                ranges.as_slice(),
                                self.messages.scroll.saturating_add(page.saturating_sub(1)),
                            )
                        } else {
                            message_index_at_line(ranges.as_slice(), self.messages.scroll)
                        }
                    });
                let current_line = ranges
                    .get(current_index)
                    .map(|range| range.start)
                    .unwrap_or_default();
                let target_line = if direction.is_negative() {
                    current_line.saturating_sub(page)
                } else {
                    current_line
                        .saturating_add(page)
                        .min(total_lines.saturating_sub(1))
                };
                let next = message_index_at_line(ranges.as_slice(), target_line);
                Some((events[next].event_id.clone(), next, events.len()))
            }
        }) else {
            self.messages.selection = None;
            self.status = Status::from("no displayed messages".to_owned());
            return;
        };
        self.messages.selection = Some(event_id);
        self.ensure_message_index_visible(next);
        self.status = Status::from(format!("selected message {} of {}", next + 1, event_count));
    }

    fn ensure_message_index_visible(&mut self, index: usize) {
        let events = self.selected_events();
        let sender_labels = self.sender_labels(events.as_slice());
        let reactions = self.selected_reactions();
        let ranges = message_line_ranges(
            events.as_slice(),
            sender_labels.as_slice(),
            self.messages.width,
            &reactions,
            &self.colors,
        );
        let Some(range) = ranges.get(index) else {
            return;
        };
        let page_size = self.messages.page_size.max(1);
        let total_lines = ranges.last().map(|range| range.end).unwrap_or_default();
        let max_scroll = total_lines.saturating_sub(page_size);
        let mut scroll = self.messages.scroll.min(max_scroll);
        if range.start < scroll || range.end > scroll.saturating_add(page_size) {
            scroll = range.start;
        }
        self.messages.scroll = scroll.min(max_scroll);
    }

    pub(crate) async fn commit_room_search(&mut self, query: String) {
        if query.is_empty() {
            return;
        }
        let q = query.to_ascii_lowercase();
        let start = self.rooms.selected.map(|i| i + 1).unwrap_or(0);
        let count = self.rooms.rooms.len();
        let found = (start..count).find(|&i| room_matches_search(&self.rooms.rooms[i], &q));
        self.last_search = Some(query);
        match found {
            Some(idx) => {
                self.rooms.selected = Some(idx);
                self.load_selected_timeline().await;
                self.status = Status::from(format!("room match {} of {}", idx + 1, count));
            }
            None => self.status = Status::Info("no match".to_owned()),
        }
    }

    pub(crate) async fn search_adjacent_room(&mut self, query: &str, forward: bool) {
        let q = query.to_ascii_lowercase();
        let count = self.rooms.rooms.len();
        if count == 0 {
            return;
        }
        let found = if forward {
            let start = self.rooms.selected.map(|i| i + 1).unwrap_or(0);
            (start..count).find(|&i| room_matches_search(&self.rooms.rooms[i], &q))
        } else {
            let end = self.rooms.selected.unwrap_or(0);
            (0..end)
                .rev()
                .find(|&i| room_matches_search(&self.rooms.rooms[i], &q))
        };
        match found {
            Some(idx) => {
                self.rooms.selected = Some(idx);
                self.load_selected_timeline().await;
                self.status = Status::from(format!("room match {} of {}", idx + 1, count));
            }
            None => self.status = Status::Info("no more matches".to_owned()),
        }
    }

    pub(crate) fn commit_message_search(&mut self, query: String) {
        if query.is_empty() {
            return;
        }
        let q = query.to_ascii_lowercase();
        let current_id = self.messages.selection.clone();
        let (found, count) = {
            let events = self.selected_events();
            let count = events.len();
            let start = current_id
                .as_deref()
                .and_then(|id| events.iter().position(|e| e.event_id == id))
                .map(|i| i + 1)
                .unwrap_or(0);
            let found = events[start..]
                .iter()
                .enumerate()
                .find(|(_, e)| message_matches_search(e, &q))
                .map(|(i, e)| (i + start, e.event_id.clone()));
            (found, count)
        };
        self.last_search = Some(query);
        match found {
            Some((idx, event_id)) => {
                self.messages.selection = Some(event_id);
                self.ensure_message_index_visible(idx);
                self.status = Status::from(format!("message match {} of {}", idx + 1, count));
            }
            None => self.status = Status::Info("no match".to_owned()),
        }
    }

    pub(crate) fn search_adjacent_message(&mut self, query: &str, forward: bool) {
        let q = query.to_ascii_lowercase();
        let current_id = self.messages.selection.clone();
        let (found, count) = {
            let events = self.selected_events();
            let count = events.len();
            let current_pos = current_id
                .as_deref()
                .and_then(|id| events.iter().position(|e| e.event_id == id));
            let found = if forward {
                let start = current_pos.map(|i| i + 1).unwrap_or(0);
                events[start..]
                    .iter()
                    .enumerate()
                    .find(|(_, e)| message_matches_search(e, &q))
                    .map(|(i, e)| (i + start, e.event_id.clone()))
            } else {
                let end = current_pos.unwrap_or(count);
                events[..end]
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, e)| message_matches_search(e, &q))
                    .map(|(i, e)| (i, e.event_id.clone()))
            };
            (found, count)
        };
        match found {
            Some((idx, event_id)) => {
                self.messages.selection = Some(event_id);
                self.ensure_message_index_visible(idx);
                self.status = Status::from(format!("message match {} of {}", idx + 1, count));
            }
            None => self.status = Status::Info("no more matches".to_owned()),
        }
    }

    fn selected_display_line_count(&self) -> usize {
        let events = self.selected_events();
        let sender_labels = self.sender_labels(events.as_slice());
        let reactions = self.selected_reactions();
        message_line_ranges(
            events.as_slice(),
            sender_labels.as_slice(),
            self.messages.width,
            &reactions,
            &self.colors,
        )
        .last()
        .map(|range| range.end)
        .unwrap_or_default()
    }

    pub(crate) fn sender_labels(&self, events: &[&EventDto]) -> Vec<String> {
        events
            .iter()
            .map(|event| self.sender_label(event))
            .collect()
    }

    pub(crate) fn set_message_viewport(&mut self, page_size: usize, width: usize) {
        self.messages.page_size = page_size.max(1);
        self.messages.width = width.max(1);
        let line_count = self.selected_display_line_count();
        let max_scroll = line_count.saturating_sub(self.messages.page_size);
        self.messages.scroll = if self.messages.scroll == usize::MAX {
            max_scroll
        } else {
            self.messages.scroll.min(max_scroll)
        };
    }

    pub(crate) fn start_reply_to_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::from("select a displayed message before replying".to_owned());
            return;
        };
        self.status = Status::EventAction {
            debug: format!("reply to {} waits for the Axon write API", event.event_id),
            redacted: "reply to message waits for the Axon write API",
        };
    }

    pub(crate) fn start_thread_from_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status =
                Status::from("select a displayed message before starting a thread".to_owned());
            return;
        };
        self.status = Status::EventAction {
            debug: format!(
                "thread from {} waits for the Axon write API",
                event.event_id
            ),
            redacted: "thread from message waits for the Axon write API",
        };
    }

    pub(crate) fn start_edit_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::from("select a displayed message before editing".to_owned());
            return;
        };
        let event_id = event.event_id.clone();
        let body = event.display_body();
        self.input.buffer = body;
        self.move_cursor_to_end();
        self.mode = Mode::Editing {
            event_id: event_id.clone(),
        };
        self.status = Status::EventAction {
            debug: format!("editing {} - Esc to cancel", event_id),
            redacted: "editing message - Esc to cancel",
        };
    }

    pub(crate) fn start_react_to_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::from("select a displayed message before reacting".to_owned());
            return;
        };
        let event_id = event.event_id.clone();
        self.input.buffer.clear();
        self.input.cursor = 0;
        self.input.react_tab = None;
        self.mode = Mode::Reacting {
            event_id: event_id.clone(),
        };
        self.status = Status::EventAction {
            debug: format!("react to {} - type emoji, Enter to send", event_id),
            redacted: "react to message - type emoji, Enter to send",
        };
    }

    async fn send_message_to_room(&mut self, body: &str) {
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from("select a room before sending".to_owned());
            return;
        };
        match self
            .client
            .send_message(room.account_id, &room.room_id, body)
            .await
        {
            Ok(r) => {
                self.messages.scroll = usize::MAX;
                self.live.pending_own_event_id = Some(r.event_id.clone());
                self.status = Status::EventAction {
                    debug: format!("sent: {}", r.event_id),
                    redacted: "sent",
                };
            }
            Err(err) => self.status = Status::Info(format!("send failed: {err}")),
        }
    }

    pub(crate) async fn send_edit(&mut self, event_id: &str, body: &str) {
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from("no room selected".to_owned());
            return;
        };
        match self
            .client
            .edit_message(room.account_id, &room.room_id, event_id, body)
            .await
        {
            Ok(result) => {
                let key = RoomKey::from(&room);
                if let Some(events) = self.messages.events.get_mut(&key) {
                    if let Some(e) = events.iter_mut().find(|e| e.event_id == event_id) {
                        e.body = Some(body.to_owned());
                    }
                }
                self.status = Status::EventAction {
                    debug: format!("edited: {}", result.event_id),
                    redacted: "edited",
                };
            }
            Err(err) => self.status = Status::Info(format!("edit failed: {err}")),
        }
    }

    pub(crate) async fn redact_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::from("select a displayed message before redacting".to_owned());
            return;
        };
        let event_id = event.event_id.clone();
        let room = self.selected_room().cloned().expect("event implies room");
        match self
            .client
            .redact_event(room.account_id, &room.room_id, &event_id, None)
            .await
        {
            Ok(result) => {
                let key = RoomKey::from(&room);
                if let Some(events) = self.messages.events.get_mut(&key) {
                    if let Some(e) = events.iter_mut().find(|e| e.event_id == event_id) {
                        e.redacted = true;
                    }
                }
                self.status = Status::EventAction {
                    debug: format!("redacted: {}", result.event_id),
                    redacted: "redacted",
                };
            }
            Err(err) => self.status = Status::Info(format!("redact failed: {err}")),
        }
    }

    pub(crate) async fn send_react(&mut self, event_id: &str, key: &str) {
        if key.is_empty() {
            self.status = Status::from("reaction key cannot be empty".to_owned());
            return;
        }
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from("no room selected".to_owned());
            return;
        };
        self.status = match self
            .client
            .react(room.account_id, &room.room_id, event_id, key)
            .await
        {
            Ok(r) => Status::EventAction {
                debug: format!("reacted: {}", r.event_id),
                redacted: "reacted",
            },
            Err(err) => Status::Info(format!("react failed: {err}")),
        };
    }

    pub(crate) fn update_react_status(&mut self, event_id: &str) {
        if self.input.buffer.is_empty() {
            self.status = Status::EventAction {
                debug: format!("react to {event_id} - type emoji name, Enter to send"),
                redacted: "react to message - type emoji name, Enter to send",
            };
            return;
        }
        let matches = emoji_matches(&self.input.buffer);
        self.status = Status::Info(match matches.as_slice() {
            [] => format!("no emoji matches '{}'", self.input.buffer),
            [single] => format!("{} {} - Enter to send", single.as_str(), single.name()),
            _ => {
                let shown: Vec<_> = matches
                    .iter()
                    .take(5)
                    .map(|e| format!("{} {}", e.as_str(), e.name()))
                    .collect();
                let more = matches.len().saturating_sub(5);
                if more == 0 {
                    format!("matches: {} - Tab to select", shown.join(", "))
                } else {
                    format!(
                        "matches: {}, +{more} more - Tab to select",
                        shown.join(", ")
                    )
                }
            }
        });
    }

    pub(crate) fn complete_react_input(&mut self, event_id: &str) {
        let matches = emoji_matches(&self.input.buffer);
        if matches.len() < 2 {
            return;
        }
        let count = matches.len();
        let next = self.input.react_tab.map(|i| (i + 1) % count).unwrap_or(0);
        self.input.react_tab = Some(next);
        let selected = &matches[next];
        self.status = Status::EventAction {
            debug: format!(
                "[{}/{}] react to {} with {} {} - Tab for next, Enter to send",
                next + 1,
                count,
                event_id,
                selected.as_str(),
                selected.name()
            ),
            redacted: "reaction selected - Tab for next, Enter to send",
        };
    }
}

pub(crate) fn format_time(origin_ts: i64) -> String {
    let Ok(millis) = u64::try_from(origin_ts) else {
        return "--:--:--".to_owned();
    };
    let Some(time) = UNIX_EPOCH.checked_add(Duration::from_millis(millis)) else {
        return "--:--:--".to_owned();
    };
    let Ok(since_midnight) = time.duration_since(UNIX_EPOCH) else {
        return "--:--:--".to_owned();
    };
    let seconds = since_midnight.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

pub(crate) fn relative_room_index(current: usize, len: usize, offset: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if offset.is_negative() {
        current
            .checked_sub(offset.unsigned_abs())
            .unwrap_or(len.saturating_sub(1))
    } else {
        (current + offset as usize) % len
    }
}

pub(crate) fn selected_message_target_index(
    events: &[&EventDto],
    selected_message: Option<&str>,
    offset: isize,
) -> usize {
    if events.is_empty() {
        return 0;
    }
    let Some(current) = selected_message
        .and_then(|event_id| events.iter().position(|event| event.event_id == event_id))
    else {
        return if offset.is_negative() {
            events.len().saturating_sub(1)
        } else {
            0
        };
    };
    if offset.is_negative() {
        current.saturating_sub(offset.unsigned_abs())
    } else {
        current
            .saturating_add(offset as usize)
            .min(events.len().saturating_sub(1))
    }
}

pub(crate) fn message_line_ranges(
    events: &[&EventDto],
    sender_labels: &[String],
    width: usize,
    reactions: &HashMap<String, Vec<(String, usize)>>,
    colors: &ColorScheme,
) -> Vec<Range<usize>> {
    let empty = vec![];
    let mut start = 0;
    events
        .iter()
        .zip(sender_labels)
        .map(|(event, sender_label)| {
            let event_reactions = reactions
                .get(&event.event_id)
                .map(Vec::as_slice)
                .unwrap_or(&empty);
            let count =
                message_display_line_count(event, sender_label, width, event_reactions, colors);
            let range = start..start + count;
            start += count;
            range
        })
        .collect()
}

pub(crate) fn message_index_at_line(ranges: &[Range<usize>], line: usize) -> usize {
    ranges
        .iter()
        .position(|range| line < range.end)
        .unwrap_or_else(|| ranges.len().saturating_sub(1))
}

fn message_display_line_count(
    event: &EventDto,
    sender_label: &str,
    width: usize,
    event_reactions: &[(String, usize)],
    colors: &ColorScheme,
) -> usize {
    let body_lines = message_body_lines(
        event,
        sender_label,
        first_body_width(sender_label, event.origin_ts, width),
        continuation_body_width(width),
        colors,
    )
    .len()
    .max(1);
    body_lines + usize::from(!event_reactions.is_empty())
}

pub(crate) fn message_display_lines(
    events: &[&EventDto],
    sender_labels: &[String],
    selected_message: Option<&str>,
    colors: &ColorScheme,
    width: usize,
    reactions: &HashMap<String, Vec<(String, usize)>>,
    own_senders: &HashMap<Uuid, String>,
) -> Vec<Line<'static>> {
    let reaction_style = Style::default().fg(colors.input_hint);
    events
        .iter()
        .zip(sender_labels)
        .flat_map(|(event, sender_label)| {
            let is_selected = selected_message == Some(event.event_id.as_str());
            let is_own = own_senders.get(&event.account_id) == Some(&event.sender);
            let marker = if is_selected { "> " } else { "  " };
            let time_style = if is_selected {
                Style::default().fg(colors.selected_room)
            } else {
                Style::default()
            };
            let sender_color = if is_own {
                colors.own_message_sender
            } else {
                colors.message_sender
            };
            let body_lines = message_body_lines(
                event,
                sender_label,
                first_body_width(sender_label, event.origin_ts, width),
                continuation_body_width(width),
                colors,
            );
            let event_reactions = reactions.get(&event.event_id).cloned().unwrap_or_default();
            let reaction_line = if event_reactions.is_empty() {
                None
            } else {
                let text = event_reactions
                    .iter()
                    .map(|(key, count)| format!("{key} {count}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                Some(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(text, reaction_style),
                ]))
            };
            let body_iter = body_lines
                .into_iter()
                .enumerate()
                .map(move |(index, body)| {
                    if index == 0 {
                        let mut spans = vec![
                            Span::styled(marker, time_style),
                            Span::styled(format!("{} ", format_time(event.origin_ts)), time_style),
                            Span::styled(
                                format!("{sender_label}: "),
                                Style::default()
                                    .fg(sender_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ];
                        spans.extend(body);
                        Line::from(spans)
                    } else {
                        let mut spans = vec![Span::raw("  ")];
                        spans.extend(body);
                        Line::from(spans)
                    }
                });
            body_iter.chain(reaction_line)
        })
        .collect()
}

fn first_body_width(sender_label: &str, origin_ts: i64, width: usize) -> usize {
    let prefix_width =
        2 + format_time(origin_ts).chars().count() + 1 + sender_label.chars().count() + 2;
    width.saturating_sub(prefix_width).max(1)
}

pub(crate) fn display_body_with_sender(event: &EventDto, sender_label: &str) -> String {
    if event.redacted {
        return "[redacted]".to_owned();
    }
    if let Some(membership) = event.membership_change() {
        return match membership.as_str() {
            "join" => format!("{sender_label} joined the room"),
            "leave" => format!("{sender_label} left the room"),
            "ban" => format!("{sender_label} was banned from the room"),
            "invite" => format!("{sender_label} was invited to the room"),
            _ => format!("{sender_label} membership changed: {membership}"),
        };
    }
    event.display_body()
}

fn message_body_lines(
    event: &EventDto,
    sender_label: &str,
    first_width: usize,
    continuation_width: usize,
    colors: &ColorScheme,
) -> Vec<Vec<Span<'static>>> {
    if !event.redacted && event.membership_change().is_none() {
        if let Some(lines) = event.formatted_body().and_then(|html| {
            formatted_message_body_lines(html, first_width, continuation_width, colors)
        }) {
            return lines;
        }
    }

    rich_lines_to_spans(wrap_rich_lines(
        plain_rich_lines(&display_body_with_sender(event, sender_label)),
        first_width,
        continuation_width,
    ))
}

fn continuation_body_width(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}

fn apply_edits(events: &mut Vec<EventDto>) {
    let edits: Vec<(String, String)> = events
        .iter()
        .filter_map(|e| {
            let (target, body) = e.edit_relation()?;
            Some((target.to_owned(), body.to_owned()))
        })
        .collect();
    for (target_id, new_body) in &edits {
        if let Some(e) = events.iter_mut().find(|e| &e.event_id == target_id) {
            e.body = Some(new_body.clone());
        }
    }
    events.retain(|e| e.edit_relation().is_none());
}

fn collect_reactions(events: &[EventDto]) -> HashMap<String, Vec<(String, usize)>> {
    let mut map: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for event in events {
        if let Some((target, key)) = event.reaction_annotation() {
            *map.entry(target.to_owned())
                .or_default()
                .entry(key.to_owned())
                .or_default() += 1;
        }
    }
    map.into_iter()
        .map(|(event_id, counts)| {
            let mut pairs: Vec<(String, usize)> = counts.into_iter().collect();
            pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            (event_id, pairs)
        })
        .collect()
}

pub(crate) fn should_show_event(event: &EventDto, display: &DisplayOptions) -> bool {
    if event.event_type == "m.reaction" {
        return false;
    }
    display.show_state_events || !event.is_state_event() || event.is_membership_event()
}

fn room_matches_search(room: &RoomDto, query: &str) -> bool {
    [
        Some(room.room_id.as_str()),
        room.canonical_alias.as_deref(),
        room.name.as_deref(),
        room.topic.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_ascii_lowercase().contains(query))
}

fn message_matches_search(event: &EventDto, query: &str) -> bool {
    if event.redacted {
        return false;
    }
    event
        .body
        .as_deref()
        .is_some_and(|body| body.to_ascii_lowercase().contains(query))
}

pub(crate) fn emoji_matches(query: &str) -> Vec<&'static emojis::Emoji> {
    if query.is_empty() {
        return vec![];
    }
    let q = query.to_ascii_lowercase();
    emojis::iter()
        .filter(|e| e.name().to_ascii_lowercase().contains(q.as_str()))
        .collect()
}

fn slash_command_prefix(input: &str) -> Option<&str> {
    let input = input.trim_start();
    let without_slash = input.strip_prefix('/')?;
    (!without_slash.chars().any(char::is_whitespace)).then_some(without_slash)
}

fn slash_command_candidates(prefix: &str) -> Vec<SlashCommand> {
    let command_prefix = format!("/{prefix}");
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.name.starts_with(&command_prefix))
        .collect()
}

fn switch_target_prefix(input: &str) -> Option<&str> {
    let rest = input.strip_prefix("/switch")?;
    if rest.is_empty() {
        return Some("");
    }
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| rest.trim_start())
}

fn room_completion_value(room: &RoomDto) -> String {
    room.canonical_alias
        .as_deref()
        .or(room.name.as_deref())
        .unwrap_or(&room.room_id)
        .to_owned()
}

fn room_matches_completion(room: &RoomDto, target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return true;
    }

    let target_lower = target.to_lowercase();
    if [
        Some(room.room_id.as_str()),
        room.canonical_alias.as_deref(),
        room.name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().starts_with(&target_lower))
    {
        return true;
    }

    if let Some(alias) = room_alias_with_hash(target) {
        if room
            .canonical_alias
            .as_deref()
            .is_some_and(|field| field.to_lowercase().starts_with(&alias.to_lowercase()))
        {
            return true;
        }
    }

    let Some(target_local) = incomplete_matrix_room_name(target) else {
        return false;
    };
    room.canonical_alias
        .as_deref()
        .and_then(matrix_room_local_name)
        .is_some_and(|local| {
            local
                .to_lowercase()
                .starts_with(&target_local.to_lowercase())
        })
        || matrix_room_local_name(&room.room_id).is_some_and(|local| {
            local
                .to_lowercase()
                .starts_with(&target_local.to_lowercase())
        })
}

fn incomplete_matrix_room_name(target: &str) -> Option<&str> {
    let target = target.trim();
    if target.is_empty() || target.contains(':') {
        return None;
    }
    Some(target.trim_start_matches(['#', '!'])).filter(|target| !target.is_empty())
}

fn room_alias_with_hash(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() || target.starts_with(['#', '!']) || !target.contains(':') {
        return None;
    }
    Some(format!("#{target}"))
}

fn matrix_room_local_name(value: &str) -> Option<&str> {
    let value = value.strip_prefix(['#', '!'])?;
    value.split_once(':').map(|(local, _server)| local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::HELP_COMMANDS;
    use crate::ui::{entry_status_text, popup_shortcuts_lines};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn room(room_id: &str, alias: Option<&str>, name: Option<&str>) -> RoomDto {
        RoomDto {
            account_id: Uuid::nil(),
            account_user_id: Some("@alice:example.com".to_owned()),
            room_id: room_id.to_owned(),
            name: name.map(str::to_owned),
            topic: None,
            avatar_url: None,
            canonical_alias: alias.map(str::to_owned),
            last_activity_ts: 0,
            last_event_id: None,
        }
    }

    fn event_with_id(
        event_id: &str,
        event_type: &str,
        body: Option<&str>,
        content: serde_json::Value,
    ) -> EventDto {
        event_with_state_key(event_id, event_type, None, body, content)
    }

    fn event_with_state_key(
        event_id: &str,
        event_type: &str,
        state_key: Option<&str>,
        body: Option<&str>,
        content: serde_json::Value,
    ) -> EventDto {
        EventDto {
            account_id: Uuid::nil(),
            event_id: event_id.to_owned(),
            room_id: "!room:example.com".to_owned(),
            sender: "@alice:example.com".to_owned(),
            state_key: state_key.map(str::to_owned),
            origin_ts: 0,
            event_type: event_type.to_owned(),
            content: Some(content),
            body: body.map(str::to_owned),
            relates_to: None,
            redacted: false,
            redaction_event_id: None,
        }
    }

    fn event(event_type: &str, body: Option<&str>, content: serde_json::Value) -> EventDto {
        event_with_id(
            &format!("${event_type}:example.com"),
            event_type,
            body,
            content,
        )
    }

    fn app_with_rooms(rooms: Vec<RoomDto>) -> App {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned()),
            None,
            TuiConfig::test_default(),
        );
        app.rooms.rooms = rooms;
        app.show_input_help = false;
        app.status = Status::Info(String::new());
        app
    }

    #[test]
    fn room_refresh_preserves_selected_room_by_key() {
        let first = room("!one:example.com", Some("#one:example.com"), Some("One"));
        let second = room("!two:example.com", Some("#two:example.com"), Some("Two"));
        let mut app = app_with_rooms(vec![first.clone(), second.clone()]);
        app.rooms.selected = Some(1);

        app.apply_room_refresh(vec![second.clone(), first]);

        assert_eq!(
            app.selected_room().map(|room| room.room_id.as_str()),
            Some("!two:example.com")
        );
        assert_eq!(app.rooms.selected, Some(0));
    }

    #[test]
    fn live_event_for_unknown_room_requests_room_refresh() {
        let mut app = app_with_rooms(Vec::new());
        let event = event_with_id(
            "$new:example.com",
            "m.room.message",
            Some("hello"),
            serde_json::json!({ "msgtype": "m.text", "body": "hello" }),
        );

        let action = app.handle_live_frame(LiveFrame::Timeline(Box::new(event)));

        assert_eq!(action, LiveFrameAction::RefreshRooms);
    }

    #[test]
    fn live_event_for_known_unselected_room_only_updates_unread() {
        let mut app = app_with_rooms(vec![room(
            "!room:example.com",
            Some("#room:example.com"),
            Some("Room"),
        )]);
        let event = event_with_id(
            "$known:example.com",
            "m.room.message",
            Some("hello"),
            serde_json::json!({ "msgtype": "m.text", "body": "hello" }),
        );

        let action = app.handle_live_frame(LiveFrame::Timeline(Box::new(event)));

        assert_eq!(action, LiveFrameAction::None);
        assert_eq!(
            app.rooms
                .unread
                .get(&RoomKey {
                    account_id: Uuid::nil(),
                    room_id: "!room:example.com".to_owned(),
                })
                .copied(),
            Some(1)
        );
    }

    #[test]
    fn hidden_live_event_for_known_unselected_room_does_not_update_unread() {
        let mut app = app_with_rooms(vec![room(
            "!room:example.com",
            Some("#room:example.com"),
            Some("Room"),
        )]);
        let event = event_with_id(
            "$reaction:example.com",
            "m.reaction",
            None,
            serde_json::json!({
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": "$known:example.com",
                    "key": "👍"
                }
            }),
        );

        let action = app.handle_live_frame(LiveFrame::Timeline(Box::new(event)));

        assert_eq!(action, LiveFrameAction::None);
        assert_eq!(
            app.rooms.unread.get(&RoomKey {
                account_id: Uuid::nil(),
                room_id: "!room:example.com".to_owned(),
            }),
            None
        );
    }

    #[test]
    pub(crate) fn find_room_matches_incomplete_alias_localpart() {
        let app = app_with_rooms(vec![room(
            "!abc:example.com",
            Some("#test:example.com"),
            Some("Test Room"),
        )]);

        assert_eq!(app.find_room("test"), Some(0));
        assert_eq!(app.find_room("#test"), Some(0));
        assert_eq!(app.find_room("TEST"), Some(0));
    }

    #[test]
    pub(crate) fn find_room_matches_one_based_room_list_number() {
        let app = app_with_rooms(vec![
            room("!one:example.com", Some("#one:example.com"), Some("One")),
            room("!two:example.com", Some("#two:example.com"), Some("Two")),
        ]);

        assert_eq!(app.find_room("1"), Some(0));
        assert_eq!(app.find_room("2"), Some(1));
        assert_eq!(app.find_room("0"), None);
        assert_eq!(app.find_room("3"), None);
    }

    #[test]
    pub(crate) fn relative_room_index_wraps_next_and_previous() {
        assert_eq!(relative_room_index(0, 3, 1), 1);
        assert_eq!(relative_room_index(2, 3, 1), 0);
        assert_eq!(relative_room_index(1, 3, -1), 0);
        assert_eq!(relative_room_index(0, 3, -1), 2);
    }

    #[test]
    fn event_filter_hides_state_events_but_keeps_membership() {
        let mut display = DisplayOptions {
            debug: false,
            show_state_events: false,
            sender_name: SenderNameStyle::DisplayName,
            input_lines: 1,
        };
        let state = event_with_state_key(
            "$m.room.topic:example.com",
            "m.room.topic",
            Some(""),
            None,
            serde_json::json!({ "topic": "new topic" }),
        );
        let membership = event_with_state_key(
            "$m.room.member:example.com",
            "m.room.member",
            Some("@alice:example.com"),
            None,
            serde_json::json!({ "membership": "join" }),
        );
        let message = event(
            "m.room.message",
            Some("hello"),
            serde_json::json!({ "msgtype": "m.text", "body": "hello" }),
        );
        let utd = EventDto {
            content: None,
            body: None,
            event_type: "m.room.encrypted".to_owned(),
            ..event("m.room.encrypted", None, serde_json::json!({}))
        };

        assert!(!should_show_event(&state, &display));
        assert!(should_show_event(&membership, &display));
        assert!(should_show_event(&message, &display));
        assert!(should_show_event(&utd, &display));

        display.show_state_events = true;
        assert!(should_show_event(&state, &display));
    }

    #[test]
    pub(crate) fn sender_label_defaults_to_membership_display_name() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        let membership = EventDto {
            sender: "@jamie:example.com".to_owned(),
            ..event_with_state_key(
                "$member:example.com",
                "m.room.member",
                Some("@alice:example.com"),
                None,
                serde_json::json!({
                    "membership": "join",
                    "displayname": "Alice"
                }),
            )
        };
        app.rebuild_display_names(&room, &[membership]);
        let message = event_with_id(
            "$message:example.com",
            "m.room.message",
            Some("hello"),
            serde_json::json!({ "msgtype": "m.text", "body": "hello" }),
        );

        assert_eq!(app.sender_label(&message), "Alice");
    }

    #[test]
    pub(crate) fn sender_label_can_use_matrix_address() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.display.sender_name = SenderNameStyle::MatrixAddress;
        let membership = event_with_state_key(
            "$member:example.com",
            "m.room.member",
            Some("@alice:example.com"),
            None,
            serde_json::json!({
                "membership": "join",
                "displayname": "Alice"
            }),
        );
        app.rebuild_display_names(&room, &[membership]);
        let message = event_with_id(
            "$message:example.com",
            "m.room.message",
            Some("hello"),
            serde_json::json!({ "msgtype": "m.text", "body": "hello" }),
        );

        assert_eq!(app.sender_label(&message), "@alice:example.com");
    }

    #[test]
    fn own_sender_is_known_from_room_summary_before_first_send() {
        let account_id = Uuid::from_u128(1);
        let mut room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        room.account_id = account_id;
        room.account_user_id = Some("@me:example.com".to_owned());
        let mut app = app_with_rooms(vec![room]);

        app.seed_own_senders_from_rooms();

        assert_eq!(
            app.live.own_senders.get(&account_id).map(String::as_str),
            Some("@me:example.com")
        );
    }

    #[test]
    fn room_summary_without_own_sender_still_loads() {
        let account_id = Uuid::from_u128(3);
        let mut room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        room.account_id = account_id;
        room.account_user_id = None;
        let mut app = app_with_rooms(vec![room]);

        app.seed_own_senders_from_rooms();

        assert!(!app.live.own_senders.contains_key(&account_id));
    }

    #[test]
    fn own_message_color_applies_without_send_echo() {
        let account_id = Uuid::from_u128(2);
        let colors = TuiConfig::test_default().colors;
        let event = EventDto {
            account_id,
            sender: "@me:example.com".to_owned(),
            ..event_with_id(
                "$message:example.com",
                "m.room.message",
                Some("hello"),
                serde_json::json!({ "msgtype": "m.text", "body": "hello" }),
            )
        };
        let sender_labels = vec!["@me:example.com".to_owned()];
        let own_senders = HashMap::from([(account_id, "@me:example.com".to_owned())]);
        let lines = message_display_lines(
            &[&event],
            sender_labels.as_slice(),
            None,
            &colors,
            80,
            &HashMap::new(),
            &own_senders,
        );

        assert_eq!(lines[0].spans[2].style.fg, Some(colors.own_message_sender));
    }

    #[tokio::test]
    async fn whoami_shows_current_user_id_and_display_name() {
        let mut room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        room.account_user_id = Some("@me:example.com".to_owned());
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        let membership = event_with_state_key(
            "$member:example.com",
            "m.room.member",
            Some("@me:example.com"),
            None,
            serde_json::json!({
                "membership": "join",
                "displayname": "Me Myself"
            }),
        );
        app.rebuild_display_names(&room, &[membership]);

        app.handle_command(Command::Whoami).await;

        assert_eq!(
            app.status.text(false),
            "Matrix ID: @me:example.com; Display Name: Me Myself"
        );
    }

    #[tokio::test]
    async fn whoami_reports_unknown_display_name() {
        let mut room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        room.account_user_id = Some("@me:example.com".to_owned());
        let mut app = app_with_rooms(vec![room]);
        app.rooms.selected = Some(0);

        app.handle_command(Command::Whoami).await;

        assert_eq!(
            app.status.text(false),
            "Matrix ID: @me:example.com; Display Name: unknown"
        );
    }

    #[tokio::test]
    async fn whoami_requires_selected_room_with_user_id() {
        let mut app = app_with_rooms(Vec::new());

        app.handle_command(Command::Whoami).await;

        assert_eq!(app.status.text(false), "select a room before using /whoami");

        let mut room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        room.account_user_id = None;
        app.rooms.rooms = vec![room];
        app.rooms.selected = Some(0);

        app.handle_command(Command::Whoami).await;

        assert_eq!(
            app.status.text(false),
            "current user is unavailable for this room"
        );
    }

    #[tokio::test]
    async fn whereami_opens_room_info_popup_for_selected_room() {
        let mut app = app_with_rooms(vec![room(
            "!room:example.com",
            Some("#room:example.com"),
            Some("Room"),
        )]);
        app.rooms.selected = Some(0);
        app.popup_scroll = 4;

        app.handle_command(Command::Whereami).await;

        assert_eq!(app.mode, Mode::Popup(PopupKind::RoomInfo));
        assert_eq!(app.popup_scroll, 0);
    }

    #[tokio::test]
    async fn whereami_requires_selected_room() {
        let mut app = app_with_rooms(Vec::new());

        app.handle_command(Command::Whereami).await;

        assert_eq!(
            app.status.text(false),
            "select a room before using /whereami"
        );
        assert_eq!(app.mode, Mode::Compose);
    }

    #[tokio::test]
    async fn refresh_requests_terminal_redraw_once() {
        let mut app = app_with_rooms(Vec::new());

        app.handle_command(Command::Refresh).await;

        assert_eq!(app.status.text(false), "display refresh requested");
        assert!(app.take_redraw_request());
        assert!(!app.take_redraw_request());
    }

    #[tokio::test]
    async fn unsupported_and_unknown_commands_report_distinct_statuses() {
        let mut app = app_with_rooms(Vec::new());

        app.handle_command(Command::ApiUnsupported(
            "/join is not supported by the current Axon API".to_owned(),
        ))
        .await;
        assert_eq!(
            app.status.text(false),
            "/join is not supported by the current Axon API"
        );

        app.handle_command(Command::Unknown("unknown command: /frobnicate".to_owned()))
            .await;
        assert_eq!(app.status.text(false), "unknown command: /frobnicate");
    }

    #[test]
    fn formatted_body_renders_supported_html_styles() {
        let colors = TuiConfig::test_default().colors;
        let event = EventDto {
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "bold link code",
                "format": "org.matrix.custom.html",
                "formatted_body": "<strong>bold</strong> <a href=\"https://example.com\">link</a> <code>code</code>"
            })),
            body: Some("bold link code".to_owned()),
            ..event_with_id(
                "$message:example.com",
                "m.room.message",
                Some("bold link code"),
                serde_json::json!({ "msgtype": "m.text", "body": "bold link code" }),
            )
        };
        let sender_labels = vec!["@alice:example.com".to_owned()];
        let lines = message_display_lines(
            &[&event],
            sender_labels.as_slice(),
            None,
            &colors,
            80,
            &HashMap::new(),
            &HashMap::new(),
        );

        assert!(lines[0].spans.iter().any(|span| {
            span.content.contains("bold") && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        assert!(lines[0].spans.iter().any(|span| {
            span.content.contains("link")
                && span.style.fg == Some(colors.status)
                && span.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
        assert!(lines[0].spans.iter().any(|span| {
            span.content.contains("code") && span.style.fg == Some(colors.input_hint)
        }));
    }

    #[test]
    fn formatted_body_strips_unsupported_html_and_falls_back_when_empty() {
        let colors = TuiConfig::test_default().colors;
        let event = EventDto {
            content: Some(serde_json::json!({
                "msgtype": "m.text",
                "body": "fallback",
                "format": "org.matrix.custom.html",
                "formatted_body": "<script>alert('x')</script>"
            })),
            body: Some("fallback".to_owned()),
            ..event_with_id(
                "$message:example.com",
                "m.room.message",
                Some("fallback"),
                serde_json::json!({ "msgtype": "m.text", "body": "fallback" }),
            )
        };
        let sender_labels = vec!["@alice:example.com".to_owned()];
        let lines = message_display_lines(
            &[&event],
            sender_labels.as_slice(),
            None,
            &colors,
            80,
            &HashMap::new(),
            &HashMap::new(),
        );

        let text = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("fallback"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn message_navigation_selects_displayed_messages() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$one:example.com",
                    "m.room.message",
                    Some("one"),
                    serde_json::json!({ "msgtype": "m.text", "body": "one" }),
                ),
                event_with_id(
                    "$two:example.com",
                    "m.room.message",
                    Some("two"),
                    serde_json::json!({ "msgtype": "m.text", "body": "two" }),
                ),
            ],
        );

        app.move_selected_message(1);
        assert_eq!(app.selected_message_id(), Some("$one:example.com"));
        app.move_selected_message(1);
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        app.move_selected_message(-1);
        assert_eq!(app.selected_message_id(), Some("$one:example.com"));
    }

    #[test]
    fn message_navigation_clamps_at_list_edges() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$one:example.com",
                    "m.room.message",
                    Some("one"),
                    serde_json::json!({ "msgtype": "m.text", "body": "one" }),
                ),
                event_with_id(
                    "$two:example.com",
                    "m.room.message",
                    Some("two"),
                    serde_json::json!({ "msgtype": "m.text", "body": "two" }),
                ),
            ],
        );

        app.move_selected_message(-1);
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        app.move_selected_message(1);
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        app.move_selected_message(-10);
        assert_eq!(app.selected_message_id(), Some("$one:example.com"));
    }

    #[test]
    fn message_navigation_moves_by_message_not_display_line() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.page_size = 2;
        app.messages.width = 80;
        app.messages.scroll = 0;
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$multi:example.com",
                    "m.room.message",
                    Some("one\ntwo\nthree"),
                    serde_json::json!({ "msgtype": "m.text", "body": "one\ntwo\nthree" }),
                ),
                event_with_id(
                    "$next:example.com",
                    "m.room.message",
                    Some("next"),
                    serde_json::json!({ "msgtype": "m.text", "body": "next" }),
                ),
            ],
        );

        app.move_selected_message(1);
        assert_eq!(app.selected_message_id(), Some("$multi:example.com"));
        app.move_selected_message(1);
        assert_eq!(app.selected_message_id(), Some("$next:example.com"));
        assert_eq!(app.messages.scroll, 2);
        app.move_selected_message(-1);
        assert_eq!(app.selected_message_id(), Some("$multi:example.com"));
        assert_eq!(app.messages.scroll, 0);
    }

    #[test]
    fn message_page_navigation_uses_message_pane_page_size() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.page_size = 3;
        app.messages.scroll = 0;
        app.messages.events.insert(
            RoomKey::from(&room),
            (0..8)
                .map(|index| {
                    event_with_id(
                        &format!("${index}:example.com"),
                        "m.room.message",
                        Some("message"),
                        serde_json::json!({ "msgtype": "m.text", "body": "message" }),
                    )
                })
                .collect(),
        );

        app.page_selected_message(1);
        assert_eq!(app.selected_message_id(), Some("$3:example.com"));
        assert_eq!(app.messages.scroll, 3);
        app.page_selected_message(-1);
        assert_eq!(app.selected_message_id(), Some("$0:example.com"));
        assert_eq!(app.messages.scroll, 0);
    }

    #[test]
    fn message_navigation_ignores_hidden_state_events() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_state_key(
                    "$topic:example.com",
                    "m.room.topic",
                    Some(""),
                    None,
                    serde_json::json!({ "topic": "new topic" }),
                ),
                event_with_id(
                    "$message:example.com",
                    "m.room.message",
                    Some("message"),
                    serde_json::json!({ "msgtype": "m.text", "body": "message" }),
                ),
            ],
        );

        app.move_selected_message(1);

        assert_eq!(app.selected_message_id(), Some("$message:example.com"));
    }

    #[test]
    fn reply_and_thread_actions_target_selected_message() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![event_with_id(
                "$message:example.com",
                "m.room.message",
                Some("message"),
                serde_json::json!({ "msgtype": "m.text", "body": "message" }),
            )],
        );
        app.messages.selection = Some("$message:example.com".to_owned());

        app.start_reply_to_selected_message();
        assert_eq!(
            app.status,
            "reply to $message:example.com waits for the Axon write API"
        );

        app.start_thread_from_selected_message();
        assert_eq!(
            app.status,
            "thread from $message:example.com waits for the Axon write API"
        );
    }

    #[test]
    fn entry_status_hides_event_codes_unless_debug_is_enabled() {
        let mut app = app_with_rooms(Vec::new());
        app.status = Status::EventAction {
            debug: "editing $message:example.com - Esc to cancel".to_owned(),
            redacted: "editing message - Esc to cancel",
        };

        assert_eq!(entry_status_text(&app), "editing message - Esc to cancel");

        app.display.debug = true;

        assert_eq!(
            entry_status_text(&app),
            "editing $message:example.com - Esc to cancel"
        );
    }

    #[test]
    fn entry_status_hides_live_socket_status_unless_debug_is_enabled() {
        let mut app = app_with_rooms(Vec::new());
        app.status = Status::Debug("live WebSocket connected".to_owned());

        assert_eq!(entry_status_text(&app), "");

        app.display.debug = true;

        assert_eq!(entry_status_text(&app), "live WebSocket connected");
    }

    #[test]
    fn reconnecting_live_socket_status_is_visible() {
        let mut app = app_with_rooms(Vec::new());

        let action = app.handle_live_frame(LiveFrame::Reconnecting {
            reason: "connection reset".to_owned(),
            delay: std::time::Duration::from_secs(4),
        });

        assert_eq!(action, LiveFrameAction::None);
        assert_eq!(
            entry_status_text(&app),
            "live WebSocket reconnecting in 4s: connection reset"
        );
    }

    #[tokio::test]
    async fn clear_input_shortcut_aborts_message_selection() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![event_with_id(
                "$message:example.com",
                "m.room.message",
                Some("message"),
                serde_json::json!({ "msgtype": "m.text", "body": "message" }),
            )],
        );
        app.messages.selection = Some("$message:example.com".to_owned());
        app.input.buffer = "/switch room".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;

        assert_eq!(app.selected_message_id(), None);
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.input.cursor, 0);
    }

    #[tokio::test]
    async fn input_cursor_supports_readline_start_and_end() {
        let mut app = app_with_rooms(Vec::new());
        for ch in "abc".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(ch))).await;
        }

        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .await;
        app.handle_key(KeyEvent::from(KeyCode::Char('X'))).await;
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL))
            .await;
        app.handle_key(KeyEvent::from(KeyCode::Char('Y'))).await;

        assert_eq!(app.input.buffer, "XabcY");
        assert_eq!(app.input.cursor, app.input.buffer.len());
    }

    #[tokio::test]
    async fn arrow_up_navigates_timeline_messages_for_editing() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$one:example.com",
                    "m.room.message",
                    Some("first"),
                    serde_json::json!({ "msgtype": "m.text", "body": "first" }),
                ),
                event_with_id(
                    "$two:example.com",
                    "m.room.message",
                    Some("second"),
                    serde_json::json!({ "msgtype": "m.text", "body": "second" }),
                ),
            ],
        );

        // Up from no selection: jump to the last message
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "second");
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        assert!(matches!(app.mode, Mode::Editing { .. }));

        // Up again: move to the previous message
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "first");
        assert_eq!(app.selected_message_id(), Some("$one:example.com"));

        // Up at the first message: stay put
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "first");

        // Down: move forward
        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        assert_eq!(app.input.buffer, "second");
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));

        // Down past the last message: clear edit mode
        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.input.cursor, 0);
        assert!(matches!(app.mode, Mode::Compose));
        assert!(app.selected_message_id().is_none());
    }

    #[tokio::test]
    async fn arrow_up_does_nothing_with_no_room_selected() {
        let mut app = app_with_rooms(Vec::new());
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "");
        assert!(matches!(app.mode, Mode::Compose));
    }

    #[tokio::test]
    async fn global_message_navigation_abandons_edit_mode() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$one:example.com",
                    "m.room.message",
                    Some("first"),
                    serde_json::json!({ "msgtype": "m.text", "body": "first" }),
                ),
                event_with_id(
                    "$two:example.com",
                    "m.room.message",
                    Some("second"),
                    serde_json::json!({ "msgtype": "m.text", "body": "second" }),
                ),
            ],
        );
        app.messages.selection = Some("$one:example.com".to_owned());
        app.start_edit_selected_message();

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL))
            .await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.input.cursor, 0);
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
    }

    #[tokio::test]
    async fn focus_cycle_abandons_edit_mode_to_compose() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::Editing {
            event_id: "$old:example.com".to_owned(),
        };
        app.input.buffer = "old body".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL))
            .await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.input.cursor, 0);
    }

    #[tokio::test]
    async fn action_shortcuts_do_not_steal_compose_text_input() {
        for text in ["testing", "editing", "dog", "replying", "Reacting"] {
            let mut app = app_with_rooms(Vec::new());
            for ch in text.chars() {
                app.handle_key(KeyEvent::from(KeyCode::Char(ch))).await;
            }

            assert_eq!(app.input.buffer, text);
            assert_eq!(app.status, "");
        }
    }

    #[test]
    fn switch_completion_fills_unique_room_alias_match() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", Some("#one:example.com"), Some("One")),
            room("!test:example.com", Some("#test:example.com"), Some("Test")),
        ]);
        app.input.buffer = "/switch te".to_owned();

        app.complete_switch_input();

        assert_eq!(app.input.buffer, "/switch #test:example.com");
    }

    #[test]
    fn tab_completion_fills_unique_slash_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/sw".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/switch ");
    }

    #[test]
    fn tab_completion_fills_no_argument_slash_command_without_space() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/ro".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/rooms");
    }

    #[test]
    fn tab_completion_fills_help_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/he".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/help");
    }

    #[test]
    fn tab_completion_fills_shortcuts_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/sh".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/shortcuts");
    }

    #[test]
    fn tab_completion_reports_ambiguous_slash_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/");
        assert!(app.status.text(false).contains("/switch"));
        assert!(app.status.text(false).contains("/rooms"));
        assert!(app.status.text(false).contains("/event"));
        assert!(app.status.text(false).contains("/whoami"));
        assert!(app.status.text(false).contains("/whereami"));
        assert!(app.status.text(false).contains("/help"));
        assert!(app.status.text(false).contains("/shortcuts"));
        assert!(app.status.text(false).contains("/refresh"));
        assert!(app.status.text(false).contains("/quit"));
        assert!(app.status.text(false).contains("/join"));
        assert!(app.status.text(false).contains("/leave"));
        assert!(app.status.text(false).contains("/part"));
    }

    #[test]
    fn tab_completion_fills_refresh_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/ref".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/refresh");
    }

    #[test]
    fn tab_completion_fills_known_api_unsupported_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/jo".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/join ");
    }

    #[test]
    fn tab_completion_fills_whoami_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/who".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/whoami");
    }

    #[test]
    fn tab_completion_fills_whereami_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/where".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/whereami");
    }

    #[tokio::test]
    async fn popup_keys_scroll_and_close_popup() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::Popup(PopupKind::RoomInfo);

        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        assert_eq!(app.popup_scroll, 1);

        app.handle_key(KeyEvent::from(KeyCode::PageDown)).await;
        assert_eq!(app.popup_scroll, 9);

        app.handle_key(KeyEvent::from(KeyCode::PageUp)).await;
        assert_eq!(app.popup_scroll, 1);

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;
        assert_eq!(app.popup_scroll, 0);
        assert_eq!(app.mode, Mode::Compose);
    }

    #[tokio::test]
    async fn help_popup_selects_command_into_input() {
        let mut app = app_with_rooms(Vec::new());
        app.handle_command(Command::Help).await;

        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.input.buffer, "/switch ");
        assert_eq!(app.input.cursor, "/switch ".len());
        assert_eq!(app.status.text(false), "selected command: /switch <room>");
    }

    #[tokio::test]
    async fn help_popup_selection_wraps_and_esc_resets_it() {
        let mut app = app_with_rooms(Vec::new());
        app.handle_command(Command::Help).await;

        app.handle_key(KeyEvent::from(KeyCode::Up)).await;

        assert_eq!(app.help_selection, HELP_COMMANDS.len() - 1);

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.popup_scroll, 0);
        assert_eq!(app.help_selection, 0);
    }

    #[test]
    fn shortcuts_popup_lists_all_configurable_shortcuts() {
        let config = TuiConfig::test_default();
        let lines = popup_shortcuts_lines(&config.shortcuts);
        let text = lines.join("\n");

        assert!(text.contains("Ctrl-Space"));
        assert!(text.contains("Ctrl-N"));
        assert!(text.contains("Ctrl-P"));
        assert!(text.contains("Ctrl-J"));
        assert!(text.contains("Ctrl-K"));
        assert!(text.contains("PageUp"));
        assert!(text.contains("PageDown"));
        assert!(text.contains("edit previous message"));
        assert!(text.contains("edit next message"));
    }

    #[test]
    pub(crate) fn new_app_starts_with_one_time_input_help() {
        let app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned()),
            None,
            TuiConfig::test_default(),
        );

        assert!(app.show_input_help);
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn first_input_action_dismisses_input_help() {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned()),
            None,
            TuiConfig::test_default(),
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('/'))).await;

        assert!(!app.show_input_help);
        assert_eq!(app.input.buffer, "/");
    }

    #[tokio::test]
    async fn room_switch_shortcut_dismisses_input_help_when_no_rooms_exist() {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned()),
            None,
            TuiConfig::test_default(),
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))
            .await;

        assert!(!app.show_input_help);
        assert_eq!(app.status, "no rooms to switch");
    }

    #[tokio::test]
    async fn room_switch_shortcut_abandons_edit_mode() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::Editing {
            event_id: "$old:example.com".to_owned(),
        };
        app.input.buffer = "old body".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))
            .await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.input.cursor, 0);
        assert_eq!(app.status, "no rooms to switch");
    }

    #[test]
    fn tab_completion_reports_missing_slash_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/zzz".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/zzz");
        assert_eq!(app.status, "no command matches: /zzz");
    }

    #[test]
    fn switch_completion_adds_missing_hash_for_qualified_alias() {
        let mut app = app_with_rooms(vec![room(
            "!test:example.com",
            Some("#test:example.com"),
            Some("Test"),
        )]);
        app.input.buffer = "/switch test:ex".to_owned();

        app.complete_switch_input();

        assert_eq!(app.input.buffer, "/switch #test:example.com");
    }

    #[test]
    fn switch_completion_reports_ambiguous_room_matches() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", Some("#test:example.com"), Some("Test")),
            room(
                "!two:example.com",
                Some("#testing:example.com"),
                Some("Testing"),
            ),
        ]);
        app.input.buffer = "/switch test".to_owned();

        app.complete_switch_input();

        assert_eq!(app.input.buffer, "/switch test");
        assert!(app.status.text(false).contains("#test:example.com"));
        assert!(app.status.text(false).contains("#testing:example.com"));
    }

    #[test]
    fn switch_completion_only_runs_for_switch_command() {
        let mut app = app_with_rooms(vec![room(
            "!test:example.com",
            Some("#test:example.com"),
            Some("Test"),
        )]);
        app.input.buffer = "/event te".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/event te");
    }

    #[test]
    pub(crate) fn find_room_adds_missing_hash_to_fully_qualified_alias() {
        let app = app_with_rooms(vec![room(
            "!abc:example.com",
            Some("#test:example.com"),
            Some("Test Room"),
        )]);

        assert_eq!(app.find_room("test:example.com"), Some(0));
        assert_eq!(app.find_room("test:other.example"), None);
    }

    #[test]
    pub(crate) fn find_room_keeps_exact_alias_and_name_matches() {
        let app = app_with_rooms(vec![room(
            "!abc:example.com",
            Some("#test:example.com"),
            Some("Friendly Name"),
        )]);

        assert_eq!(app.find_room("#test:example.com"), Some(0));
        assert_eq!(app.find_room("friendly name"), Some(0));
    }

    #[test]
    pub(crate) fn find_room_does_not_local_match_fully_qualified_wrong_server() {
        let app = app_with_rooms(vec![room(
            "!abc:example.com",
            Some("#test:example.com"),
            Some("Test Room"),
        )]);

        assert_eq!(app.find_room("#test:other.example"), None);
    }
}
