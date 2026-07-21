use std::time::{Duration, Instant};

use anyhow::bail;

pub fn path_segment(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(nibble(byte >> 4));
                out.push(nibble(byte & 0x0f));
            }
        }
    }
    out
}

fn nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + (n - 10)) as char,
        _ => unreachable!("nibble out of range"),
    }
}

pub async fn wait_for<F, Fut>(label: &str, timeout: Duration, mut check: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if check().await? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for {label}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The negated counterpart to [`wait_for`]: poll `check` every 250ms across
/// `window` and fail the moment it returns `false`, instead of succeeding on
/// the first `true`. Use this to prove a condition holds *stably* — e.g. "no
/// background task changed this" — rather than only checking it once
/// immediately after a triggering action, which would pass even if the
/// reaction under test just hadn't landed yet.
pub async fn stays_true<F, Fut>(label: &str, window: Duration, mut check: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let deadline = Instant::now() + window;
    loop {
        if !check().await? {
            bail!("{label} did not hold stable");
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
