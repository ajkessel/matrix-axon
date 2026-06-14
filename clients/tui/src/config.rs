use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde::Deserialize;
use thiserror::Error;

pub const DEFAULT_CONFIG: &str = r#"# axon-tui configuration.
#
# Key names are case-insensitive. Supported forms include:
#   ctrl-n, ctrl-p, ctrl-c, ctrl-j, ctrl-k, ctrl-space, tab, enter, esc,
#   backspace, ctrl-a, ctrl-e, home, end, up, down, left, right,
#   pageup, pagedown, space, r, t, shift-r
#
# Color names are case-insensitive. Supported names:
#   black, red, green, yellow, blue, magenta, cyan, gray,
#   dark-gray, light-red, light-green, light-yellow, light-blue,
#   light-magenta, light-cyan, white

[shortcuts]
next_room = "ctrl-n"
previous_room = "ctrl-p"
next_account = "alt-n"
previous_account = "alt-p"
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
confirm_logout = true
"#;

#[derive(Debug, Clone)]
pub struct TuiConfig {
    pub shortcuts: Shortcuts,
    pub colors: ColorScheme,
    pub display: DisplayOptions,
    pub path: PathBuf,
    pub created_default: bool,
}

impl TuiConfig {
    pub fn load_or_create_default() -> Result<Self, ConfigError> {
        let path = config_path()?;
        Self::load_or_create_at(path)
    }

    pub fn load_or_create_at(path: PathBuf) -> Result<Self, ConfigError> {
        let created_default = ensure_default_config(&path)?;
        let text = fs::read_to_string(&path)?;
        let raw = RawConfig::load_with_defaults(&text)?;
        let repaired = raw.to_toml();
        if repaired != text {
            fs::write(&path, repaired)?;
        }
        Ok(Self {
            shortcuts: raw.shortcuts.into_shortcuts()?,
            colors: raw.colors.into_color_scheme()?,
            display: raw.display.into_display_options()?,
            path,
            created_default,
        })
    }

