//! Command-line surface for the `axon` binary.
//!
//! With no subcommand, `axon` runs the server (the default and only
//! long-running mode). The `token` subcommand is the M7b bootstrap path: it
//! mints / lists / revokes the bearer tokens clients use to reach the `/v1/`
//! API, talking only to the database (no sync engine, no HTTP listener). The
//! `search` subcommand is the M9b operator path for the full-text index. The
//! `init` subcommand (M13, ADR 0051) generates a starter config on first run.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use uuid::Uuid;

/// Top-level CLI: an optional subcommand, defaulting to "run the server".
#[derive(Debug, Parser)]
#[command(name = "axon", version, about = "A personal Matrix state layer")]
pub struct Cli {
    /// Path to the TOML config file. Overrides the `AXON_CONFIG` env var and the
    /// `./axon.toml` / platform-config-dir discovery. Applies to the server and
    /// every subcommand.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The `axon` subcommands. Absent → run the server.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage client→axon bearer tokens (the local-API auth gate).
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
    /// Manage the full-text search index.
    Search {
        #[command(subcommand)]
        action: SearchAction,
    },
    /// Generate a starter configuration file (first-run setup).
    ///
    /// Writes a minimal config (a generated `store_key` plus the Postgres URL) to
    /// the platform config directory, or to `--config <PATH>` when given. On an
    /// interactive terminal it confirms the database URL and offers to mint a
    /// first bearer token; with `--non-interactive` it runs from flags/defaults.
    Init(InitArgs),
}

/// Flags for `axon init`.
#[derive(Debug, clap::Args)]
pub struct InitArgs {
    /// Postgres connection URL to write into the generated config. Defaults to
    /// `postgres://axon:axon@127.0.0.1:5432/axon` (the prompt's default too).
    #[arg(long, value_name = "URL")]
    pub database_url: Option<String>,
    /// Overwrite an existing config file. Without this, `init` refuses to clobber
    /// one (regenerating `store_key` would orphan existing encrypted data).
    #[arg(long)]
    pub force: bool,
    /// Never prompt; take every value from flags/defaults. Implied when stdin is
    /// not a terminal.
    #[arg(long)]
    pub non_interactive: bool,
    /// Do not mint a first bearer token even if the database is reachable.
    #[arg(long, conflicts_with_all = ["print_token", "tui_config"])]
    pub no_token: bool,
    /// Mint and print a first bearer token without prompting (requires a reachable
    /// database).
    #[arg(long)]
    pub print_token: bool,
    /// If the database is unreachable, start a matching local Postgres in Docker
    /// without prompting (the container's credentials are derived from the config's
    /// database URL). Requires Docker and a loopback database host.
    #[arg(long)]
    pub start_postgres: bool,
    /// After minting a token, also write it into an axon-tui client config
    /// (`~/.config/axon-tui/config.toml`) without prompting. Implies minting a
    /// token; merges into an existing config, preserving other settings.
    #[arg(long)]
    pub tui_config: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_token_conflicts_with_tui_config() {
        let err = Cli::try_parse_from(["axon", "init", "--no-token", "--tui-config"])
            .expect_err("--tui-config implies minting, so --no-token must conflict");
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}

/// `axon search …` actions.
#[derive(Debug, Subcommand)]
pub enum SearchAction {
    /// Force a from-scratch rebuild of the search index: clears the seed marker so
    /// the next server start reseeds the whole corpus from Postgres.
    Reindex,
}

/// `axon token …` actions.
#[derive(Debug, Subcommand)]
pub enum TokenAction {
    /// Mint a new token and print the secret once (it is never recoverable).
    Issue {
        /// A human-readable label, e.g. the device or client the token is for.
        #[arg(long)]
        label: String,
    },
    /// List all tokens with their status (never prints any secret).
    List,
    /// Revoke a token by id (see `list`).
    Revoke {
        /// The token's id.
        id: Uuid,
    },
}
