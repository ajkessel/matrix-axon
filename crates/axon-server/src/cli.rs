//! Command-line surface for the `axon` binary.
//!
//! With no subcommand, `axon` runs the server (the default and only
//! long-running mode). The `token` subcommand is the M7b bootstrap path: it
//! mints / lists / revokes the bearer tokens clients use to reach the `/v1/`
//! API, talking only to the database (no sync engine, no HTTP listener). The
//! `search` subcommand is the M9b operator path for the full-text index.

use clap::{Parser, Subcommand};
use uuid::Uuid;

/// Top-level CLI: an optional subcommand, defaulting to "run the server".
#[derive(Debug, Parser)]
#[command(name = "axon", version, about = "A personal Matrix state layer")]
pub struct Cli {
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
