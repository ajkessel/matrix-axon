//! Matrix event rows: one per event, scoped by `account_id`.
//!
//! `axon-store` owns all event persistence. Other crates (notably `axon-sync`)
//! call [`Store::upsert_event`] with a [`NewEvent`] describing the incoming
//! Matrix timeline event. The operation is idempotent: re-delivering the same
//! `(account_id, event_id)` is a no-op, so callers need not deduplicate.

use serde_json::Value;
use sqlx_core::row::Row;
use sqlx_postgres::{PgRow, Postgres};
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
    /// decrypted (UTDs); back-filled by the re-decryption queue once keys arrive.
    pub content: Option<Value>,
    /// The full event envelope as dispatched to the SDK handler: type, sender,
    /// `content`, unsigned, etc. Plaintext for events the SDK decrypted; the
    /// `m.room.encrypted` envelope (ciphertext + `session_id`) for UTDs. This is
    /// the whole event, not just the ciphertext — `content` above is the
    /// extracted, decrypted payload.
    pub raw_event: Value,
    /// For a UTD (`m.room.encrypted` with `content = NULL`), the megolm
    /// `session_id` the message was encrypted with, lifted out of the envelope
    /// so the re-decryption queue can match arriving room keys to pending rows.
    /// `None` for events that arrived already decrypted. See ADR 0014.
    pub megolm_session_id: Option<&'a str>,
}

/// A persisted event still awaiting decryption (a UTD): `content IS NULL`, with
/// its `m.room.encrypted` ciphertext preserved in `raw_event`. The re-decryption
/// queue loads these, decrypts them once the megolm key arrives, and back-fills
/// `content` + `event_type` via [`Store::update_decrypted_event`].
#[derive(Debug, Clone)]
pub struct PendingUtd {
    /// Matrix event ID — the key for the back-fill update.
    pub event_id: String,
    /// Matrix room ID the event belongs to (needed to get the `Room` handle).
    pub room_id: String,
    /// The full `m.room.encrypted` envelope as dispatched, including the
    /// ciphertext the SDK re-decrypts against.
    pub raw_event: Value,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for PendingUtd {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(PendingUtd {
            event_id: row.try_get("event_id")?,
            room_id: row.try_get("room_id")?,
            raw_event: row.try_get("raw_event")?,
        })
    }
}

/// Columns selected for a [`PendingUtd`].
const PENDING_UTD_COLUMNS: &str = "event_id, room_id, raw_event";

impl Store {
    /// Insert a Matrix event. Idempotent: if `(account_id, event_id)` already
    /// exists the existing row is left unchanged and no error is returned.
    pub async fn upsert_event(&self, ev: &NewEvent<'_>) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "INSERT INTO events \
             (event_id, room_id, account_id, sender, origin_ts, event_type, \
              content, raw_event, megolm_session_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (account_id, event_id) DO NOTHING",
        )
        .bind(ev.event_id)
        .bind(ev.room_id)
        .bind(ev.account_id)
        .bind(ev.sender)
        .bind(ev.origin_ts)
        .bind(ev.event_type)
        .bind(&ev.content)
        .bind(&ev.raw_event)
        .bind(ev.megolm_session_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load the pending UTDs in `room_id` encrypted with `session_id` — the rows
    /// a freshly-arrived megolm room key can decrypt. Uses the partial
    /// `events_pending_utd_idx` (account, room, session WHERE content IS NULL).
    pub async fn pending_utds_for_session(
        &self,
        account_id: Uuid,
        room_id: &str,
        session_id: &str,
    ) -> Result<Vec<PendingUtd>, StoreError> {
        let sql = format!(
            "SELECT {PENDING_UTD_COLUMNS} FROM events \
             WHERE account_id = $1 AND room_id = $2 AND megolm_session_id = $3 \
               AND content IS NULL"
        );
        let rows = sqlx_core::query_as::query_as::<Postgres, PendingUtd>(&sql)
            .bind(account_id)
            .bind(room_id)
            .bind(session_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    /// Load every pending UTD for an account, regardless of session. Used for
    /// the startup sweep: keys may already sit in the SDK crypto store from a
    /// prior run or a just-completed `recover()`, so we retry the whole backlog
    /// once at boot rather than waiting for new arrivals.
    pub async fn pending_utds_for_account(
        &self,
        account_id: Uuid,
    ) -> Result<Vec<PendingUtd>, StoreError> {
        let sql = format!(
            "SELECT {PENDING_UTD_COLUMNS} FROM events \
             WHERE account_id = $1 AND content IS NULL"
        );
        let rows = sqlx_core::query_as::query_as::<Postgres, PendingUtd>(&sql)
            .bind(account_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    /// Back-fill a successfully re-decrypted event: set its `content` and real
    /// `event_type`. The `content IS NULL` guard keeps this idempotent and
    /// avoids clobbering a row another path already decrypted (e.g. a live
    /// dispatch that raced the queue).
    pub async fn update_decrypted_event(
        &self,
        account_id: Uuid,
        event_id: &str,
        content: &Value,
        event_type: &str,
    ) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "UPDATE events SET content = $3, event_type = $4 \
             WHERE account_id = $1 AND event_id = $2 AND content IS NULL",
        )
        .bind(account_id)
        .bind(event_id)
        .bind(content)
        .bind(event_type)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
