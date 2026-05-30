//! Matrix event rows: one per event, scoped by `account_id`.
//!
//! `axon-store` owns all event persistence. Other crates (notably `axon-sync`)
//! call [`Store::upsert_event`] with a [`NewEvent`] describing the incoming
//! Matrix timeline event. The operation is idempotent: re-delivering the same
//! `(account_id, event_id)` is a no-op, so callers need not deduplicate.

use serde_json::Value;
use uuid::Uuid;

use crate::{Store, StoreError};

/// Data required to insert a single Matrix event into the store.
pub struct NewEvent<'a> {
    /// Matrix event ID (e.g. `$abc123:example.org`).
    pub event_id: &'a str,
    /// Matrix room ID.
    pub room_id: &'a str,
    /// Axon account this event belongs to.
    pub account_id: Uuid,
    /// Matrix user ID of the event sender.
    pub sender: &'a str,
    /// `origin_server_ts` in milliseconds since the Unix epoch.
    pub origin_ts: i64,
    /// Matrix event type string (e.g. `m.room.message`).
    pub event_type: &'a str,
    /// Decrypted event `content` as JSON. `None` for events that could not be
    /// decrypted (UTDs); will be populated by the re-decryption queue (M3c).
    pub content: Option<Value>,
    /// Full event JSON as seen by the SDK (post-decryption for E2EE rooms).
    pub raw_content: Value,
}

impl Store {
    /// Insert a Matrix event. Idempotent: if `(account_id, event_id)` already
    /// exists the existing row is left unchanged and no error is returned.
    pub async fn upsert_event(&self, ev: &NewEvent<'_>) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "INSERT INTO events \
             (event_id, room_id, account_id, sender, origin_ts, event_type, \
              content, raw_content) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (account_id, event_id) DO NOTHING",
        )
        .bind(ev.event_id)
        .bind(ev.room_id)
        .bind(ev.account_id)
        .bind(ev.sender)
        .bind(ev.origin_ts)
        .bind(ev.event_type)
        .bind(&ev.content)
        .bind(&ev.raw_content)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
