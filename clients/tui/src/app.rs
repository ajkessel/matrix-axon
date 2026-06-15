use ratatui::layout::Size;
use ratatui_image::picker::Picker;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use uuid::Uuid;

#[cfg(test)]
use crate::api::LiveFrame;
use crate::api::{AccountDto, AccountState, AxonClient, EventDto, RoomDto};
use crate::command::Command;
#[cfg(test)]
use crate::config::SenderNameStyle;
use crate::config::{ColorScheme, DisplayOptions, Shortcuts, TuiConfig};
#[cfg(test)]
use ratatui::style::Modifier;
use std::path::PathBuf;
mod completion;
mod lifecycle;
pub(crate) use lifecycle::LifecycleOutcome;
pub(crate) mod media;
pub(crate) use media::{
    ImageState, MediaKey, MediaResult, ProtocolKey, ProtocolState, IMAGE_CACHE_LIMIT,
    MEDIA_WORKERS, PROTOCOL_CACHE_LIMIT,
};
mod reactions;
mod render;
mod rooms;
mod timeline;

pub(crate) use reactions::{collect_reactions, emoji_matches, unreact_selection_status};
pub(crate) use render::{
    display_body_with_sender, format_time, message_display_lines, message_index_at_line,
    message_line_ranges, IMAGE_THUMB_ROWS,
};
pub(crate) use rooms::account_localpart;
#[cfg(test)]
use timeline::should_show_event;

