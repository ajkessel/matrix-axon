mod api;
mod app;
mod args;
mod command;
mod config;
mod html;
mod keymap;
mod ui;
mod wrap;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use api::{websocket_task, AxonClient};
use app::{App, LiveFrameAction};
use args::{normalize_token, Args};
use config::TuiConfig;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use ratatui_image::picker::{Picker, ProtocolType};
use tokio::sync::mpsc;
use tokio::time;
use ui::draw;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    let config = TuiConfig::load_or_create_default()?;
    let base_url = args
        .base_url
        .or_else(|| config.base_url.clone())
        .unwrap_or_else(|| "http://127.0.0.1:8080".to_owned());
    let token = args.token.or_else(|| normalize_token(config.token.clone()));
    if let Some(ref t) = token {
        if reqwest::header::HeaderValue::from_str(&api::bearer_value_str(t)).is_err() {
            eprintln!(
                "Error: the bearer token contains characters that are invalid in an HTTP header \
                 value (e.g. control characters or non-ASCII bytes).\n\
                 \nCheck your config file ({path}) or AXON_TOKEN environment variable.",
                path = config.path.display()
            );
            std::process::exit(1);
        }
    }
    let client = AxonClient::new(base_url, token.clone());

    if token.is_none() {
        if let Err(api::ApiError::Status { status, .. }) = client.list_accounts().await {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                eprintln!(
                    "Error: this Axon server requires authentication but no bearer token is configured.\n\
                     \nMint a token on the server:\n\
                     \n    axon token issue --label <name>\n\
                     \nThen add it to your config file ({path}):\n\
                     \n    [server]\n    bearer_token = \"<token>\"\n\
                     \nOr supply it at launch:\n\
                     \n    axon-tui --token <token>\n    AXON_TOKEN=<token> axon-tui",
                    path = config.path.display()
                );
                std::process::exit(1);
            }
        }
    }

    // Do not use Picker::from_query_stdio(): on terminals that don't answer its
    // capability query, ratatui-image leaves a detached stdin reader behind.
    // That reader can race the TUI input thread and silently consume keystrokes.
    // Use safe environment hints (or an explicit override) and fall back to
    // halfblocks, which works everywhere without touching stdin.
    let picker = terminal_image_picker();
    let mut terminal = TerminalGuard::enter()?;
    let result = run_app(
        &mut terminal.terminal,
        client,
        args.account_id,
        config,
        picker,
    )
    .await;
    terminal.leave()?;
    result
}

fn terminal_image_picker() -> Picker {
    let mut picker = Picker::halfblocks();
    let protocol = std::env::var("AXON_IMAGE_PROTOCOL")
        .ok()
        .and_then(|value| parse_image_protocol(&value))
        .or_else(detect_image_protocol_from_env);
    if let Some(protocol) = protocol {
        picker.set_protocol_type(protocol);
    }
    picker
}

fn parse_image_protocol(value: &str) -> Option<ProtocolType> {
    match value.trim().to_ascii_lowercase().as_str() {
        "halfblocks" | "half-blocks" => Some(ProtocolType::Halfblocks),
        "kitty" => Some(ProtocolType::Kitty),
        "sixel" => Some(ProtocolType::Sixel),
        "iterm2" | "iterm" => Some(ProtocolType::Iterm2),
        _ => None,
    }
}

