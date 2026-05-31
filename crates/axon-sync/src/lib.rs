//! matrix-rust-sdk sync engine wrapper (Simplified Sliding Sync only).
//!
//! `axon-sync` owns the live connection to each account's homeserver. It builds
//! one matrix-rust-sdk [`Client`](matrix_sdk::Client) per account, authenticates
//! (login on first boot, session restore thereafter), and runs a
//! [`SyncService`](matrix_sdk_ui::sync_service::SyncService) — Simplified
//! Sliding Sync (MSC4186), no legacy `/sync`. Event persistence into the
//! Postgres archive lands in the next subphase.
//!
//! The public surface is [`SyncEngine`]: start it with a `Store`, a
//! `SyncConfig`, and a cancellation token; it supervises a task per account and
//! restarts failed syncs with exponential backoff.

mod client;
mod engine;
mod error;

pub use engine::SyncEngine;
pub use error::SyncError;