    #[cfg(test)]
    pub fn test_default() -> Self {
        let raw = RawConfig::default_values();
        Self {
            shortcuts: raw
                .shortcuts
                .into_shortcuts()
                .expect("default shortcuts parse"),
            colors: raw
                .colors
                .into_color_scheme()
                .expect("default colors parse"),
            display: raw
                .display
                .into_display_options()
                .expect("default display options parse"),
            path: PathBuf::from("/tmp/axon-tui-test-config.toml"),
            created_default: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Shortcuts {
    pub next_room: KeyBinding,
    pub previous_room: KeyBinding,
    pub next_account: KeyBinding,
    pub previous_account: KeyBinding,
    pub quit: KeyBinding,
    pub complete: KeyBinding,
    pub submit: KeyBinding,
    pub clear_input: KeyBinding,
    pub backspace: KeyBinding,
    pub cursor_start: KeyBinding,
    pub cursor_end: KeyBinding,
    pub cursor_left: KeyBinding,
    pub cursor_right: KeyBinding,
    pub edit_previous: KeyBinding,
    pub edit_next: KeyBinding,
    pub message_down: KeyBinding,
    pub message_up: KeyBinding,
    pub message_page_up: KeyBinding,
    pub message_page_down: KeyBinding,
    pub reply: KeyBinding,
    pub thread: KeyBinding,
    pub edit_message: KeyBinding,
    pub redact_message: KeyBinding,
    pub react_message: KeyBinding,
    pub unreact_message: KeyBinding,
    pub focus_next: KeyBinding,
}

#[derive(Debug, Clone)]
pub struct ColorScheme {
    pub border: Color,
    pub selected_room: Color,
    pub unread_count: Color,
    pub message_sender: Color,
    pub own_message_sender: Color,
    pub input_hint: Color,
    pub status: Color,
}

#[derive(Debug, Clone)]
pub struct DisplayOptions {
    pub debug: bool,
    pub show_state_events: bool,
    pub sender_name: SenderNameStyle,
    pub input_lines: u16,
    pub confirm_logout: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenderNameStyle {
    DisplayName,
    MatrixAddress,
}

impl SenderNameStyle {
    fn as_str(self) -> &'static str {
        match self {
            Self::DisplayName => "display_name",
            Self::MatrixAddress => "matrix_address",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyBinding {
    pub fn matches(&self, key: KeyEvent) -> bool {
        self.code == key.code && self.modifiers == key.modifiers
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("Alt".to_owned());
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift".to_owned());
        }
        parts.push(match self.code {
            KeyCode::Char(' ') => "Space".to_owned(),
            KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
            KeyCode::Tab => "Tab".to_owned(),
            KeyCode::Enter => "Enter".to_owned(),
            KeyCode::Esc => "Esc".to_owned(),
            KeyCode::Backspace => "Backspace".to_owned(),
            KeyCode::Home => "Home".to_owned(),
            KeyCode::End => "End".to_owned(),
            KeyCode::Up => "Up".to_owned(),
            KeyCode::Down => "Down".to_owned(),
            KeyCode::Left => "Left".to_owned(),
            KeyCode::Right => "Right".to_owned(),
            KeyCode::PageUp => "PageUp".to_owned(),
            KeyCode::PageDown => "PageDown".to_owned(),
            _ => "?".to_owned(),
        });
        parts.join("-")
    }
}

#[derive(Debug, Clone)]
struct RawConfig {
    shortcuts: RawShortcuts,
    colors: RawColorScheme,
    display: RawDisplayOptions,
}

impl RawConfig {
    fn load_with_defaults(text: &str) -> Result<Self, ConfigError> {
        let parsed = toml::from_str::<PartialRawConfig>(text)?;
        let mut raw = Self::default_values();
        raw.shortcuts.merge(parsed.shortcuts);
        raw.colors.merge(parsed.colors);
        raw.display.merge(parsed.display);
        Ok(raw)
    }

    fn default_values() -> Self {
        Self {
            shortcuts: RawShortcuts {
                next_room: "ctrl-n".to_owned(),
                previous_room: "ctrl-p".to_owned(),
                next_account: "alt-n".to_owned(),
                previous_account: "alt-p".to_owned(),
                quit: "ctrl-c".to_owned(),
                complete: "tab".to_owned(),
                submit: "enter".to_owned(),
                clear_input: "esc".to_owned(),
                backspace: "backspace".to_owned(),
                cursor_start: "ctrl-a".to_owned(),
                cursor_end: "ctrl-e".to_owned(),
                cursor_left: "left".to_owned(),
                cursor_right: "right".to_owned(),
                edit_previous: "up".to_owned(),
                edit_next: "down".to_owned(),
                message_down: "ctrl-j".to_owned(),
                message_up: "ctrl-k".to_owned(),
                message_page_up: "pageup".to_owned(),
                message_page_down: "pagedown".to_owned(),
                reply: "r".to_owned(),
                thread: "t".to_owned(),
                edit_message: "e".to_owned(),
                redact_message: "d".to_owned(),
                react_message: "shift-r".to_owned(),
                unreact_message: "shift-u".to_owned(),
                focus_next: "ctrl-space".to_owned(),
            },
            colors: RawColorScheme {
                border: "gray".to_owned(),
                selected_room: "cyan".to_owned(),
                unread_count: "yellow".to_owned(),
                message_sender: "green".to_owned(),
                own_message_sender: "light-cyan".to_owned(),
                input_hint: "dark-gray".to_owned(),
                status: "cyan".to_owned(),
            },
            display: RawDisplayOptions {
                debug: false,
                show_state_events: false,
                sender_name: SenderNameStyle::DisplayName.as_str().to_owned(),
                input_lines: 1,
                confirm_logout: true,
            },
        }
    }

    fn to_toml(&self) -> String {
        format!(
            r#"# axon-tui configuration.
#
# Key names are case-insensitive. Supported forms include:
#   ctrl-n, ctrl-p, ctrl-c, ctrl-j, ctrl-k, ctrl-space, tab, enter, esc,
#   backspace, ctrl-a, ctrl-e, home, end, up, down, left, right,
#   pageup, pagedown, space, r, t, shift-r
#
# Color names are case-insensitive. Supported names:
#   black, red, green, yellow, blue, magenta, cyan, gray,
#   dark-gray, light-red, light-green, light-yellow, light-blue,
#   light-magenta, light-cyan, white

[shortcuts]
next_room = "{next_room}"
previous_room = "{previous_room}"
next_account = "{next_account}"
previous_account = "{previous_account}"
quit = "{quit}"
complete = "{complete}"
submit = "{submit}"
clear_input = "{clear_input}"
backspace = "{backspace}"
cursor_start = "{cursor_start}"
cursor_end = "{cursor_end}"
cursor_left = "{cursor_left}"
cursor_right = "{cursor_right}"
edit_previous = "{edit_previous}"
edit_next = "{edit_next}"
message_down = "{message_down}"
message_up = "{message_up}"
message_page_up = "{message_page_up}"
message_page_down = "{message_page_down}"
reply = "{reply}"
thread = "{thread}"
edit_message = "{edit_message}"
redact_message = "{redact_message}"
react_message = "{react_message}"
unreact_message = "{unreact_message}"
focus_next = "{focus_next}"

[colors]
border = "{border}"
selected_room = "{selected_room}"
unread_count = "{unread_count}"
message_sender = "{message_sender}"
own_message_sender = "{own_message_sender}"
input_hint = "{input_hint}"
status = "{status}"

[display]
debug = {debug}
show_state_events = {show_state_events}
sender_name = "{sender_name}"
input_lines = {input_lines}
confirm_logout = {confirm_logout}
"#,
            next_room = self.shortcuts.next_room,
            previous_room = self.shortcuts.previous_room,
            next_account = self.shortcuts.next_account,
            previous_account = self.shortcuts.previous_account,
            quit = self.shortcuts.quit,
            complete = self.shortcuts.complete,
            submit = self.shortcuts.submit,
            clear_input = self.shortcuts.clear_input,
            backspace = self.shortcuts.backspace,
            cursor_start = self.shortcuts.cursor_start,
            cursor_end = self.shortcuts.cursor_end,
            cursor_left = self.shortcuts.cursor_left,
            cursor_right = self.shortcuts.cursor_right,
            edit_previous = self.shortcuts.edit_previous,
            edit_next = self.shortcuts.edit_next,
            message_down = self.shortcuts.message_down,
            message_up = self.shortcuts.message_up,
            message_page_up = self.shortcuts.message_page_up,
            message_page_down = self.shortcuts.message_page_down,
            reply = self.shortcuts.reply,
            thread = self.shortcuts.thread,
            edit_message = self.shortcuts.edit_message,
            redact_message = self.shortcuts.redact_message,
            react_message = self.shortcuts.react_message,
            unreact_message = self.shortcuts.unreact_message,
            focus_next = self.shortcuts.focus_next,
            border = self.colors.border,
            selected_room = self.colors.selected_room,
            unread_count = self.colors.unread_count,
            message_sender = self.colors.message_sender,
            own_message_sender = self.colors.own_message_sender,
            input_hint = self.colors.input_hint,
            status = self.colors.status,
            debug = self.display.debug,
            show_state_events = self.display.show_state_events,
            sender_name = self.display.sender_name,
            input_lines = self.display.input_lines,
            confirm_logout = self.display.confirm_logout,
        )
    }
}

#[derive(Debug, Default, Deserialize)]
struct PartialRawConfig {
    shortcuts: Option<PartialRawShortcuts>,
    colors: Option<PartialRawColorScheme>,
    display: Option<PartialDisplayOptions>,
}

#[derive(Debug, Clone)]
struct RawShortcuts {
    next_room: String,
    previous_room: String,
    next_account: String,
    previous_account: String,
    quit: String,
    complete: String,
    submit: String,
    clear_input: String,
    backspace: String,
    cursor_start: String,
    cursor_end: String,
    cursor_left: String,
    cursor_right: String,
    edit_previous: String,
    edit_next: String,
    message_down: String,
    message_up: String,
    message_page_up: String,
    message_page_down: String,
    reply: String,
    thread: String,
    edit_message: String,
    redact_message: String,
    react_message: String,
    unreact_message: String,
    focus_next: String,
}

impl RawShortcuts {
    fn merge(&mut self, partial: Option<PartialRawShortcuts>) {
        let Some(partial) = partial else {
            return;
        };
        assign_if_some(&mut self.next_room, partial.next_room);
        assign_if_some(&mut self.previous_room, partial.previous_room);
        assign_if_some(&mut self.next_account, partial.next_account);
        assign_if_some(&mut self.previous_account, partial.previous_account);
        assign_if_some(&mut self.quit, partial.quit);
        assign_if_some(&mut self.complete, partial.complete);
        assign_if_some(&mut self.submit, partial.submit);
        assign_if_some(&mut self.clear_input, partial.clear_input);
        assign_if_some(&mut self.backspace, partial.backspace);
        assign_if_some(&mut self.cursor_start, partial.cursor_start);
        assign_if_some(&mut self.cursor_end, partial.cursor_end);
        assign_if_some(&mut self.cursor_left, partial.cursor_left);
        assign_if_some(&mut self.cursor_right, partial.cursor_right);
        assign_if_some(&mut self.edit_previous, partial.edit_previous);
        assign_if_some(&mut self.edit_next, partial.edit_next);
        assign_if_some(&mut self.message_down, partial.message_down);
        assign_if_some(&mut self.message_up, partial.message_up);
        assign_if_some(&mut self.message_page_up, partial.message_page_up);
        assign_if_some(&mut self.message_page_down, partial.message_page_down);
        assign_if_some(&mut self.reply, partial.reply);
        assign_if_some(&mut self.thread, partial.thread);
        assign_if_some(&mut self.edit_message, partial.edit_message);
        assign_if_some(&mut self.redact_message, partial.redact_message);
        assign_if_some(&mut self.react_message, partial.react_message);
        assign_if_some(&mut self.unreact_message, partial.unreact_message);
        assign_if_some(&mut self.focus_next, partial.focus_next);
    }

    fn into_shortcuts(self) -> Result<Shortcuts, ConfigError> {
        Ok(Shortcuts {
            next_room: parse_key_binding("shortcuts.next_room", &self.next_room)?,
            previous_room: parse_key_binding("shortcuts.previous_room", &self.previous_room)?,
            next_account: parse_key_binding("shortcuts.next_account", &self.next_account)?,
            previous_account: parse_key_binding(
                "shortcuts.previous_account",
                &self.previous_account,
            )?,
            quit: parse_key_binding("shortcuts.quit", &self.quit)?,
            complete: parse_key_binding("shortcuts.complete", &self.complete)?,
            submit: parse_key_binding("shortcuts.submit", &self.submit)?,
            clear_input: parse_key_binding("shortcuts.clear_input", &self.clear_input)?,
            backspace: parse_key_binding("shortcuts.backspace", &self.backspace)?,
            cursor_start: parse_key_binding("shortcuts.cursor_start", &self.cursor_start)?,
            cursor_end: parse_key_binding("shortcuts.cursor_end", &self.cursor_end)?,
            cursor_left: parse_key_binding("shortcuts.cursor_left", &self.cursor_left)?,
            cursor_right: parse_key_binding("shortcuts.cursor_right", &self.cursor_right)?,
            edit_previous: parse_key_binding("shortcuts.edit_previous", &self.edit_previous)?,
            edit_next: parse_key_binding("shortcuts.edit_next", &self.edit_next)?,
            message_down: parse_key_binding("shortcuts.message_down", &self.message_down)?,
            message_up: parse_key_binding("shortcuts.message_up", &self.message_up)?,
            message_page_up: parse_key_binding("shortcuts.message_page_up", &self.message_page_up)?,
            message_page_down: parse_key_binding(
                "shortcuts.message_page_down",
                &self.message_page_down,
            )?,
            reply: parse_key_binding("shortcuts.reply", &self.reply)?,
            thread: parse_key_binding("shortcuts.thread", &self.thread)?,
            edit_message: parse_key_binding("shortcuts.edit_message", &self.edit_message)?,
            redact_message: parse_key_binding("shortcuts.redact_message", &self.redact_message)?,
            react_message: parse_key_binding("shortcuts.react_message", &self.react_message)?,
            unreact_message: parse_key_binding("shortcuts.unreact_message", &self.unreact_message)?,
            focus_next: parse_key_binding("shortcuts.focus_next", &self.focus_next)?,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct PartialRawShortcuts {
    next_room: Option<String>,
    previous_room: Option<String>,
    next_account: Option<String>,
    previous_account: Option<String>,
    quit: Option<String>,
    complete: Option<String>,
    submit: Option<String>,
    clear_input: Option<String>,
    backspace: Option<String>,
    cursor_start: Option<String>,
    cursor_end: Option<String>,
    cursor_left: Option<String>,
    cursor_right: Option<String>,
    #[serde(alias = "history_previous")]
    edit_previous: Option<String>,
    #[serde(alias = "history_next")]
    edit_next: Option<String>,
    message_down: Option<String>,
    message_up: Option<String>,
    message_page_up: Option<String>,
    message_page_down: Option<String>,
    reply: Option<String>,
    thread: Option<String>,
    edit_message: Option<String>,
    redact_message: Option<String>,
    react_message: Option<String>,
    unreact_message: Option<String>,
    focus_next: Option<String>,
}

#[derive(Debug, Clone)]
struct RawColorScheme {
    border: String,
    selected_room: String,
    unread_count: String,
    message_sender: String,
    own_message_sender: String,
    input_hint: String,
    status: String,
}

impl RawColorScheme {
    fn merge(&mut self, partial: Option<PartialRawColorScheme>) {
        let Some(partial) = partial else {
            return;
        };
        assign_if_some(&mut self.border, partial.border);
        assign_if_some(&mut self.selected_room, partial.selected_room);
        assign_if_some(&mut self.unread_count, partial.unread_count);
        assign_if_some(&mut self.message_sender, partial.message_sender);
        assign_if_some(&mut self.own_message_sender, partial.own_message_sender);
        assign_if_some(&mut self.input_hint, partial.input_hint);
        assign_if_some(&mut self.status, partial.status);
    }

    fn into_color_scheme(self) -> Result<ColorScheme, ConfigError> {
        Ok(ColorScheme {
            border: parse_color("colors.border", &self.border)?,
            selected_room: parse_color("colors.selected_room", &self.selected_room)?,
            unread_count: parse_color("colors.unread_count", &self.unread_count)?,
            message_sender: parse_color("colors.message_sender", &self.message_sender)?,
            own_message_sender: parse_color("colors.own_message_sender", &self.own_message_sender)?,
            input_hint: parse_color("colors.input_hint", &self.input_hint)?,
            status: parse_color("colors.status", &self.status)?,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct PartialRawColorScheme {
    border: Option<String>,
    selected_room: Option<String>,
    unread_count: Option<String>,
    message_sender: Option<String>,
    own_message_sender: Option<String>,
    input_hint: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Clone)]
struct RawDisplayOptions {
    debug: bool,
    show_state_events: bool,
    sender_name: String,
    input_lines: u16,
    confirm_logout: bool,
}

impl RawDisplayOptions {
    fn merge(&mut self, partial: Option<PartialDisplayOptions>) {
        let Some(partial) = partial else {
            return;
        };
        if let Some(debug) = partial.debug {
            self.debug = debug;
        }
        if let Some(show_state_events) = partial.show_state_events {
            self.show_state_events = show_state_events;
        }
        if let Some(sender_name) = partial.sender_name {
            self.sender_name = sender_name;
        }
        if let Some(input_lines) = partial.input_lines {
            self.input_lines = input_lines.max(1);
        }
        if let Some(confirm_logout) = partial.confirm_logout {
            self.confirm_logout = confirm_logout;
        }
    }

    fn into_display_options(self) -> Result<DisplayOptions, ConfigError> {
        Ok(DisplayOptions {
            debug: self.debug,
            show_state_events: self.show_state_events,
            sender_name: parse_sender_name_style("display.sender_name", &self.sender_name)?,
            input_lines: self.input_lines.max(1),
            confirm_logout: self.confirm_logout,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct PartialDisplayOptions {
    debug: Option<bool>,
    show_state_events: Option<bool>,
    sender_name: Option<String>,
    input_lines: Option<u16>,
    confirm_logout: Option<bool>,
}

fn assign_if_some(target: &mut String, value: Option<String>) {
    if let Some(value) = value {
        *target = value;
    }
}

fn ensure_default_config(path: &Path) -> Result<bool, ConfigError> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, DEFAULT_CONFIG)?;
    Ok(true)
}

fn config_path() -> Result<PathBuf, ConfigError> {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(dir).join("axon-tui").join("config.toml"));
    }
    let home = env::var_os("HOME").ok_or(ConfigError::MissingHome)?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("axon-tui")
        .join("config.toml"))
}

fn parse_key_binding(field: &'static str, value: &str) -> Result<KeyBinding, ConfigError> {
    let mut modifiers = KeyModifiers::empty();
    let mut key = None;
    for part in value.split('-') {
        let part = part.trim().to_ascii_lowercase();
        match part.as_str() {
            "" => {}
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "tab" => key = Some(KeyCode::Tab),
            "enter" | "return" => key = Some(KeyCode::Enter),
            "esc" | "escape" => key = Some(KeyCode::Esc),
            "backspace" => key = Some(KeyCode::Backspace),
            "home" => key = Some(KeyCode::Home),
            "end" => key = Some(KeyCode::End),
            "up" => key = Some(KeyCode::Up),
            "down" => key = Some(KeyCode::Down),
            "left" => key = Some(KeyCode::Left),
            "right" => key = Some(KeyCode::Right),
            "pageup" | "page_up" | "pgup" => key = Some(KeyCode::PageUp),
            "pagedown" | "page_down" | "pgdn" => key = Some(KeyCode::PageDown),
            "space" => key = Some(KeyCode::Char(' ')),
            key_name if key_name.chars().count() == 1 => {
                key = key_name.chars().next().map(KeyCode::Char);
            }
            _ => {
                return Err(ConfigError::InvalidKey {
                    field,
                    value: value.to_owned(),
                })
            }
        }
    }
    let mut code = key.ok_or_else(|| ConfigError::InvalidKey {
        field,
        value: value.to_owned(),
    })?;
    if modifiers.contains(KeyModifiers::SHIFT) {
        if let KeyCode::Char(ch) = code {
            code = KeyCode::Char(ch.to_ascii_uppercase());
        }
    }
    Ok(KeyBinding { code, modifiers })
}

fn parse_color(field: &'static str, value: &str) -> Result<Color, ConfigError> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "black" => Ok(Color::Black),
        "red" => Ok(Color::Red),
        "green" => Ok(Color::Green),
        "yellow" => Ok(Color::Yellow),
        "blue" => Ok(Color::Blue),
        "magenta" => Ok(Color::Magenta),
        "cyan" => Ok(Color::Cyan),
        "gray" | "grey" => Ok(Color::Gray),
        "dark-gray" | "dark-grey" => Ok(Color::DarkGray),
        "light-red" => Ok(Color::LightRed),
        "light-green" => Ok(Color::LightGreen),
        "light-yellow" => Ok(Color::LightYellow),
        "light-blue" => Ok(Color::LightBlue),
        "light-magenta" => Ok(Color::LightMagenta),
        "light-cyan" => Ok(Color::LightCyan),
        "white" => Ok(Color::White),
        _ => Err(ConfigError::InvalidColor {
            field,
            value: value.to_owned(),
        }),
    }
}

fn parse_sender_name_style(
    field: &'static str,
    value: &str,
) -> Result<SenderNameStyle, ConfigError> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "display_name" | "displayname" | "name" => Ok(SenderNameStyle::DisplayName),
        "matrix_address" | "matrix_id" | "mxid" | "address" => Ok(SenderNameStyle::MatrixAddress),
        _ => Err(ConfigError::InvalidDisplayOption {
            field,
            value: value.to_owned(),
        }),
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine home directory for axon-tui config")]
    MissingHome,
    #[error("config file I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("config TOML is invalid: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid key binding for {field}: {value}")]
    InvalidKey { field: &'static str, value: String },
    #[error("invalid color for {field}: {value}")]
    InvalidColor { field: &'static str, value: String },
    #[error("invalid display option for {field}: {value}")]
    InvalidDisplayOption { field: &'static str, value: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_default_config_when_missing() {
        let path =
            env::temp_dir().join(format!("axon-tui-test-{}-config.toml", std::process::id()));
        let _ = fs::remove_file(&path);

        let config = TuiConfig::load_or_create_at(path.clone()).expect("load");

        assert!(config.created_default);
        assert!(path.exists());
        assert!(fs::read_to_string(&path).unwrap().contains("[shortcuts]"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_default_config() {
        let raw = RawConfig::load_with_defaults(DEFAULT_CONFIG).expect("default config parses");
        let shortcuts = raw.shortcuts.into_shortcuts().expect("shortcuts");

        assert!(shortcuts
            .next_room
            .matches(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn repairs_config_missing_newer_fields() {
        let path = env::temp_dir().join(format!(
            "axon-tui-test-{}-repair-config.toml",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            r#"[shortcuts]
next_room = "ctrl-n"
previous_room = "ctrl-p"
quit = "ctrl-c"
complete = "tab"
submit = "enter"
clear_input = "esc"
backspace = "backspace"

[colors]
border = "gray"
selected_room = "cyan"
unread_count = "yellow"
message_sender = "green"
input_hint = "dark-gray"
status = "cyan"
"#,
        )
        .expect("write old config");

        let config = TuiConfig::load_or_create_at(path.clone()).expect("load repaired config");

        assert!(!config.created_default);
        assert!(config
            .shortcuts
            .cursor_start
            .matches(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        let repaired = fs::read_to_string(&path).expect("read repaired config");
        assert!(repaired.contains("cursor_start = \"ctrl-a\""));
        assert!(repaired.contains("edit_next = \"down\""));
        assert!(repaired.contains("message_down = \"ctrl-j\""));
        assert!(repaired.contains("message_page_up = \"pageup\""));
        assert!(repaired.contains("thread = \"t\""));
        assert!(repaired.contains("unreact_message = \"shift-u\""));
        assert!(repaired.contains("focus_next = \"ctrl-space\""));
        assert!(repaired.contains("own_message_sender = \"light-cyan\""));
        assert!(repaired.contains("[display]"));
        assert!(repaired.contains("debug = false"));
        assert!(repaired.contains("show_state_events = false"));
        assert!(repaired.contains("sender_name = \"display_name\""));
        assert!(repaired.contains("confirm_logout = true"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn accepts_legacy_history_shortcut_keys() {
        let path = env::temp_dir().join(format!(
            "axon-tui-test-{}-legacy-history-config.toml",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            r#"[shortcuts]
history_previous = "ctrl-k"
history_next = "ctrl-j"
"#,
        )
        .expect("write legacy config");

        let config = TuiConfig::load_or_create_at(path.clone()).expect("load legacy config");

        assert!(config
            .shortcuts
            .edit_previous
            .matches(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)));
        assert!(config
            .shortcuts
            .edit_next
            .matches(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)));
        let repaired = fs::read_to_string(&path).expect("read repaired config");
        assert!(repaired.contains("edit_previous = \"ctrl-k\""));
        assert!(repaired.contains("edit_next = \"ctrl-j\""));
        assert!(!repaired.contains("history_previous"));
        assert!(!repaired.contains("history_next"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parse_error_does_not_overwrite_existing_config() {
        let path = env::temp_dir().join(format!(
            "axon-tui-test-{}-bad-config.toml",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let original = r#"[display]
input_lines = "one"
"#;
        fs::write(&path, original).expect("write invalid config");

        let err = TuiConfig::load_or_create_at(path.clone()).expect_err("invalid TOML type");

        assert!(matches!(err, ConfigError::Toml(_)));
        assert_eq!(
            fs::read_to_string(&path).expect("read invalid config"),
            original
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn parses_sender_name_style() {
        assert_eq!(
            parse_sender_name_style("display.sender_name", "display-name").unwrap(),
            SenderNameStyle::DisplayName
        );
        assert_eq!(
            parse_sender_name_style("display.sender_name", "matrix_address").unwrap(),
            SenderNameStyle::MatrixAddress
        );
        assert!(parse_sender_name_style("display.sender_name", "unknown").is_err());
    }

    #[test]
    fn parses_color_names() {
        assert_eq!(
            parse_color("colors.status", "light-cyan").unwrap(),
            Color::LightCyan
        );
        assert_eq!(
            parse_color("colors.status", "dark_gray").unwrap(),
            Color::DarkGray
        );
    }
}
