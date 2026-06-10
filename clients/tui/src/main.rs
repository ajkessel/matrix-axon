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
    std::thread::spawn(move || input_task(key_tx));

    let mut app = App::new(client, account_filter, config);
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
        }
    }

    Ok(())
}

fn input_task(tx: mpsc::UnboundedSender<KeyEvent>) {
    loop {
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
