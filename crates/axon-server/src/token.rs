//! The `axon token` subcommand: mint, list, and revoke bearer tokens (M7b).
//!
//! This is the bootstrap path for client→axon auth — the first token has to be
//! minted out-of-band before any client can call the now-gated `/v1/` API. It
//! needs only the database, so it connects the [`Store`] and skips the sync
//! engine and HTTP listener entirely.

use anyhow::Context;
use axon_core::Config;
use axon_store::Store;

use crate::cli::TokenAction;

/// Run a `token` subcommand against the configured database.
pub async fn run(action: TokenAction, config: &Config) -> anyhow::Result<()> {
    let store = Store::connect(&config.database.url, config.database.max_connections)
        .await
        .context("connecting to database")?;

    match action {
        TokenAction::Issue { label } => {
            let issued = store.issue_token(&label).await.context("issuing token")?;
            // The raw secret goes to stdout (not the tracing log) and is shown
            // exactly once — only its hash is stored.
            println!(
                "Issued token {} (label {:?}). Copy it now — it will not be shown again:\n",
                issued.id, issued.label
            );
            println!("{}", issued.token);
        }
        TokenAction::List => {
            let tokens = store.list_tokens().await.context("listing tokens")?;
            if tokens.is_empty() {
                println!("No tokens. Mint one with `axon token issue --label <name>`.");
            } else {
                for token in tokens {
                    let status = if token.is_revoked() {
                        "revoked"
                    } else {
                        "active"
                    };
                    let last_used = token
                        .last_used_at
                        .map(|ts| ts.to_rfc3339())
                        .unwrap_or_else(|| "never".to_owned());
                    println!(
                        "{}  {:<7}  {:<24}  created {}  last used {}",
                        token.id,
                        status,
                        token.label,
                        token.created_at.to_rfc3339(),
                        last_used,
                    );
                }
            }
        }
        TokenAction::Revoke { id } => {
            if store.revoke_token(id).await.context("revoking token")? {
                println!("Revoked token {id}.");
            } else {
                println!("No active token with id {id} (already revoked or unknown).");
            }
        }
    }

    Ok(())
}
