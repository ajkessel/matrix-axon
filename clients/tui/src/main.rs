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
use args::Args;
use config::TuiConfig;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::time;
use ui::draw;
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse()?;
    let config = TuiConfig::load_or_create_default()?;
    let client = AxonClient::new(args.base_url);
    let mut terminal = TerminalGuard::enter()?;
    let result = run_app(&mut terminal.terminal, client, args.account_id, config).await;
    terminal.leave()?;
    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: AxonClient,
    account_filter: Option<Uuid>,
    config: TuiConfig,
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
    let mut app = App::new(client, account_filter, config);
    app.set_lifecycle_sender(lifecycle_tx);
    app.refresh_accounts().await;
    app.refresh_rooms().await;
    app.load_selected_timeline().await;

    let mut tick = time::interval(Duration::from_millis(100));
    loop {
        if app.take_redraw_request() {
            terminal.clear()?;
        }
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
