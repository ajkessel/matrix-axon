//! S1 TUI smoke scenarios.
//!
//! Each spawns the real `axon-tui` against a fresh in-process stub, asserts on
//! the rendered screen and the stub's request journal, and tears the process
//! down. Assertions read the parsed terminal screen; exit assertions also
//! require the alternate-screen leave sequence so a clean process exit cannot
//! hide a terminal-restoration regression.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};

use crate::pty::PtyDriver;
use crate::runner::{Ctx, ScenarioOutcome};
use crate::stub::{JournalEntry, Stub, StubState};

/// `launch_and_quit`: first paint renders the room, message, and input panes,
/// then `/quit` exits cleanly with the terminal restored.
pub async fn launch_and_quit(ctx: &Ctx) -> ScenarioOutcome {
    let stub = match Stub::start(&ctx.run_id).await {
        Ok(stub) => stub,
        Err(err) => return failed_before_spawn(err),
    };
    let mut driver = match ctx.spawn_tui("launch_and_quit", &stub.base_url()) {
        Ok(driver) => driver,
        Err(err) => {
            stub.stop().await;
            return failed_before_spawn(err);
        }
    };

    let result = (|| {
        wait_first_paint(&mut driver, &stub.state, ctx.timeout)?;
        if !driver.saw_alt_screen_enter() {
            bail!("TUI never entered the alternate screen");
        }
        driver.type_text("/quit")?;
        driver.press_enter()?;
        require_clean_exit(&mut driver, ctx.timeout)
    })();

    let outcome = ScenarioOutcome::capture(&driver, result);
    drop(driver);
    stub.stop().await;
    outcome
}

/// `ctrl_c_exit`: the configured Ctrl-C shortcut exits cleanly.
pub async fn ctrl_c_exit(ctx: &Ctx) -> ScenarioOutcome {
    let stub = match Stub::start(&ctx.run_id).await {
        Ok(stub) => stub,
        Err(err) => return failed_before_spawn(err),
    };
    let mut driver = match ctx.spawn_tui("ctrl_c_exit", &stub.base_url()) {
        Ok(driver) => driver,
        Err(err) => {
            stub.stop().await;
            return failed_before_spawn(err);
        }
    };

    let result = (|| {
        wait_first_paint(&mut driver, &stub.state, ctx.timeout)?;
        driver.press_ctrl_c()?;
        require_clean_exit(&mut driver, ctx.timeout)
    })();

    let outcome = ScenarioOutcome::capture(&driver, result);
    drop(driver);
    stub.stop().await;
    outcome
}

/// `send_round_trip`: a run-marked message is submitted via keystrokes, the
/// stub's journal records the send, and the WebSocket echo renders in the room.
pub async fn send_round_trip(ctx: &Ctx) -> ScenarioOutcome {
    let stub = match Stub::start(&ctx.run_id).await {
        Ok(stub) => stub,
        Err(err) => return failed_before_spawn(err),
    };
    let mut driver = match ctx.spawn_tui("send_round_trip", &stub.base_url()) {
        Ok(driver) => driver,
        Err(err) => {
            stub.stop().await;
            return failed_before_spawn(err);
        }
    };

    // Use the first 8 chars of the UUID (32-bit isolation). The full run_id
    // would make the marker 46 chars which wraps across rows in the message
    // pane, breaking the screen.contains() check (vt100 splits at column edges).
    let marker = format!("roundtrip-{}", &ctx.run_id[..8]);
    let result = (|| {
        wait_first_paint(&mut driver, &stub.state, ctx.timeout)?;
        // Connect-before-trigger: `/v1/ws` is a live tail with no replay, so
        // wait for the TUI's WS upgrade before sending.
        wait_for_journal(&stub.state, ctx.timeout, "GET /v1/ws", |entries| {
            entries
                .iter()
                .any(|e| e.method == "GET" && e.path == "/v1/ws")
        })?;

        driver.type_text(&marker)?;
        driver.press_enter()?;

        // The journal must record the send with our exact body.
        wait_for_journal(&stub.state, ctx.timeout, "the send request", |entries| {
            entries.iter().any(|e| is_send_of(e, &marker))
        })?;

        // The WS echo of that send must render in the open room.
        driver.wait_for_screen("the echoed message to render", ctx.timeout, |screen| {
            screen.contains(&marker)
        })?;
        Ok(())
    })();

    let outcome = ScenarioOutcome::capture(&driver, result);
    driver.terminate();
    drop(driver);
    stub.stop().await;
    outcome
}

/// Wait until the room list, the seeded message, and the input line have all
/// painted — i.e. the TUI is up and talking to the stub.
fn wait_first_paint(
    driver: &mut PtyDriver,
    state: &StubState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let room_name = state.room_name.clone();
    driver.wait_for_screen_or_exit("the room list to render", timeout, move |screen| {
        screen.contains("Rooms") && screen.contains(&room_name)
    })?;
    // Anchor to the bottom section of the screen where the input box lives,
    // not the entire screen — the room-list selection marker "> " renders at
    // the top and would satisfy screen.contains('>') immediately.
    driver.wait_for_screen_or_exit("the input line to render", timeout, |screen| {
        screen.lines().rev().take(5).any(|l| l.contains('>'))
    })?;
    Ok(())
}

/// Require a zero-status exit within the deadline, with the alternate screen
/// left (terminal restored). A forced kill here would be a failure, not success.
fn require_clean_exit(driver: &mut PtyDriver, timeout: Duration) -> anyhow::Result<()> {
    let status = driver.wait_for_exit(timeout)?;
    if !status.success() {
        bail!("TUI exited with failure status: {status:?}");
    }
    // The leave sequence is emitted during teardown; give the reader thread up
    // to the full scenario timeout to drain the final bytes after exit.
    let deadline = Instant::now() + timeout;
    while !driver.saw_alt_screen_leave() {
        if Instant::now() >= deadline {
            bail!("TUI exited without leaving the alternate screen (terminal not restored)");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Poll the stub journal until `predicate` holds.
fn wait_for_journal<F>(
    state: &StubState,
    timeout: Duration,
    what: &str,
    predicate: F,
) -> anyhow::Result<()>
where
    F: Fn(&[JournalEntry]) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        if predicate(&state.journal()) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!("timed out after {timeout:?} waiting for {what}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `entry` is a message send carrying exactly `marker` as its body.
fn is_send_of(entry: &JournalEntry, marker: &str) -> bool {
    entry.method == "POST"
        && entry.path.ends_with("/send")
        && entry
            .body
            .as_ref()
            .and_then(|b| b.get("body"))
            .and_then(|b| b.as_str())
            == Some(marker)
}

fn failed_before_spawn(err: anyhow::Error) -> ScenarioOutcome {
    ScenarioOutcome {
        result: Err(err),
        transcript: Vec::new(),
        final_screen: String::new(),
    }
}
