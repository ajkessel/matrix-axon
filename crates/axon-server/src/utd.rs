//! Operator CLI for UTD re-decryption retries.
//!
//! This is deliberately an API client rather than a DB-local subcommand:
//! re-decryption needs the live matrix-rust-sdk client and crypto store owned by
//! the running server.

use anyhow::{bail, Context};
use serde::Deserialize;

use crate::cli::UtdAction;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080";

#[derive(Debug, Deserialize)]
struct Envelope {
    data: RedecryptResponse,
}

#[derive(Debug, Deserialize)]
struct RedecryptResponse {
    selected: usize,
    attempted: usize,
    decrypted: usize,
    still_pending: usize,
    timed_out: bool,
}

pub async fn run(action: &UtdAction) -> anyhow::Result<()> {
    match action {
        UtdAction::Redecrypt {
            account_id,
            base_url,
            token,
        } => {
            let base_url = base_url
                .clone()
                .or_else(|| std::env::var("AXON_BASE_URL").ok())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
            let token = token
                .clone()
                .or_else(|| std::env::var("AXON_TOKEN").ok())
                .filter(|value| !value.trim().is_empty())
                .context("missing bearer token; pass --token or set AXON_TOKEN")?;
            let url = format!(
                "{}/v1/accounts/{account_id}/utds/redecrypt",
                base_url.trim_end_matches('/')
            );
            let response = reqwest::Client::new()
                .post(&url)
                .bearer_auth(token)
                .send()
                .await
                .with_context(|| format!("calling {url}"))?;
            let status = response.status();
            let body = response.text().await.context("reading response body")?;
            if !status.is_success() {
                bail!("UTD re-decryption request failed with {status}: {body}");
            }
            let envelope: Envelope =
                serde_json::from_str(&body).context("parsing UTD re-decryption response")?;
            println!(
                "selected={} attempted={} decrypted={} still_pending={} timed_out={}",
                envelope.data.selected,
                envelope.data.attempted,
                envelope.data.decrypted,
                envelope.data.still_pending,
                envelope.data.timed_out
            );
            Ok(())
        }
    }
}