fn detect_image_protocol_from_env() -> Option<ProtocolType> {
    if std::env::var_os("AXON_NO_IMAGE_QUERY").is_some() {
        return None;
    }
    if std::env::var_os("ITERM_SESSION_ID").is_some()
        || std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || std::env::var("TERM_PROGRAM").is_ok_and(|value| value == "iTerm.app")
    {
        return Some(ProtocolType::Iterm2);
    }
    let inside_tmux = std::env::var_os("TMUX").is_some()
        || std::env::var("TERM_PROGRAM").is_ok_and(|value| value == "tmux");
    if !inside_tmux
        && (std::env::var_os("KITTY_WINDOW_ID").is_some()
            || std::env::var("TERM").is_ok_and(|value| value.contains("kitty")))
    {
        return Some(ProtocolType::Kitty);
    }
    None
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: AxonClient,
    account_filter: Option<Uuid>,
    config: TuiConfig,
    picker: Picker,
) -> anyhow::Result<()> {
    let (live_tx, mut live_rx) = mpsc::unbounded_channel();
    tokio::spawn(websocket_task(client.clone(), live_tx));

    let (key_tx, mut key_rx) = mpsc::unbounded_channel();
    let input_paused = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let paused = input_paused.clone();
        move || input_task(key_tx, paused)
    });

    let (lifecycle_tx, mut lifecycle_rx) = mpsc::unbounded_channel();
    let (media_tx, mut media_rx) = mpsc::channel(app::MEDIA_WORKERS * 2);
    let mut app = App::new(client, account_filter, config, picker);
    app.set_lifecycle_sender(lifecycle_tx);
    app.set_media_sender(media_tx);
    app.refresh_accounts().await;
    app.refresh_rooms().await;
    app.load_selected_timeline().await;

    let mut tick = time::interval(Duration::from_millis(100));
    loop {
        // `take_redraw_request` signals that cached image content changed; for
        // halfblocks rendering ratatui's diff-based draw handles the update
        // without a full terminal clear (which queries cursor position and fails
        // in some terminals, e.g. WSL2 pass-through).
        let _ = app.take_redraw_request();
        terminal.draw(|frame| draw(frame, &mut app))?;

        tokio::select! {
            _ = tick.tick() => {}
            Some(key) = key_rx.recv() => {
                if app.handle_key(key).await {
                    break;
                }
                if app.take_edit_config_request() {
                    // Pause the input thread so it does not compete with the
                    // editor for /dev/tty keystrokes while it has control.
                    input_paused.store(true, Ordering::SeqCst);
                    // Give the thread up to one poll cycle (50 ms) to finish
                    // any in-progress event::read() and see the pause flag.
                    std::thread::sleep(Duration::from_millis(60));
                    while key_rx.try_recv().is_ok() {} // drain any stray events

                    // Suspend the TUI: restore normal terminal state.
                    disable_raw_mode()?;
                    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                    terminal.show_cursor()?;

                    let result = open_in_editor(&app.config_path);
                    app.apply_editor_result(result);

                    // Re-enter the TUI.
                    enable_raw_mode()?;
                    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                    terminal.clear()?;

                    // Drain events the editor may have left in the buffer,
                    // then resume the input thread.
                    while key_rx.try_recv().is_ok() {}
                    input_paused.store(false, Ordering::Release);
                }
            }
            Some(frame) = live_rx.recv() => {
                if app.handle_live_frame(frame) == LiveFrameAction::RefreshRooms {
                    let had_selection = app.selected_room().is_some();
                    app.refresh_rooms().await;
                    if !had_selection && app.selected_room().is_some() {
                        app.load_selected_timeline().await;
                    }
                }
            }
            Some(outcome) = lifecycle_rx.recv() => {
                app.handle_lifecycle_outcome(outcome).await;
            }
            Some(result) = media_rx.recv() => {
                app.handle_media_result(result);
            }
        }
    }

    Ok(())
}

/// Launch the user's preferred editor on `path`, blocking until it exits.
/// Respects `$EDITOR`; falls back to `nano` on Unix and `notepad` on Windows.
/// If `$EDITOR` contains arguments (e.g. `"vim -u NONE"`), they are split on
/// whitespace and passed correctly.
fn open_in_editor(path: &std::path::Path) -> io::Result<()> {
    let editor_var = std::env::var("EDITOR").unwrap_or_else(|_| {
        if cfg!(windows) {
            "notepad".to_owned()
        } else {
            "nano".to_owned()
        }
    });
    let mut parts = editor_var.split_whitespace();
    let bin = parts.next().unwrap_or("nano");
    let extra_args: Vec<&str> = parts.collect();
    std::process::Command::new(bin)
        .args(&extra_args)
        .arg(path)
        .status()?;
    Ok(())
}

/// Keyboard input thread. Uses `event::poll` with a short timeout so the
/// `paused` flag is checked regularly, allowing the main loop to suspend input
/// while an external editor has control of the terminal.
fn input_task(tx: mpsc::UnboundedSender<KeyEvent>, paused: Arc<AtomicBool>) {
    loop {
        if paused.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        match event::poll(Duration::from_millis(50)) {
            Ok(true) => {
                // Re-check the pause flag: it may have been set between poll
                // returning and us calling read().
                if paused.load(Ordering::Relaxed) {
                    continue;
                }
                match event::read() {
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if tx.send(key).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn leave(&mut self) -> anyhow::Result<()> {
        if self.active {
            disable_raw_mode()?;
            execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
            self.terminal.show_cursor()?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_image_protocol_overrides() {
        assert_eq!(parse_image_protocol("kitty"), Some(ProtocolType::Kitty));
        assert_eq!(parse_image_protocol("SIXEL"), Some(ProtocolType::Sixel));
        assert_eq!(parse_image_protocol("iterm"), Some(ProtocolType::Iterm2));
        assert_eq!(
            parse_image_protocol("half-blocks"),
            Some(ProtocolType::Halfblocks)
        );
        assert_eq!(parse_image_protocol("unknown"), None);
    }
}