const TIMELINE_LIMIT: usize = 50;
pub(super) const PENDING_ECHO_MSG: &str =
    "message not yet confirmed by server — please wait a moment and try again";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Compose,
    LoginUsername,
    LoginPassword {
        username: String,
        /// Homeserver override carried from the username step, if the user gave
        /// one there. `None` means Axon resolves the homeserver.
        homeserver: Option<String>,
    },
    RecoveryKey {
        account: AccountDto,
        origin: RecoveryOrigin,
    },
    ConfirmLogout {
        account: AccountDto,
    },
    ConfirmDelete {
        account: AccountDto,
    },
    RoomList,
    AccountList,
    MessageList,
    Search(SearchKind, String),
    Editing {
        event_id: String,
    },
    Reacting {
        event_id: String,
    },
    Unreacting {
        target_event_id: String,
        choices: Vec<OwnReaction>,
        selected: usize,
    },
    Popup(PopupKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryOrigin {
    PostLogin,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnReaction {
    pub(crate) key: String,
    pub(crate) event_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchKind {
    Rooms,
    Messages,
    Accounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccountSelection {
    All,
    Account(usize),
}

impl AccountSelection {
    pub(crate) fn display_number(self) -> usize {
        match self {
            Self::All => 0,
            Self::Account(index) => index + 1,
        }
    }

    pub(crate) fn display_label(self, user_id: Option<&str>) -> String {
        match self {
            Self::All => format!("{} All Accounts", self.display_number()),
            Self::Account(_) => {
                format!("{} {}", self.display_number(), user_id.unwrap_or("?"))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PopupKind {
    Help,
    Shortcuts,
    RoomInfo,
    Status,
    CommandResponse,
    MediaPreview,
}

#[derive(Debug, Clone, Default)]
pub(crate) enum ConnectionState {
    #[default]
    Unknown,
    Connected,
    Reconnecting {
        reason: String,
        delay: std::time::Duration,
    },
    Disconnected(String),
    ProtocolError(String),
}

#[derive(Debug, Clone)]
pub(crate) enum Status {
    /// Transient guidance or general operation feedback.
    Info(String),
    /// Diagnostics hidden unless debug display is enabled.
    Debug(String),
    /// Feedback for an action tied to a specific event, with identifiers hidden by default.
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoomTargetResolution {
    Match(usize),
    Ambiguous(Vec<String>),
    Missing,
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
    pub(crate) accounts: AccountsState,
    pub(crate) messages: MessagePane,
    pub(crate) input: InputState,
    pub(crate) live: LiveState,
    pub(crate) connection_state: ConnectionState,
    pub(crate) mode: Mode,
    pub(crate) popup_scroll: usize,
    pub(crate) help_selection: usize,
    pub(crate) last_search: Option<String>,
    pub(crate) show_input_help: bool,
    pub(crate) status: Status,
    /// A completed slash-command response waiting for the renderer to decide
    /// whether it fits in the fixed-height entry box.
    pub(crate) pending_command_response: Option<String>,
    pub(crate) should_quit: bool,
    /// Sender for results of in-flight login/logout work spawned off the event
    /// loop. `None` until the main loop wires up the channel (and in unit tests).
    pub(crate) lifecycle_tx: Option<mpsc::UnboundedSender<LifecycleOutcome>>,
    /// True while a login or logout request is awaiting its result, so the UI
    /// stays responsive but a second lifecycle verb can't race the first.
    pub(crate) lifecycle_busy: bool,
    pub(crate) redraw_requested: bool,
    /// User-toggled hide for the accounts panel (independent of account count).
    pub(crate) accounts_panel_hidden: bool,
    /// User-toggled hide for the rooms panel.
    pub(crate) rooms_panel_hidden: bool,
    /// Path to the loaded config file, used by /saveconfig and /editconfig.
    pub(crate) config_path: PathBuf,
    /// Set by /editconfig; consumed by the main loop to suspend the TUI and open an editor.
    pub(crate) edit_config_requested: bool,
    /// When true, the room list shows only rooms with unread messages.
    pub(crate) unread_filter: bool,
    /// In-flight and decoded images, account-scoped and bounded by LRU order.
    pub(crate) image_cache: HashMap<MediaKey, ImageState>,
    image_cache_order: VecDeque<MediaKey>,
    /// Sender end of the channel the main loop listens on for completed media
    /// work. `None` until `set_media_sender` is called (unit tests may
    /// omit it).
    pub(crate) media_tx: Option<mpsc::Sender<MediaResult>>,
    media_workers: Arc<Semaphore>,
    /// Terminal image protocol picker, detected before raw mode with halfblocks
    /// as the universal fallback.
    pub(crate) picker: Picker,
    /// Bounded cache of protocols encoded for a specific image and cell size.
    pub(crate) proto_cache: HashMap<ProtocolKey, ProtocolState>,
    proto_cache_order: VecDeque<ProtocolKey>,
}

#[derive(Default)]
pub(crate) struct RoomsState {
    pub(crate) rooms: Vec<RoomDto>,
    pub(crate) selected: Option<usize>,
    pub(crate) scroll: usize,
    pub(crate) page_size: usize,
    pub(crate) display_names: HashMap<RoomKey, HashMap<String, String>>,
    pub(crate) unread: HashMap<RoomKey, usize>,
}

pub(crate) struct AccountsState {
    /// Only Active accounts. Used for panel display, navigation, and filtering.
    pub(crate) accounts: Vec<AccountDto>,
    /// Every account returned by `GET /v1/accounts` (active + deactivated).
    /// Retained for lifecycle/status views without exposing logged-out accounts
    /// through active-only navigation.
    pub(crate) client_visible: Vec<AccountDto>,
    /// Account IDs known to be inactive (deactivated/deleting). Kept separately
    /// so room-list filtering can drop their rooms even though they are not
    /// displayed in the panel.
    pub(crate) inactive_ids: std::collections::HashSet<Uuid>,
    pub(crate) selected: AccountSelection,
    pub(crate) scroll: usize,
    pub(crate) page_size: usize,
}

impl Default for AccountsState {
    fn default() -> Self {
        Self {
            accounts: Vec::new(),
            client_visible: Vec::new(),
            inactive_ids: std::collections::HashSet::new(),
            selected: AccountSelection::All,
            scroll: 0,
            page_size: 1,
        }
    }
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
    pub(crate) react_command_completion: Option<(String, usize)>,
    pub(crate) partial_room_completions: Option<Vec<String>>,
    pub(crate) room_command_completion: Option<(String, usize)>,
    pub(crate) logout_command_completion: Option<(String, usize)>,
    pub(crate) recover_command_completion: Option<(String, usize)>,
    pub(crate) delete_command_completion: Option<(String, usize)>,
    pub(crate) account_command_completion: Option<(String, usize)>,
}

#[derive(Default)]
pub(crate) struct LiveState {
    pub(crate) own_senders: HashMap<Uuid, String>,
    pub(crate) pending_own_event_id: Option<String>,
}

impl App {
    pub(crate) fn new(
        client: AxonClient,
        account_filter: Option<Uuid>,
        config: TuiConfig,
        picker: Picker,
    ) -> Self {
        let config_status = if config.created_default {
            format!("created default config at {}", config.path.display())
        } else {
            "connecting to Axon".to_owned()
        };
        let config_path = config.path.clone();
        Self {
            client,
            account_filter,
            shortcuts: config.shortcuts,
            colors: config.colors,
            display: config.display,
            rooms: RoomsState::default(),
            accounts: AccountsState::default(),
            messages: MessagePane::default(),
            input: InputState::default(),
            live: LiveState::default(),
            connection_state: ConnectionState::Unknown,
            mode: Mode::Compose,
            popup_scroll: 0,
            help_selection: 0,
            last_search: None,
            show_input_help: true,
            status: Status::Info(config_status),
            pending_command_response: None,
            should_quit: false,
            lifecycle_tx: None,
            lifecycle_busy: false,
            image_cache: HashMap::new(),
            image_cache_order: VecDeque::new(),
            media_tx: None,
            media_workers: Arc::new(Semaphore::new(MEDIA_WORKERS)),
            picker,
            proto_cache: HashMap::new(),
            proto_cache_order: VecDeque::new(),
            redraw_requested: false,
            accounts_panel_hidden: false,
            rooms_panel_hidden: false,
            config_path,
            edit_config_requested: false,
            unread_filter: false,
        }
    }

    /// Wire up the channel the main loop drains for spawned login/logout results.
    pub(crate) fn set_lifecycle_sender(&mut self, tx: mpsc::UnboundedSender<LifecycleOutcome>) {
        self.lifecycle_tx = Some(tx);
    }

    /// Wire up the channel the main loop drains for completed image downloads.
    pub(crate) fn set_media_sender(&mut self, tx: mpsc::Sender<MediaResult>) {
        self.media_tx = Some(tx);
    }

    /// Request a background download of `mxc_url` if it is not already cached
    /// or in flight. Does nothing if the image channel has not been wired up.
    pub(crate) fn request_image(&mut self, account_id: Uuid, mxc_url: String, is_encrypted: bool) {
        let key = MediaKey::new(account_id, mxc_url);
        if self.image_cache.contains_key(&key) {
            touch_lru(&mut self.image_cache_order, &key);
            return;
        }
        let Some(tx) = self.media_tx.clone() else {
            return;
        };
        if !evict_lru_where(
            &mut self.image_cache,
            &mut self.image_cache_order,
            IMAGE_CACHE_LIMIT,
            |state| !matches!(state, ImageState::Fetching),
        ) {
            return;
        }
        self.image_cache.insert(key.clone(), ImageState::Fetching);
        touch_lru(&mut self.image_cache_order, &key);
        media::spawn_image_fetch(
            self.client.clone(),
            key,
            is_encrypted,
            self.media_workers.clone(),
            tx,
        );
    }

    pub(crate) fn request_protocol(&mut self, key: MediaKey, size: Size) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let protocol_key = ProtocolKey {
            media: key.clone(),
            size,
        };
        if self.proto_cache.contains_key(&protocol_key) {
            touch_lru(&mut self.proto_cache_order, &protocol_key);
            return;
        }
        let Some(ImageState::Ready(image)) = self.image_cache.get(&key) else {
            return;
        };
        let Some(tx) = self.media_tx.clone() else {
            return;
        };
        let image = Arc::clone(image);
        if !evict_lru_where(
            &mut self.proto_cache,
            &mut self.proto_cache_order,
            PROTOCOL_CACHE_LIMIT,
            |state| !matches!(state, ProtocolState::Encoding),
        ) {
            return;
        }
        self.proto_cache
            .insert(protocol_key.clone(), ProtocolState::Encoding);
        touch_lru(&mut self.proto_cache_order, &protocol_key);
        media::spawn_protocol_encode(
            self.picker.clone(),
            image,
            protocol_key,
            self.media_workers.clone(),
            tx,
        );
    }

    pub(crate) fn handle_media_result(&mut self, result: MediaResult) {
        let updated = match result {
            MediaResult::Image { key, outcome } => {
                if !matches!(self.image_cache.get(&key), Some(ImageState::Fetching)) {
                    return;
                }
                let state = match outcome {
                    Ok(image) => ImageState::Ready(image),
                    Err(error) => ImageState::Failed(error),
                };
                self.image_cache.insert(key.clone(), state);
                touch_lru(&mut self.image_cache_order, &key);
                true
            }
            MediaResult::Protocol { key, outcome } => {
                if !matches!(self.proto_cache.get(&key), Some(ProtocolState::Encoding)) {
                    return;
                }
                let state = match outcome {
                    Ok(protocol) => ProtocolState::Ready(protocol),
                    Err(error) => ProtocolState::Failed(error),
                };
                self.proto_cache.insert(key.clone(), state);
                touch_lru(&mut self.proto_cache_order, &key);
                true
            }
        };
        if updated {
            self.redraw_requested = true;
        }
    }

    pub(crate) fn take_redraw_request(&mut self) -> bool {
        std::mem::take(&mut self.redraw_requested)
    }

    pub(crate) fn take_edit_config_request(&mut self) -> bool {
        std::mem::take(&mut self.edit_config_requested)
    }

    /// Called by the main loop after the editor process exits.
    pub(crate) fn apply_editor_result(&mut self, result: std::io::Result<()>) {
        match result {
            Ok(()) => self.reload_config(),
            Err(e) => self.status = Status::Info(format!("editor launch failed: {e}")),
        }
    }

    fn reload_config(&mut self) {
        let path = self.config_path.clone();
        match TuiConfig::load_or_create_at(path) {
            Ok(config) => {
                self.shortcuts = config.shortcuts;
                self.colors = config.colors;
                self.display = config.display;
                self.redraw_requested = true;
                self.status = Status::Info("config reloaded".to_owned());
            }
            Err(e) => self.status = Status::Info(format!("config reload failed: {e}")),
        }
    }

    /// Returns true when the user is mid-command in a transient input mode
    /// or has an account lifecycle request in flight, where unsolicited status
    /// updates would overwrite an active prompt/progress message or disrupt
    /// in-progress text entry.
    pub(crate) fn is_mid_command(&self) -> bool {
        self.lifecycle_busy
            || matches!(
                self.mode,
                Mode::LoginUsername
                    | Mode::LoginPassword { .. }
                    | Mode::RecoveryKey { .. }
                    | Mode::ConfirmLogout { .. }
                    | Mode::ConfirmDelete { .. }
                    | Mode::Editing { .. }
                    | Mode::Reacting { .. }
                    | Mode::Unreacting { .. }
                    | Mode::Search(..)
            )
    }

    pub(crate) fn dismiss_input_help(&mut self) {
        self.show_input_help = false;
    }

    /// Replace the account list. Only Active accounts go into `accounts.accounts`
    /// (for display and navigation); inactive IDs are recorded separately so
    /// `is_known_inactive_account` can still filter their rooms off the room list.
    pub(crate) fn set_accounts(&mut self, accounts: Vec<AccountDto>) {
        let selected_account_id = self.active_account_filter();
        self.accounts.inactive_ids = accounts
            .iter()
            .filter(|a| a.state != AccountState::Active)
            .map(|a| a.account_id)
            .collect();
        let active: Vec<AccountDto> = accounts
            .iter()
            .filter(|a| {
                a.state == AccountState::Active
                    && self
                        .account_filter
                        .is_none_or(|account_id| a.account_id == account_id)
            })
            .cloned()
            .collect();
        self.accounts.client_visible = accounts;
        self.accounts.accounts = active;
        self.accounts.selected = selected_account_id
            .and_then(|account_id| {
                self.accounts
                    .accounts
                    .iter()
                    .position(|account| account.account_id == account_id)
            })
            .map(AccountSelection::Account)
            .unwrap_or(AccountSelection::All);
    }

    pub(crate) fn accounts_panel_visible(&self) -> bool {
        !self.accounts_panel_hidden && self.accounts.accounts.len() >= 2
    }

    pub(crate) fn rooms_panel_visible(&self) -> bool {
        !self.rooms_panel_hidden
    }

    pub(crate) fn toggle_accounts_panel(&mut self) {
        self.accounts_panel_hidden = !self.accounts_panel_hidden;
        if self.accounts_panel_hidden
            && matches!(
                self.mode,
                Mode::AccountList | Mode::Search(SearchKind::Accounts, _)
            )
        {
            self.mode = Mode::Compose;
        }
    }

    pub(crate) fn toggle_rooms_panel(&mut self) {
        self.rooms_panel_hidden = !self.rooms_panel_hidden;
        if self.rooms_panel_hidden
            && matches!(
                self.mode,
                Mode::RoomList | Mode::Search(SearchKind::Rooms, _)
            )
        {
            self.mode = Mode::Compose;
        }
    }

    pub(crate) fn adjust_accounts_width(&mut self, delta: i16) {
        const MIN: u16 = 10;
        const MAX: u16 = 60;
        self.display.accounts_panel_width =
            (self.display.accounts_panel_width as i16 + delta).clamp(MIN as i16, MAX as i16) as u16;
    }

    pub(crate) fn adjust_rooms_width(&mut self, delta: i16) {
        self.display.rooms_panel_width_adj = self
            .display
            .rooms_panel_width_adj
            .saturating_add(delta)
            .clamp(-50, 50);
    }

    pub(crate) fn adjust_input_lines(&mut self, delta: i16) {
        self.display.input_lines = (self.display.input_lines as i16 + delta).clamp(1, 10) as u16;
    }

    pub(crate) fn active_account_filter(&self) -> Option<Uuid> {
        match self.accounts.selected {
            AccountSelection::All => None,
            AccountSelection::Account(idx) => self.accounts.accounts.get(idx).map(|a| a.account_id),
        }
    }

    pub(crate) fn toggle_unread_filter(&mut self) {
        self.unread_filter = !self.unread_filter;
        self.sync_room_selection_to_account_filter();
    }

    pub(crate) fn visible_room_indices(&self) -> Vec<usize> {
        let filter = self.active_account_filter();
        let selected = self.rooms.selected;
        self.rooms
            .rooms
            .iter()
            .enumerate()
            .filter(|(_, r)| filter.is_none_or(|id| r.account_id == id))
            .filter(|(i, r)| {
                !self.unread_filter
                    || selected == Some(*i)
                    || self
                        .rooms
                        .unread
                        .get(&RoomKey::from(*r))
                        .copied()
                        .unwrap_or(0)
                        > 0
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn insert_char(&mut self, ch: char) {
        self.input.react_command_completion = None;
        self.input.partial_room_completions = None;
        self.input.room_command_completion = None;
        self.input.logout_command_completion = None;
        self.input.recover_command_completion = None;
        self.input.delete_command_completion = None;
        self.input.account_command_completion = None;
        self.input.buffer.insert(self.input.cursor, ch);
        self.input.cursor += ch.len_utf8();
    }

    pub(crate) fn backspace(&mut self) {
        self.input.react_command_completion = None;
        self.input.partial_room_completions = None;
        self.input.room_command_completion = None;
        self.input.logout_command_completion = None;
        self.input.recover_command_completion = None;
        self.input.delete_command_completion = None;
        self.input.account_command_completion = None;
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
        self.input.react_command_completion = None;
        self.input.partial_room_completions = None;
        self.input.room_command_completion = None;
        self.input.logout_command_completion = None;
        self.input.recover_command_completion = None;
        self.input.delete_command_completion = None;
        self.input.account_command_completion = None;
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

    pub(crate) fn move_cursor_word_left(&mut self) {
        let s = &self.input.buffer[..self.input.cursor];
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let mut i = chars.len();
        while i > 0 && chars[i - 1].1.is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].1.is_whitespace() {
            i -= 1;
        }
        self.input.cursor = chars.get(i).map(|(idx, _)| *idx).unwrap_or(0);
    }

    pub(crate) fn move_cursor_word_right(&mut self) {
        let s = &self.input.buffer[self.input.cursor..];
        let chars: Vec<(usize, char)> = s.char_indices().collect();
        let mut i = 0;
        while i < chars.len() && !chars[i].1.is_whitespace() {
            i += 1;
        }
        while i < chars.len() && chars[i].1.is_whitespace() {
            i += 1;
        }
        let advance = chars.get(i).map(|(idx, _)| *idx).unwrap_or(s.len());
        self.input.cursor += advance;
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
        let tracks_response =
            !matches!(&command, Command::Send(_) | Command::Empty | Command::Quit);
        self.pending_command_response = None;
        self.handle_command_inner(command).await;
        if tracks_response {
            self.queue_completed_command_response();
        }
    }

    pub(crate) fn queue_completed_command_response(&mut self) {
        if self.mode != Mode::Compose || self.is_mid_command() {
            return;
        }
        let response = self.status.text(self.display.debug);
        if !response.is_empty() {
            self.pending_command_response = Some(response);
        }
    }

    async fn handle_command_inner(&mut self, command: Command) {
        match command {
            Command::Login {
                username,
                password,
                homeserver,
            } => {
                self.start_login(username, password, homeserver).await;
            }
            Command::Logout(target) => self.start_logout(target),
            Command::Recover(target) => self.start_recover(target),
            Command::Delete(target) => self.start_delete(target),
            Command::Room(target) => self.switch_room(&target).await,
            Command::Account(target) => {
                if self.switch_account(&target) {
                    self.load_selected_timeline().await;
                }
            }
            Command::Status => self.open_popup(PopupKind::Status),
            Command::Event(event_id) => self.show_event(&event_id).await,
            Command::Whoami => self.show_whoami(),
            Command::Whereami => self.show_whereami(),
            Command::React(None) => {
                self.select_most_recent_message_if_needed();
                self.start_react_to_selected_message();
            }
            Command::React(Some(input)) => {
                let (event_id, reaction_key) = match self.prepare_reaction(&input) {
                    Ok(reaction) => reaction,
                    Err(message) => {
                        self.status = Status::from(message);
                        return;
                    }
                };
                self.send_react(&event_id, &reaction_key).await;
            }
            Command::Unreact => {
                self.select_most_recent_message_if_needed();
                self.start_unreact_from_selected_message().await;
            }
            Command::Reply => {
                self.select_most_recent_message_if_needed();
                self.start_reply_to_selected_message();
            }
            Command::Thread => {
                self.select_most_recent_message_if_needed();
                self.start_thread_from_selected_message();
            }
            Command::Help => self.open_popup(PopupKind::Help),
            Command::Shortcuts => self.open_popup(PopupKind::Shortcuts),
            Command::Refresh => {
                self.refresh_rooms().await;
                self.redraw_requested = true;
            }
            Command::EditConfig => {
                self.edit_config_requested = true;
            }
            Command::SaveConfig => {
                match TuiConfig::save_display(
                    &self.config_path,
                    self.display.input_lines,
                    self.display.accounts_panel_width,
                    self.display.rooms_panel_width_adj,
                ) {
                    Ok(()) => {
                        self.status =
                            Status::Info(format!("config saved to {}", self.config_path.display()))
                    }
                    Err(e) => self.status = Status::Info(format!("save failed: {e}")),
                }
            }
            Command::Quit => self.should_quit = true,
            Command::Send(body) => self.send_message_to_room(&body),
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

    pub(crate) fn open_selected_media_preview(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::Info("select an image message first".to_owned());
            return;
        };
        let Some((account_id, mxc_url)) = event.image_mxc() else {
            self.status = Status::Info("selected message has no image".to_owned());
            return;
        };
        let encrypted = event.image_is_encrypted();
        self.request_image(account_id, mxc_url, encrypted);
        self.open_popup(PopupKind::MediaPreview);
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

    pub(crate) fn start_reply_to_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::from("select a displayed message before replying".to_owned());
            return;
        };
        if event.event_id.starts_with("local-echo:") {
            self.status = Status::from(PENDING_ECHO_MSG.to_owned());
            return;
        }
        self.status = Status::EventAction {
            debug: format!("reply to {} waits for the Axon write API", event.event_id),
            redacted: "reply to message waits for the Axon write API",
        };
    }

    fn select_most_recent_message_if_needed(&mut self) {
        if self.selected_message_event().is_some() {
            return;
        }
        self.messages.selection = self
            .selected_events()
            .last()
            .map(|event| event.event_id.clone());
    }

    fn prepare_reaction(&mut self, input: &str) -> Result<(String, String), String> {
        self.select_most_recent_message_if_needed();
        let event_id = self
            .selected_message_id()
            .map(str::to_owned)
            .ok_or_else(|| "no displayed messages".to_owned())?;
        if event_id.starts_with("local-echo:") {
            return Err(PENDING_ECHO_MSG.to_owned());
        }
        let reaction_key = self
            .take_reaction_key(input)
            .ok_or_else(|| format!("unknown or ambiguous emoji: {input}"))?;
        Ok((event_id, reaction_key))
    }

    pub(crate) fn start_thread_from_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status =
                Status::from("select a displayed message before starting a thread".to_owned());
            return;
        };
        if event.event_id.starts_with("local-echo:") {
            self.status = Status::from(PENDING_ECHO_MSG.to_owned());
            return;
        }
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
        if event.event_id.starts_with("local-echo:") {
            self.status = Status::from(PENDING_ECHO_MSG.to_owned());
            return;
        }
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

    fn send_message_to_room(&mut self, body: &str) {
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from("select a room before sending".to_owned());
            return;
        };
        let Some(tx) = self.lifecycle_tx.clone() else {
            return;
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let sender = room
            .account_user_id
            .clone()
            .or_else(|| self.live.own_senders.get(&room.account_id).cloned())
            .or_else(|| {
                // Fallback: account_user_id is always present on AccountDto even
                // when the room DTO omits it (e.g. the server hasn't joined yet).
                self.accounts
                    .accounts
                    .iter()
                    .find(|a| a.account_id == room.account_id)
                    .map(|a| a.user_id.clone())
            })
            .unwrap_or_default();
        // Optimistic echo: insert before spawning so the message appears on the
        // very next frame. The spawned task delivers the real event_id (or error)
        // back via the lifecycle channel without blocking the render loop.
        let temp_id = format!("local-echo:{now_ms}");
        let key = RoomKey {
            account_id: room.account_id,
            room_id: room.room_id.clone(),
        };
        let echo = EventDto {
            account_id: room.account_id,
            event_id: temp_id.clone(),
            room_id: room.room_id.clone(),
            sender,
            state_key: None,
            origin_ts: now_ms,
            event_type: "m.room.message".to_owned(),
            content: None,
            body: Some(body.to_owned()),
            relates_to: None,
            redacted: false,
            redaction_event_id: None,
        };
        self.messages.scroll = usize::MAX;
        // Clear selection so the next message-targeted command auto-selects
        // rather than staying on whatever message was selected before the send.
        self.messages.selection = None;
        self.messages
            .events
            .entry(key.clone())
            .or_default()
            .push(echo);

        let client = self.client.clone();
        let body = body.to_owned();
        tokio::spawn(async move {
            let result = client
                .send_message(room.account_id, &room.room_id, &body)
                .await
                .map(|r| r.event_id)
                .map_err(|e| e.to_string());
            let _ = tx.send(LifecycleOutcome::MessageSent {
                key,
                temp_id,
                result,
            });
        });
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
        if event.event_id.starts_with("local-echo:") {
            self.status = Status::from(PENDING_ECHO_MSG.to_owned());
            return;
        }
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

pub(crate) fn cycle_index(current: usize, len: usize, reverse: bool) -> usize {
    if reverse {
        (current + len - 1) % len
    } else {
        (current + 1) % len
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

/// Returns the next/previous value from `matches` (a sorted list of source-list indices)
/// relative to `current`, with optional wrap-around.
pub(crate) fn next_match_index(
    matches: &[usize],
    current: Option<usize>,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    if forward {
        let after = current.map(|i| i + 1).unwrap_or(0);
        let found = matches.iter().copied().find(|&i| i >= after);
        if found.is_some() || !wrap {
            found
        } else {
            matches.first().copied()
        }
    } else {
        let before = current.unwrap_or(0);
        let found = matches.iter().copied().rev().find(|&i| i < before);
        if found.is_some() || !wrap {
            found
        } else {
            matches.last().copied()
        }
    }
}

pub(crate) fn match_status(match_num: usize, total: usize) -> Status {
    Status::from(format!("match {} of {}", match_num, total))
}

fn touch_lru<K: Clone + Eq>(order: &mut VecDeque<K>, key: &K) {
    if let Some(index) = order.iter().position(|candidate| candidate == key) {
        order.remove(index);
    }
    order.push_back(key.clone());
}

fn evict_lru_where<K, V>(
    cache: &mut HashMap<K, V>,
    order: &mut VecDeque<K>,
    limit: usize,
    can_evict: impl Fn(&V) -> bool,
) -> bool
where
    K: Clone + Eq + std::hash::Hash,
{
    if cache.len() < limit {
        return true;
    }
    let Some(index) = order
        .iter()
        .position(|key| cache.get(key).is_some_and(&can_evict))
    else {
        return false;
    };
    let Some(oldest) = order.remove(index) else {
        return false;
    };
    cache.remove(&oldest);
    true
}

/// Apply the EXIF orientation tag to `img` so it displays upright, matching
/// what other Matrix clients show. `load_from_memory` decodes raw pixels but
/// ignores EXIF, so without this correction rotated JPEGs appear sideways.
/// Returns `img` unchanged if EXIF is absent, unreadable, or already upright.
pub(super) fn apply_exif_orientation(
    img: image::DynamicImage,
    bytes: &[u8],
) -> image::DynamicImage {
    use std::io::Cursor;
    let orientation = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
        .and_then(|exif| {
            exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1);

    // EXIF orientation values 1–8; 1 = already upright.
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Identify an image format from raw magic bytes without relying on the
/// compiled-in feature set of the `image` crate. Returns a short description
/// including a hex dump of the first bytes for truly unrecognised content.
pub(super) fn sniff_format(bytes: &[u8]) -> String {
    if bytes.starts_with(b"\xFF\xD8\xFF") {
        return "JPEG".into();
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1A\n") {
        return "PNG".into();
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "GIF".into();
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return "WebP".into();
    }
    if bytes.starts_with(b"BM") {
        return "BMP".into();
    }
    if bytes.starts_with(b"II\x2A\x00") || bytes.starts_with(b"MM\x00\x2A") {
        return "TIFF".into();
    }
    // ISO Base Media File Format container: AVIF, HEIC, HEIF, MP4, …
    if bytes.get(4..8) == Some(b"ftyp") {
        return match bytes.get(8..12) {
            Some(b"avif") | Some(b"avis") => "AVIF".into(),
            Some(b"heic") | Some(b"heis") | Some(b"heim") | Some(b"heix") => "HEIC".into(),
            Some(b"mif1") | Some(b"msf1") => "HEIF".into(),
            _ => "ISO BMFF (AVIF/HEIC/MP4/…)".into(),
        };
    }
    if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") || bytes.starts_with(b"<SVG") {
        return "SVG (not supported)".into();
    }
    if bytes.starts_with(b"\x00\x00\x01\x00") {
        return "ICO".into();
    }
    // Not a recognised image format — could be a JSON/HTML error body served
    // with a 2xx status. Show the first bytes so the cause is obvious.
    let prefix: String = bytes
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let printable: String = bytes
        .iter()
        .take(16)
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("unknown — first bytes: {prefix}  ({printable})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AccountDto, AccountState};
    use crate::command::HELP_COMMANDS;
    use crate::ui::{entry_status_text, popup_shortcuts_lines, popup_status_lines};
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

    fn reaction_event(event_id: &str, sender: &str, target: &str, key: &str) -> EventDto {
        let mut event = event_with_id(event_id, "m.reaction", None, serde_json::json!({}));
        event.sender = sender.to_owned();
        event.relates_to = Some(serde_json::json!({
            "rel_type": "m.annotation",
            "event_id": target,
            "key": key
        }));
        event
    }

    fn app_with_rooms(rooms: Vec<RoomDto>) -> App {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
            Picker::halfblocks(),
        );
        app.rooms.rooms = rooms;
        app.show_input_help = false;
        app.status = Status::Info(String::new());
        app
    }

    fn account(user_id: &str, state: AccountState) -> AccountDto {
        account_with_id(Uuid::from_u128(1), user_id, state)
    }

    fn account_with_id(account_id: Uuid, user_id: &str, state: AccountState) -> AccountDto {
        AccountDto {
            account_id,
            user_id: user_id.to_owned(),
            state,
            verified: Some(false),
        }
    }

    #[test]
    fn media_cache_keys_are_account_scoped() {
        let url = "mxc://example.com/media".to_owned();

        assert_ne!(
            MediaKey::new(Uuid::from_u128(1), url.clone()),
            MediaKey::new(Uuid::from_u128(2), url)
        );
    }

    #[test]
    fn bounded_cache_never_evicts_in_flight_work() {
        let mut cache = HashMap::from([("ready".to_owned(), false), ("fetching".to_owned(), true)]);
        let mut order = VecDeque::from(["ready".to_owned(), "fetching".to_owned()]);

        assert!(evict_lru_where(&mut cache, &mut order, 2, |in_flight| {
            !*in_flight
        }));
        assert!(!cache.contains_key("ready"));
        assert!(cache.contains_key("fetching"));

        cache.insert("encoding".to_owned(), true);
        order.push_back("encoding".to_owned());
        assert!(!evict_lru_where(&mut cache, &mut order, 2, |in_flight| {
            !*in_flight
        }));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn account_refresh_preserves_selected_account_by_id() {
        let first_id = Uuid::from_u128(1);
        let selected_id = Uuid::from_u128(2);
        let added_id = Uuid::from_u128(3);
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(first_id, "@first:example.com", AccountState::Active),
            account_with_id(selected_id, "@selected:example.com", AccountState::Active),
        ]);
        app.accounts.selected = AccountSelection::Account(1);

        app.set_accounts(vec![
            account_with_id(selected_id, "@selected:example.com", AccountState::Active),
            account_with_id(added_id, "@added:example.com", AccountState::Active),
        ]);

        assert_eq!(app.active_account_filter(), Some(selected_id));
        assert_eq!(app.accounts.selected, AccountSelection::Account(0));
    }

    #[test]
    fn cli_account_filter_restricts_account_navigation_state() {
        let filter_id = Uuid::from_u128(1);
        let other_id = Uuid::from_u128(2);
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            Some(filter_id),
            TuiConfig::test_default(),
            Picker::halfblocks(),
        );

        app.set_accounts(vec![
            account_with_id(filter_id, "@filtered:example.com", AccountState::Active),
            account_with_id(other_id, "@other:example.com", AccountState::Active),
        ]);

        assert_eq!(app.accounts.accounts.len(), 1);
        assert_eq!(app.accounts.accounts[0].account_id, filter_id);
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
    fn room_refresh_drops_rooms_for_logged_out_accounts() {
        let active_id = Uuid::from_u128(1);
        let logged_out_id = Uuid::from_u128(2);

        let mut active_room = room("!active:example.com", None, Some("Active"));
        active_room.account_id = active_id;
        let mut stale_room = room("!stale:example.com", None, Some("Stale"));
        stale_room.account_id = logged_out_id;

        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            AccountDto {
                account_id: active_id,
                user_id: "@alice:example.com".to_owned(),
                state: AccountState::Active,
                verified: Some(false),
            },
            AccountDto {
                account_id: logged_out_id,
                user_id: "@bob:example.com".to_owned(),
                state: AccountState::Deactivated,
                verified: Some(false),
            },
        ]);

        app.apply_room_refresh(vec![active_room, stale_room]);

        assert_eq!(
            app.rooms
                .rooms
                .iter()
                .map(|room| room.room_id.as_str())
                .collect::<Vec<_>>(),
            vec!["!active:example.com"]
        );
    }

    #[test]
    fn status_lists_active_and_deactivated_accounts() {
        let active_id = Uuid::from_u128(1);
        let logged_out_id = Uuid::from_u128(2);
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(active_id, "@alice:example.com", AccountState::Active),
            account_with_id(logged_out_id, "@bob:example.com", AccountState::Deactivated),
        ]);

        assert_eq!(
            app.accounts
                .accounts
                .iter()
                .map(|account| account.account_id)
                .collect::<Vec<_>>(),
            vec![active_id],
            "active navigation remains active-only"
        );

        let status = popup_status_lines(&app).join("\n");
        assert!(status.contains("@alice:example.com  (logged in, 0 rooms)"));
        assert!(status.contains("@bob:example.com  (logged out, 0 rooms)"));
    }

    #[test]
    fn status_disambiguates_duplicate_matrix_ids_with_account_ids() {
        let first_id = Uuid::from_u128(1);
        let second_id = Uuid::from_u128(2);
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(first_id, "@alice:example.com", AccountState::Active),
            account_with_id(second_id, "@alice:example.com", AccountState::Deactivated),
        ]);

        let status = popup_status_lines(&app).join("\n");
        assert!(status.contains(&format!("@alice:example.com  [{first_id}]  (logged in")));
        assert!(status.contains(&format!("@alice:example.com  [{second_id}]  (logged out")));
    }

    #[test]
    fn room_refresh_keeps_rooms_for_accounts_not_yet_listed() {
        // An empty/stale account list must not blank the whole room list.
        let mut unknown_room = room("!unknown:example.com", None, Some("Unknown"));
        unknown_room.account_id = Uuid::from_u128(9);
        let mut app = app_with_rooms(Vec::new());

        app.apply_room_refresh(vec![unknown_room]);

        assert_eq!(app.rooms.rooms.len(), 1);
    }

    #[test]
    fn filtered_room_refresh_does_not_select_a_hidden_room() {
        let visible_account = Uuid::from_u128(1);
        let other_account = Uuid::from_u128(2);
        let mut other_room = room("!other:example.com", None, Some("Other"));
        other_room.account_id = other_account;
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(
                visible_account,
                "@visible:example.com",
                AccountState::Active,
            ),
            account_with_id(other_account, "@other:example.com", AccountState::Active),
        ]);
        app.accounts.selected = AccountSelection::Account(0);

        app.apply_room_refresh(vec![other_room]);

        assert_eq!(app.rooms.selected, None);
        assert!(app.selected_room().is_none());
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

        assert_eq!(
            app.resolve_room_target("test"),
            RoomTargetResolution::Match(0)
        );
        assert_eq!(
            app.resolve_room_target("#test"),
            RoomTargetResolution::Match(0)
        );
        assert_eq!(
            app.resolve_room_target("TEST"),
            RoomTargetResolution::Match(0)
        );
    }

    #[test]
    pub(crate) fn find_room_matches_one_based_room_list_number() {
        let app = app_with_rooms(vec![
            room("!one:example.com", Some("#one:example.com"), Some("One")),
            room("!two:example.com", Some("#two:example.com"), Some("Two")),
        ]);

        assert_eq!(app.resolve_room_target("1"), RoomTargetResolution::Match(0));
        assert_eq!(app.resolve_room_target("2"), RoomTargetResolution::Match(1));
        assert_eq!(app.resolve_room_target("0"), RoomTargetResolution::Missing);
        assert_eq!(app.resolve_room_target("3"), RoomTargetResolution::Missing);
    }

    #[test]
    fn room_resolution_ignores_rooms_hidden_by_account_filter() {
        let visible_account = Uuid::from_u128(1);
        let hidden_account = Uuid::from_u128(2);
        let mut visible = room("!visible:example.com", None, Some("General"));
        visible.account_id = visible_account;
        let mut hidden = room("!hidden:example.com", None, Some("General"));
        hidden.account_id = hidden_account;
        let mut app = app_with_rooms(vec![visible, hidden]);
        app.set_accounts(vec![
            account_with_id(
                visible_account,
                "@visible:example.com",
                AccountState::Active,
            ),
            account_with_id(hidden_account, "@hidden:example.com", AccountState::Active),
        ]);
        app.accounts.selected = AccountSelection::Account(0);

        assert_eq!(
            app.resolve_room_target("General"),
            RoomTargetResolution::Match(0)
        );
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
            confirm_logout: true,
            search_wrap: true,
            accounts_panel_width: 25,
            rooms_panel_width_adj: 0,
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

        // /refresh both refreshes rooms (status reflects that) and queues a redraw
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

    #[tokio::test]
    async fn slash_command_response_waits_for_layout_fit_check() {
        let mut app = app_with_rooms(Vec::new());

        app.handle_command(Command::Whoami).await;

        assert_eq!(
            app.pending_command_response.as_deref(),
            Some("select a room before using /whoami")
        );
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

    #[tokio::test]
    async fn message_action_commands_target_most_recent_message_without_selection() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$older:example.com",
                    "m.room.message",
                    Some("older"),
                    serde_json::json!({ "msgtype": "m.text", "body": "older" }),
                ),
                event_with_id(
                    "$newest:example.com",
                    "m.room.message",
                    Some("newest"),
                    serde_json::json!({ "msgtype": "m.text", "body": "newest" }),
                ),
            ],
        );

        app.handle_command(Command::React(None)).await;
        assert_eq!(app.selected_message_id(), Some("$newest:example.com"));
        assert_eq!(
            app.mode,
            Mode::Reacting {
                event_id: "$newest:example.com".to_owned()
            }
        );

        app.mode = Mode::Compose;
        app.messages.selection = None;
        app.handle_command(Command::Reply).await;
        assert_eq!(app.selected_message_id(), Some("$newest:example.com"));
        assert_eq!(
            app.status,
            "reply to $newest:example.com waits for the Axon write API"
        );

        app.messages.selection = None;
        app.handle_command(Command::Thread).await;
        assert_eq!(app.selected_message_id(), Some("$newest:example.com"));
        assert_eq!(
            app.status,
            "thread from $newest:example.com waits for the Axon write API"
        );
    }

    #[tokio::test]
    async fn message_action_commands_preserve_an_existing_selection() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$selected:example.com",
                    "m.room.message",
                    Some("selected"),
                    serde_json::json!({ "msgtype": "m.text", "body": "selected" }),
                ),
                event_with_id(
                    "$newest:example.com",
                    "m.room.message",
                    Some("newest"),
                    serde_json::json!({ "msgtype": "m.text", "body": "newest" }),
                ),
            ],
        );
        app.messages.selection = Some("$selected:example.com".to_owned());

        app.handle_command(Command::React(None)).await;

        assert_eq!(app.selected_message_id(), Some("$selected:example.com"));
        assert_eq!(
            app.mode,
            Mode::Reacting {
                event_id: "$selected:example.com".to_owned()
            }
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

    #[test]
    fn in_flight_lifecycle_status_ignores_live_socket_updates() {
        let mut app = app_with_rooms(Vec::new());
        app.lifecycle_busy = true;
        app.status = Status::Info("logging in @alice:example.com…".to_owned());

        let action = app.handle_live_frame(LiveFrame::Reconnecting {
            reason: "connection reset".to_owned(),
            delay: std::time::Duration::from_secs(4),
        });

        assert_eq!(action, LiveFrameAction::None);
        assert_eq!(app.status.text(false), "logging in @alice:example.com…");
    }

    #[test]
    fn lifecycle_prompt_status_survives_background_room_refresh() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::LoginUsername;
        app.status = Status::Info("Matrix ID: @user:example.com".to_owned());

        app.apply_room_refresh(Vec::new());

        assert_eq!(app.status.text(false), "Matrix ID: @user:example.com");
    }

    #[test]
    fn in_flight_lifecycle_status_survives_background_room_refresh() {
        let mut app = app_with_rooms(Vec::new());
        app.lifecycle_busy = true;
        app.status = Status::Info("deleting @alice:example.com…".to_owned());

        app.apply_room_refresh(Vec::new());

        assert_eq!(app.status.text(false), "deleting @alice:example.com…");
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
        app.input.buffer = "/room room".to_owned();
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
    async fn arrow_up_from_compose_enters_message_list_mode() {
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

        // Up from no selection: jump to the last message and enter MessageList mode
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        assert!(matches!(app.mode, Mode::MessageList));

        // Up again (now in MessageList): move to the previous message
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "");
        assert_eq!(app.selected_message_id(), Some("$one:example.com"));
        assert!(matches!(app.mode, Mode::MessageList));

        // Up at the first message: stay put
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.selected_message_id(), Some("$one:example.com"));

        // Down: move forward
        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        assert!(matches!(app.mode, Mode::MessageList));

        // Down at the last message: stay put (Esc returns to Compose)
        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        assert_eq!(app.selected_message_id(), Some("$two:example.com"));
        assert!(matches!(app.mode, Mode::MessageList));
    }

    #[tokio::test]
    async fn arrow_up_with_no_messages_enters_message_list() {
        let mut app = app_with_rooms(Vec::new());
        app.handle_key(KeyEvent::from(KeyCode::Up)).await;
        assert_eq!(app.input.buffer, "");
        assert!(matches!(app.mode, Mode::MessageList));
    }

    #[tokio::test]
    async fn media_preview_hotkey_opens_popup_for_selected_image() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.selection = Some("$image:example.com".to_owned());
        app.mode = Mode::MessageList;
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![event_with_id(
                "$image:example.com",
                "m.room.message",
                Some("photo.jpg"),
                serde_json::json!({
                    "msgtype": "m.image",
                    "body": "photo.jpg",
                    "url": "mxc://example.com/photo"
                }),
            )],
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('v'))).await;

        assert_eq!(app.mode, Mode::Popup(PopupKind::MediaPreview));
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

        app.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE))
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

    #[tokio::test]
    async fn reaction_tab_completion_shows_and_cycles_matching_emoji() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::Reacting {
            event_id: "$message:example.com".to_owned(),
        };
        app.input.buffer = "face".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        let first_status = app.status.text(false);
        assert_eq!(app.input.react_tab, Some(0));
        assert!(first_status.contains("[1/"));
        assert!(first_status.contains("Tab/Shift-Tab to cycle, Enter to send"));

        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        let second_status = app.status.text(false);
        assert_eq!(app.input.react_tab, Some(1));
        assert!(second_status.contains("[2/"));
        assert_ne!(second_status, first_status);

        app.handle_key(KeyEvent::from(KeyCode::BackTab)).await;
        assert_eq!(app.input.react_tab, Some(0));
        assert_eq!(app.status.text(false), first_status);

        app.handle_key(KeyEvent::from(KeyCode::BackTab)).await;
        assert_eq!(app.input.react_tab, Some(emoji_matches("face").len() - 1));
        assert!(app.status.text(false).contains(&format!(
            "[{}/{}]",
            emoji_matches("face").len(),
            emoji_matches("face").len()
        )));
    }

    #[tokio::test]
    async fn reaction_submit_rejects_unknown_text_without_leaving_reacting_mode() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::Reacting {
            event_id: "$message:example.com".to_owned(),
        };
        app.input.buffer = "not-a-known-emoji".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(
            app.mode,
            Mode::Reacting {
                event_id: "$message:example.com".to_owned()
            }
        );
        assert_eq!(app.input.buffer, "not-a-known-emoji");
        assert_eq!(
            app.status.text(false),
            "no emoji matches 'not-a-known-emoji'"
        );
    }

    #[test]
    fn reaction_input_accepts_only_known_or_selected_emoji() {
        let mut app = app_with_rooms(Vec::new());

        assert_eq!(app.take_reaction_key("🚀"), Some("🚀".to_owned()));
        assert_eq!(app.take_reaction_key("rocket"), Some("🚀".to_owned()));
        assert_eq!(app.take_reaction_key("not-a-known-emoji"), None);

        let matches = emoji_matches("face");
        assert!(matches.len() > 1);
        assert_eq!(app.take_reaction_key("face"), None);

        app.input.react_tab = Some(1);
        assert_eq!(
            app.take_reaction_key("face"),
            Some(matches[1].as_str().to_owned())
        );
    }

    #[test]
    fn react_command_argument_prepares_immediate_reaction_for_most_recent_message() {
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

        assert_eq!(
            app.prepare_reaction("+1"),
            Ok(("$message:example.com".to_owned(), "👍".to_owned()))
        );
        assert_eq!(app.mode, Mode::Compose);
    }

    #[test]
    fn react_command_argument_rejects_unknown_emoji() {
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

        assert_eq!(
            app.prepare_reaction("not-a-known-emoji"),
            Err("unknown or ambiguous emoji: not-a-known-emoji".to_owned())
        );
        assert_eq!(app.mode, Mode::Compose);
    }

    #[test]
    fn own_reactions_group_duplicate_keys_and_ignore_other_or_redacted_reactions() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        let mut redacted = reaction_event(
            "$redacted:example.com",
            "@alice:example.com",
            "$message:example.com",
            "🚀",
        );
        redacted.redacted = true;
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                reaction_event(
                    "$one:example.com",
                    "@alice:example.com",
                    "$message:example.com",
                    "👍",
                ),
                reaction_event(
                    "$two:example.com",
                    "@alice:example.com",
                    "$message:example.com",
                    "👍",
                ),
                reaction_event(
                    "$other:example.com",
                    "@bob:example.com",
                    "$message:example.com",
                    "🎉",
                ),
                redacted,
            ],
        );

        assert_eq!(
            app.own_reactions_for("$message:example.com"),
            Ok(vec![OwnReaction {
                key: "👍".to_owned(),
                event_ids: vec!["$one:example.com".to_owned(), "$two:example.com".to_owned()],
            }])
        );
    }

    #[tokio::test]
    async fn unreact_with_multiple_reactions_enters_and_cycles_selection_mode() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.selection = Some("$message:example.com".to_owned());
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$message:example.com",
                    "m.room.message",
                    Some("message"),
                    serde_json::json!({ "msgtype": "m.text", "body": "message" }),
                ),
                reaction_event(
                    "$rocket:example.com",
                    "@alice:example.com",
                    "$message:example.com",
                    "🚀",
                ),
                reaction_event(
                    "$thumb:example.com",
                    "@alice:example.com",
                    "$message:example.com",
                    "👍",
                ),
            ],
        );

        app.start_unreact_from_selected_message().await;

        let Mode::Unreacting {
            choices, selected, ..
        } = &app.mode
        else {
            panic!("expected unreact selection mode");
        };
        assert_eq!(choices.len(), 2);
        assert_eq!(*selected, 0);
        let first_status = app.status.text(false);

        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;

        let Mode::Unreacting { selected, .. } = app.mode else {
            panic!("expected unreact selection mode");
        };
        assert_eq!(selected, 1);
        assert_ne!(app.status.text(false), first_status);

        app.handle_key(KeyEvent::from(KeyCode::BackTab)).await;

        let Mode::Unreacting { selected, .. } = app.mode else {
            panic!("expected unreact selection mode");
        };
        assert_eq!(selected, 0);
        assert_eq!(app.status.text(false), first_status);

        app.handle_key(KeyEvent::from(KeyCode::BackTab)).await;

        let Mode::Unreacting { selected, .. } = app.mode else {
            panic!("expected unreact selection mode");
        };
        assert_eq!(selected, 1);

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;
        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.status.text(false), "unreact canceled");
    }

    #[tokio::test]
    async fn unreact_hotkey_targets_selected_message() {
        let room = room("!room:example.com", Some("#room:example.com"), Some("Room"));
        let mut app = app_with_rooms(vec![room.clone()]);
        app.rooms.selected = Some(0);
        app.messages.selection = Some("$message:example.com".to_owned());
        app.mode = Mode::MessageList;
        app.messages.events.insert(
            RoomKey::from(&room),
            vec![
                event_with_id(
                    "$message:example.com",
                    "m.room.message",
                    Some("message"),
                    serde_json::json!({ "msgtype": "m.text", "body": "message" }),
                ),
                reaction_event(
                    "$rocket:example.com",
                    "@alice:example.com",
                    "$message:example.com",
                    "🚀",
                ),
                reaction_event(
                    "$thumb:example.com",
                    "@alice:example.com",
                    "$message:example.com",
                    "👍",
                ),
            ],
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('U'), KeyModifiers::SHIFT))
            .await;

        assert!(matches!(app.mode, Mode::Unreacting { .. }));
    }

    #[test]
    fn redacted_reactions_are_not_rendered_in_counts() {
        let mut visible = reaction_event(
            "$visible:example.com",
            "@alice:example.com",
            "$message:example.com",
            "👍",
        );
        let mut redacted = visible.clone();
        redacted.event_id = "$redacted:example.com".to_owned();
        redacted.redacted = true;

        let reactions = collect_reactions(&[visible.clone(), redacted]);

        assert_eq!(
            reactions.get("$message:example.com"),
            Some(&vec![("👍".to_owned(), 1)])
        );
        visible.redacted = true;
        assert!(collect_reactions(&[visible]).is_empty());
    }

    #[test]
    fn room_completion_fills_unique_room_alias_match() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", Some("#one:example.com"), Some("One")),
            room("!test:example.com", Some("#test:example.com"), Some("Test")),
        ]);
        app.input.buffer = "/room te".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room #test:example.com");
    }

    #[test]
    fn room_completion_ignores_rooms_hidden_by_account_filter() {
        let visible_account = Uuid::from_u128(1);
        let hidden_account = Uuid::from_u128(2);
        let mut visible = room("!visible:example.com", None, Some("General"));
        visible.account_id = visible_account;
        let mut hidden = room("!hidden:example.com", None, Some("General"));
        hidden.account_id = hidden_account;
        let mut app = app_with_rooms(vec![visible, hidden]);
        app.set_accounts(vec![
            account_with_id(
                visible_account,
                "@visible:example.com",
                AccountState::Active,
            ),
            account_with_id(hidden_account, "@hidden:example.com", AccountState::Active),
        ]);
        app.accounts.selected = AccountSelection::Account(0);
        app.input.buffer = "/room Gen".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room General");
        assert!(app.input.room_command_completion.is_none());
    }

    #[test]
    fn tab_completion_keeps_parsed_command_aliases_discoverable() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/roo".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/roo");
        assert!(app.status.text(false).contains("/room, /rooms"));

        app.input.buffer = "/sw".to_owned();
        app.complete_input();

        assert_eq!(app.input.buffer, "/switch ");
    }

    #[tokio::test]
    async fn account_search_accepts_n_and_uppercase_n_as_query_text() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::Search(SearchKind::Accounts, "a".to_owned());

        app.handle_key(KeyEvent::from(KeyCode::Char('n'))).await;
        app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT))
            .await;

        assert_eq!(
            app.mode,
            Mode::Search(SearchKind::Accounts, "anN".to_owned())
        );
    }

    #[tokio::test]
    async fn submitting_account_search_selects_first_match() {
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(
                Uuid::from_u128(1),
                "@alice:example.com",
                AccountState::Active,
            ),
            account_with_id(Uuid::from_u128(2), "@bob:example.com", AccountState::Active),
        ]);
        app.mode = Mode::Search(SearchKind::Accounts, "bob".to_owned());

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.mode, Mode::AccountList);
        assert_eq!(app.accounts.selected, AccountSelection::Account(1));
        assert_eq!(app.last_search.as_deref(), Some("bob"));
    }

    #[tokio::test]
    async fn submitting_account_search_reports_no_match() {
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![account_with_id(
            Uuid::from_u128(1),
            "@alice:example.com",
            AccountState::Active,
        )]);
        app.mode = Mode::Search(SearchKind::Accounts, "missing".to_owned());

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.accounts.selected, AccountSelection::All);
        assert_eq!(app.status, "no account matches: missing");
    }

    #[test]
    fn account_numbers_match_panel_labels() {
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(
                Uuid::from_u128(1),
                "@alice:example.com",
                AccountState::Active,
            ),
            account_with_id(Uuid::from_u128(2), "@bob:example.com", AccountState::Active),
        ]);

        assert!(app.switch_account("0"));
        assert_eq!(app.accounts.selected, AccountSelection::All);
        assert!(app.switch_account("2"));
        assert_eq!(app.accounts.selected, AccountSelection::Account(1));
        assert_eq!(AccountSelection::All.display_number(), 0);
        assert_eq!(AccountSelection::Account(1).display_number(), 2);

        app.accounts.selected = AccountSelection::All;
        assert!(app.commit_account_search("2".to_owned()));
        assert_eq!(app.accounts.selected, AccountSelection::Account(1));
    }

    #[test]
    fn logout_completion_cycles_only_matching_active_accounts() {
        let mut app = app_with_rooms(Vec::new());
        app.accounts.accounts = vec![
            account("@alice:example.com", AccountState::Active),
            account("@alice:work.example", AccountState::Active),
            account("@bob:example.com", AccountState::Active),
            account("@alice:old.example", AccountState::Deactivated),
        ];
        app.input.buffer = "/logout alice".to_owned();

        app.complete_input();
        assert_eq!(app.input.buffer, "/logout @alice:example.com");
        assert!(app.status.text(false).contains("[1/2]"));

        app.complete_input();
        assert_eq!(app.input.buffer, "/logout @alice:work.example");
        assert!(app.status.text(false).contains("[2/2]"));

        app.complete_input_reverse();
        assert_eq!(app.input.buffer, "/logout @alice:example.com");
    }

    #[test]
    fn logout_completion_without_target_cycles_all_active_accounts() {
        let mut app = app_with_rooms(Vec::new());
        app.accounts.accounts = vec![
            account("@alice:example.com", AccountState::Active),
            account("@bob:example.com", AccountState::Active),
            account("@old:example.com", AccountState::Deactivated),
        ];
        app.input.buffer = "/logout".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/logout @alice:example.com");
        assert!(app.status.text(false).contains("[1/2]"));
    }

    #[test]
    fn logout_completion_normalizes_server_qualified_username_forms() {
        let mut app = app_with_rooms(Vec::new());
        app.accounts.accounts = vec![account("@alice:example.com", AccountState::Active)];

        app.input.buffer = "/logout alice:example.com".to_owned();
        app.complete_input();
        assert_eq!(app.input.buffer, "/logout @alice:example.com");

        app.input.logout_command_completion = None;
        app.input.buffer = "/logout alice@example.com".to_owned();
        app.complete_input();
        assert_eq!(app.input.buffer, "/logout @alice:example.com");
    }

    #[test]
    fn logout_completion_selects_duplicate_user_ids_by_account_id() {
        let first_id = Uuid::from_u128(1);
        let second_id = Uuid::from_u128(2);
        let mut app = app_with_rooms(Vec::new());
        app.accounts.accounts = vec![
            account_with_id(first_id, "@alice:example.com", AccountState::Active),
            account_with_id(second_id, "@alice:example.com", AccountState::Active),
        ];
        app.input.buffer = "/logout alice".to_owned();

        app.complete_input();
        assert_eq!(app.input.buffer, format!("/logout {first_id}"));

        app.complete_input();
        assert_eq!(app.input.buffer, format!("/logout {second_id}"));
        assert!(matches!(
            app.resolve_logout_target(Some(&second_id.to_string())),
            super::lifecycle::LogoutResolution::Match(AccountDto { account_id, .. })
                if account_id == second_id
        ));
    }

    #[test]
    fn delete_completion_selects_duplicate_user_ids_by_account_id() {
        let first_id = Uuid::from_u128(1);
        let second_id = Uuid::from_u128(2);
        let mut app = app_with_rooms(Vec::new());
        app.accounts.accounts = vec![
            account_with_id(first_id, "@alice:example.com", AccountState::Active),
            account_with_id(second_id, "@alice:example.com", AccountState::Deactivated),
        ];
        app.input.buffer = "/delete alice".to_owned();

        app.complete_input();
        assert_eq!(app.input.buffer, format!("/delete {first_id}"));

        app.complete_input();
        assert_eq!(app.input.buffer, format!("/delete {second_id}"));
        assert!(matches!(
            app.resolve_delete_target(Some(&second_id.to_string())),
            super::lifecycle::DeleteResolution::Match(AccountDto { account_id, .. })
                if account_id == second_id
        ));
    }

    #[test]
    fn logout_prompts_for_confirmation_when_enabled() {
        let mut app = app_with_rooms(Vec::new());
        app.display.confirm_logout = true;

        app.request_logout(account("@alice:example.com", AccountState::Active));

        assert!(matches!(app.mode, Mode::ConfirmLogout { .. }));
        assert!(app
            .status
            .text(false)
            .contains("Log out @alice:example.com"));
    }

    #[test]
    fn logout_skips_confirmation_when_disabled() {
        let mut app = app_with_rooms(Vec::new());
        app.display.confirm_logout = false;

        app.request_logout(account("@alice:example.com", AccountState::Active));

        // Without a lifecycle sender the spawned logout is a no-op in tests, but
        // we should never have entered the confirmation prompt.
        assert!(!matches!(app.mode, Mode::ConfirmLogout { .. }));
        assert_eq!(app.mode, Mode::Compose);
    }

    #[tokio::test]
    async fn logout_confirmation_cancels_on_no() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::ConfirmLogout {
            account: account("@alice:example.com", AccountState::Active),
        };

        app.handle_key(KeyEvent::from(KeyCode::Char('n'))).await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.status.text(false), "");
    }

    #[tokio::test]
    async fn logout_confirmation_ignores_unrelated_keys() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::ConfirmLogout {
            account: account("@alice:example.com", AccountState::Active),
        };

        app.handle_key(KeyEvent::from(KeyCode::Char('x'))).await;

        assert!(matches!(app.mode, Mode::ConfirmLogout { .. }));
    }

    #[tokio::test]
    async fn delete_confirmation_non_yes_enter_clears_buffer() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::ConfirmDelete {
            account: account("@alice:example.com", AccountState::Active),
        };
        app.input.buffer = "no".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn delete_confirmation_escape_clears_buffer() {
        let mut app = app_with_rooms(Vec::new());
        app.mode = Mode::ConfirmDelete {
            account: account("@alice:example.com", AccountState::Active),
        };
        app.input.buffer = "YES".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn in_flight_lifecycle_rejects_new_login_and_logout() {
        let mut app = app_with_rooms(Vec::new());
        app.lifecycle_busy = true;

        app.handle_command(Command::Login {
            username: None,
            password: None,
            homeserver: None,
        })
        .await;
        assert_eq!(app.mode, Mode::Compose);
        assert!(app.status.text(false).contains("already in progress"));

        app.status = Status::Info(String::new());
        app.handle_command(Command::Logout(None)).await;
        assert!(app.status.text(false).contains("already in progress"));
    }

    #[tokio::test]
    async fn login_without_arguments_prompts_for_username_and_escape_clears_it() {
        let mut app = app_with_rooms(Vec::new());

        app.handle_command(Command::Login {
            username: None,
            password: None,
            homeserver: None,
        })
        .await;
        assert_eq!(app.mode, Mode::LoginUsername);

        app.input.buffer = "@alice:example.com".to_owned();
        app.input.cursor = app.input.buffer.len();
        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert!(app.input.buffer.is_empty());
        assert_eq!(app.status.text(false), "");
    }

    #[tokio::test]
    async fn empty_recovery_key_skips_post_login_recovery() {
        let mut app = app_with_rooms(Vec::new());
        let account = account("@alice:example.com", AccountState::Active);
        app.mode = Mode::RecoveryKey {
            account,
            origin: RecoveryOrigin::PostLogin,
        };

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(
            app.status.text(false),
            "recovery skipped for @alice:example.com"
        );
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn escape_cancels_standalone_recovery_and_clears_secret() {
        let mut app = app_with_rooms(Vec::new());
        let account = account("@alice:example.com", AccountState::Active);
        app.mode = Mode::RecoveryKey {
            account,
            origin: RecoveryOrigin::Command,
        };
        app.input.buffer = "secret recovery key".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(
            app.status.text(false),
            "recovery cancelled for @alice:example.com"
        );
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn account_navigation_clears_recovery_key_input() {
        let mut app = app_with_rooms(Vec::new());
        app.set_accounts(vec![
            account_with_id(
                Uuid::from_u128(1),
                "@alice:example.com",
                AccountState::Active,
            ),
            account_with_id(Uuid::from_u128(2), "@bob:example.com", AccountState::Active),
        ]);
        app.mode = Mode::RecoveryKey {
            account: account("@alice:example.com", AccountState::Active),
            origin: RecoveryOrigin::Command,
        };
        app.input.buffer = "secret recovery key".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT))
            .await;

        assert_eq!(app.mode, Mode::Compose);
        assert!(app.input.buffer.is_empty());
    }

    #[test]
    fn recover_resolution_uses_active_accounts_and_uuid_for_duplicates() {
        let first_id = Uuid::from_u128(1);
        let second_id = Uuid::from_u128(2);
        let mut app = app_with_rooms(Vec::new());
        app.accounts.accounts = vec![
            account_with_id(first_id, "@alice:example.com", AccountState::Active),
            account_with_id(second_id, "@alice:example.com", AccountState::Active),
            account_with_id(
                Uuid::from_u128(3),
                "@bob:example.com",
                AccountState::Deactivated,
            ),
        ];

        assert!(matches!(
            app.resolve_recover_target(Some(&second_id.to_string())),
            super::lifecycle::RecoverResolution::Match(AccountDto { account_id, .. })
                if account_id == second_id
        ));
        assert!(matches!(
            app.resolve_recover_target(Some("bob")),
            super::lifecycle::RecoverResolution::Missing
        ));

        app.input.buffer = "/recover alice".to_owned();
        app.complete_input();
        assert_eq!(app.input.buffer, format!("/recover {first_id}"));
        app.complete_input();
        assert_eq!(app.input.buffer, format!("/recover {second_id}"));
    }

    #[tokio::test]
    async fn invalid_login_username_stays_editable() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "alice".to_owned();
        app.input.cursor = app.input.buffer.len();
        app.mode = Mode::LoginUsername;

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.mode, Mode::LoginUsername);
        assert_eq!(app.input.buffer, "alice");
        assert!(app.status.text(false).contains("name@domain"));
    }

    #[tokio::test]
    async fn login_username_prompt_canonicalizes_common_email_style() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "alice@example.com".to_owned();
        app.input.cursor = app.input.buffer.len();
        app.mode = Mode::LoginUsername;

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(
            app.mode,
            Mode::LoginPassword {
                username: "@alice:example.com".to_owned(),
                homeserver: None,
            }
        );
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn login_username_prompt_captures_optional_homeserver() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "@alice:example.com hs.example.org".to_owned();
        app.input.cursor = app.input.buffer.len();
        app.mode = Mode::LoginUsername;

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(
            app.mode,
            Mode::LoginPassword {
                username: "@alice:example.com".to_owned(),
                homeserver: Some("https://hs.example.org".to_owned()),
            }
        );
    }

    #[tokio::test]
    async fn login_username_prompt_rejects_extra_tokens() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "@alice:example.com hs.example.org junk".to_owned();
        app.input.cursor = app.input.buffer.len();
        app.mode = Mode::LoginUsername;

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        // Stays on the username step with the input intact for correction.
        assert_eq!(app.mode, Mode::LoginUsername);
        assert_eq!(app.input.buffer, "@alice:example.com hs.example.org junk");
        assert!(app.status.text(false).contains("at most"));
    }

    #[test]
    fn tab_completion_fills_argument_slash_command_with_space() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/acco".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/account ");
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
    fn tab_completion_fills_react_command_with_argument_space() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/rea".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/react ");
    }

    #[test]
    fn tab_completion_cycles_emoji_matches_after_react_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/react face".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.complete_input();
        let first = app.input.buffer.clone();
        assert!(first.starts_with("/react "));
        assert!(app.status.text(false).contains("[1/"));

        app.complete_input();
        let second = app.input.buffer.clone();
        assert!(app.status.text(false).contains("[2/"));
        assert_ne!(second, first);
    }

    #[tokio::test]
    async fn shift_tab_cycles_react_command_emoji_matches_backward() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/react face".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        let first = app.input.buffer.clone();
        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        assert_ne!(app.input.buffer, first);

        app.handle_key(KeyEvent::from(KeyCode::BackTab)).await;
        assert_eq!(app.input.buffer, first);
        assert!(app.status.text(false).contains("[1/"));

        app.handle_key(KeyEvent::from(KeyCode::BackTab)).await;
        let match_count = emoji_matches("face").len();
        assert!(app
            .status
            .text(false)
            .contains(&format!("[{match_count}/{match_count}]")));
    }

    #[tokio::test]
    async fn compose_tab_completes_react_emoji_and_edit_resets_cycle() {
        let mut app = app_with_rooms(Vec::new());
        for ch in "/react face".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(ch))).await;
        }

        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        assert!(app.input.react_command_completion.is_some());
        assert!(app.input.buffer.starts_with("/react "));

        app.handle_key(KeyEvent::from(KeyCode::Char('x'))).await;
        assert!(app.input.react_command_completion.is_none());
    }

    #[test]
    fn react_command_emoji_completion_reports_no_matches() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/react not-a-known-emoji".to_owned();

        app.complete_input();

        assert_eq!(
            app.status.text(false),
            "no emoji matches 'not-a-known-emoji'"
        );
        assert_eq!(app.input.buffer, "/react not-a-known-emoji");
    }

    #[test]
    fn tab_completion_fills_unreact_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/unr".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/unreact");
    }

    #[test]
    fn tab_completion_reports_ambiguous_slash_command() {
        let mut app = app_with_rooms(Vec::new());
        app.input.buffer = "/".to_owned();

        app.complete_input();

        assert_eq!(app.input.buffer, "/");
        assert!(app.status.text(false).contains("/room"));
        assert!(app.status.text(false).contains("/status"));
        assert!(app.status.text(false).contains("/event"));
        assert!(app.status.text(false).contains("/whoami"));
        assert!(app.status.text(false).contains("/whereami"));
        assert!(app.status.text(false).contains("/react"));
        assert!(app.status.text(false).contains("/unreact"));
        assert!(app.status.text(false).contains("/reply"));
        assert!(app.status.text(false).contains("/thread"));
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
        app.mode = Mode::Popup(PopupKind::CommandResponse);
        app.pending_command_response = Some("full command response".to_owned());

        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        assert_eq!(app.popup_scroll, 1);

        app.handle_key(KeyEvent::from(KeyCode::PageDown)).await;
        assert_eq!(app.popup_scroll, 9);

        app.handle_key(KeyEvent::from(KeyCode::PageUp)).await;
        assert_eq!(app.popup_scroll, 1);

        app.handle_key(KeyEvent::from(KeyCode::Esc)).await;
        assert_eq!(app.popup_scroll, 0);
        assert_eq!(app.mode, Mode::Compose);
        assert!(app.pending_command_response.is_none());
    }

    #[tokio::test]
    async fn help_popup_selects_command_into_input() {
        let mut app = app_with_rooms(Vec::new());
        app.handle_command(Command::Help).await;

        app.handle_key(KeyEvent::from(KeyCode::Down)).await;
        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.mode, Mode::Compose);
        assert_eq!(app.input.buffer, "//");
        assert_eq!(app.input.cursor, "//".len());
        assert_eq!(app.status.text(false), "selected command: //<text>");
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

        assert!(text.contains("F6"));
        assert!(text.contains("Ctrl-N"));
        assert!(text.contains("Ctrl-P"));
        assert!(text.contains("Ctrl-J"));
        assert!(text.contains("Ctrl-K"));
        assert!(text.contains("PageUp"));
        assert!(text.contains("PageDown"));
        assert!(text.contains("select previous message"));
        assert!(text.contains("select next message"));
    }

    #[test]
    pub(crate) fn new_app_starts_with_one_time_input_help() {
        let app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
            Picker::halfblocks(),
        );

        assert!(app.show_input_help);
        assert!(app.input.buffer.is_empty());
    }

    #[tokio::test]
    async fn first_input_action_dismisses_input_help() {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
            Picker::halfblocks(),
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('/'))).await;

        assert!(!app.show_input_help);
        assert_eq!(app.input.buffer, "/");
    }

    #[tokio::test]
    async fn room_switch_shortcut_dismisses_input_help_when_no_rooms_exist() {
        let mut app = App::new(
            AxonClient::new("http://127.0.0.1:8080".to_owned(), None),
            None,
            TuiConfig::test_default(),
            Picker::halfblocks(),
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
    fn room_completion_adds_missing_hash_for_qualified_alias() {
        let mut app = app_with_rooms(vec![room(
            "!test:example.com",
            Some("#test:example.com"),
            Some("Test"),
        )]);
        app.input.buffer = "/room test:ex".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room #test:example.com");
    }

    #[test]
    fn room_completion_reports_ambiguous_room_matches() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", Some("#test:example.com"), Some("Test")),
            room(
                "!two:example.com",
                Some("#testing:example.com"),
                Some("Testing"),
            ),
        ]);
        app.input.buffer = "/room test".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room test");
        assert!(app.status.text(false).contains("#test:example.com"));
        assert!(app.status.text(false).contains("#testing:example.com"));
    }

    #[test]
    fn room_completion_extends_to_common_prefix_and_shows_suffixes() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", None, Some("axontest")),
            room("!two:example.com", None, Some("axondev")),
        ]);
        app.input.buffer = "/room ax".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room axon");
        assert!(app.status.text(false).contains("test"));
        assert!(app.status.text(false).contains("dev"));
    }

    #[tokio::test]
    async fn enter_does_not_submit_partial_switch_completion() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", None, Some("axontest")),
            room("!two:example.com", None, Some("axondev")),
        ]);
        app.input.buffer = "/room ax".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        assert_eq!(app.input.buffer, "/room axon");
        assert_eq!(
            app.input.partial_room_completions,
            Some(vec!["test".to_owned(), "dev".to_owned()])
        );

        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;

        assert_eq!(app.input.buffer, "/room axon");
        assert_eq!(app.rooms.selected, None);
        assert_eq!(
            app.status.text(false),
            "room completion is partial: test, dev - type more or press Tab"
        );

        app.handle_key(KeyEvent::from(KeyCode::Char('t'))).await;
        assert!(app.input.partial_room_completions.is_none());
    }

    #[test]
    fn room_completion_uses_matching_names_when_rooms_have_aliases() {
        let mut app = app_with_rooms(vec![
            room(
                "!one:example.com",
                Some("#test:example.com"),
                Some("axontest"),
            ),
            room(
                "!two:example.com",
                Some("#dev:example.com"),
                Some("axondev"),
            ),
        ]);
        app.input.buffer = "/room ax".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room axon");
        assert!(app.status.text(false).contains("test"));
        assert!(app.status.text(false).contains("dev"));
    }

    #[test]
    fn room_completion_still_completes_unique_match_fully() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", None, Some("axontest")),
            room("!two:example.com", None, Some("axondev")),
        ]);
        app.input.buffer = "/room axont".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room axontest");
    }

    #[test]
    fn room_completion_replaces_unique_name_match_with_canonical_alias() {
        let mut app = app_with_rooms(vec![
            room(
                "!one:example.com",
                Some("#test:example.com"),
                Some("axontest"),
            ),
            room(
                "!two:example.com",
                Some("#dev:example.com"),
                Some("axondev"),
            ),
        ]);
        app.input.buffer = "/room axont".to_owned();

        app.complete_room_input(false);

        assert_eq!(app.input.buffer, "/room #test:example.com");
    }

    #[test]
    fn room_completion_cycles_duplicate_named_rooms_with_disambiguator() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", None, Some("General")),
            room("!two:example.com", None, Some("General")),
        ]);
        app.input.buffer = "/room General".to_owned();

        app.complete_room_input(false);
        assert_eq!(app.input.buffer, "/room !one:example.com");
        assert!(app.status.text(false).contains("[1/2]"));
        assert!(app.status.text(false).contains("General"));
        assert!(app.status.text(false).contains("!one:example.com"));
        assert!(app.status.text(false).contains("Tab/Shift-Tab to cycle"));

        app.complete_room_input(false);
        assert_eq!(app.input.buffer, "/room !two:example.com");
        assert!(app.status.text(false).contains("[2/2]"));
        assert!(app.status.text(false).contains("!two:example.com"));

        app.complete_room_input(true);
        assert_eq!(app.input.buffer, "/room !one:example.com");
        assert!(app.status.text(false).contains("[1/2]"));
    }

    #[test]
    fn room_completion_deduplicates_same_room_across_accounts() {
        // Regression: the same Matrix room joined by two accounts appears twice
        // in the room list (one per account_id). If one account hasn't synced the
        // canonical_alias state event yet, the room shows up as both
        // "#scratch:example.com" and "scratch", producing a spurious third match.
        // visible_rooms_for_completion deduplicates by room_id, keeping the entry
        // with a canonical alias.
        let mut account_b_entry = room("!scratch:example.com", None, Some("scratch"));
        account_b_entry.account_id = Uuid::from_u128(2);

        let mut app = app_with_rooms(vec![
            room(
                "!scratch:example.com",
                Some("#scratch:example.com"),
                Some("scratch"),
            ),
            room(
                "!scratch2:example.com",
                Some("#scratch-2:example.com"),
                Some("scratch-2"),
            ),
            account_b_entry,
        ]);
        app.input.buffer = "/room scratch".to_owned();

        app.complete_room_input(false);

        // Should see exactly 2 candidates, not 3.
        let status = app.status.text(false);
        assert!(
            status.contains("#scratch:example.com"),
            "expected alias in status: {status}"
        );
        assert!(
            status.contains("#scratch-2:example.com"),
            "expected alias-2 in status: {status}"
        );
        assert!(
            !status.contains("completions: #scratch:example.com, #scratch-2:example.com, scratch"),
            "spurious bare 'scratch' entry in status: {status}"
        );
    }

    #[tokio::test]
    async fn room_completion_enter_selects_after_prefix_expansion_then_cycling() {
        // Regression: partial_room_completions set during prefix expansion must be
        // cleared when cycling begins, otherwise Enter is incorrectly blocked.
        let mut app = app_with_rooms(vec![
            room("!one:example.com", None, Some("General")),
            room("!two:example.com", None, Some("General")),
        ]);
        app.input.buffer = "/room G".to_owned();
        app.input.cursor = app.input.buffer.len();

        // First Tab: prefix-expands "G" → "General", sets partial_room_completions
        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        assert_eq!(app.input.buffer, "/room General");
        assert!(app.input.partial_room_completions.is_some());

        // Second Tab: enters cycling mode — partial_room_completions must be cleared
        app.handle_key(KeyEvent::from(KeyCode::Tab)).await;
        assert!(app.input.buffer.starts_with("/room !"));
        assert!(app.input.partial_room_completions.is_none());

        // Enter must not be blocked
        app.handle_key(KeyEvent::from(KeyCode::Enter)).await;
        assert!(app.rooms.selected.is_some());
    }

    #[test]
    fn room_completion_typing_after_cycling_resets_to_normal_completion() {
        let mut app = app_with_rooms(vec![
            room("!one:example.com", None, Some("General")),
            room("!two:example.com", None, Some("General")),
        ]);
        app.input.buffer = "/room General".to_owned();
        app.input.cursor = app.input.buffer.len();

        app.complete_room_input(false);
        assert!(app.input.room_command_completion.is_some());

        app.insert_char('x');
        assert!(app.input.room_command_completion.is_none());
    }

    #[test]
    fn room_resolution_accepts_unique_name_prefix() {
        let app = app_with_rooms(vec![
            room(
                "!one:example.com",
                Some("#test:example.com"),
                Some("axontest"),
            ),
            room(
                "!two:example.com",
                Some("#dev:example.com"),
                Some("axondev"),
            ),
        ]);

        assert_eq!(
            app.resolve_room_target("axont"),
            RoomTargetResolution::Match(0)
        );
        assert_eq!(
            app.resolve_room_target("axon"),
            RoomTargetResolution::Ambiguous(vec!["test".to_owned(), "dev".to_owned()])
        );
    }

    #[tokio::test]
    async fn switch_command_reports_ambiguous_name_suffixes() {
        let mut app = app_with_rooms(vec![
            room(
                "!one:example.com",
                Some("#test:example.com"),
                Some("axontest"),
            ),
            room(
                "!two:example.com",
                Some("#dev:example.com"),
                Some("axondev"),
            ),
        ]);

        app.handle_command(Command::Room("axon".to_owned())).await;

        assert_eq!(app.status.text(false), "room name is ambiguous: test, dev");
        assert_eq!(app.rooms.selected, None);
    }

    #[test]
    fn room_completion_only_runs_for_switch_command() {
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

        assert_eq!(
            app.resolve_room_target("test:example.com"),
            RoomTargetResolution::Match(0)
        );
        assert_eq!(
            app.resolve_room_target("test:other.example"),
            RoomTargetResolution::Missing
        );
    }

    #[test]
    pub(crate) fn find_room_keeps_exact_alias_and_name_matches() {
        let app = app_with_rooms(vec![room(
            "!abc:example.com",
            Some("#test:example.com"),
            Some("Friendly Name"),
        )]);

        assert_eq!(
            app.resolve_room_target("#test:example.com"),
            RoomTargetResolution::Match(0)
        );
        assert_eq!(
            app.resolve_room_target("friendly name"),
            RoomTargetResolution::Match(0)
        );
    }

    #[test]
    pub(crate) fn find_room_does_not_local_match_fully_qualified_wrong_server() {
        let app = app_with_rooms(vec![room(
            "!abc:example.com",
            Some("#test:example.com"),
            Some("Test Room"),
        )]);

        assert_eq!(
            app.resolve_room_target("#test:other.example"),
            RoomTargetResolution::Missing
        );
    }
}
