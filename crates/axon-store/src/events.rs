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
    /// For an `m.room.redaction` event, the `event_id` it redacts. `None` for
    /// every other event. Drives timeline-read masking of the target row.
    pub redacts: Option<&'a str>,
    /// The event's `m.relates_to` object (rel_type + event_id), captured
    /// generically (replies, edits, annotations, `m.thread`). `None` if absent.
    pub relates_to: Option<Value>,
    /// Plaintext `content.body` of a message event, lifted for fast timeline
    /// render and search ingestion. `None` for events with no textual body.
    pub decrypted_body_text: Option<&'a str>,
}

/// The original `m.room.encrypted` envelope of an event, preserved in its own
/// table independently of `events.raw_event`. Written only on the UTD path —
/// the SDK surfaces ciphertext to us only for events it could not decrypt
/// (ADR 0015).
pub struct EventCiphertext<'a> {
    /// Axon account this event belongs to.
    pub account_id: Uuid,
    /// Matrix event ID.
    pub event_id: &'a str,
    /// Matrix room ID.
    pub room_id: &'a str,
    /// Encryption algorithm, e.g. `m.megolm.v1.aes-sha2`.
    pub algorithm: &'a str,
    /// The curve25519 `sender_key` on the envelope, if present.
    pub sender_key: Option<&'a str>,
    /// The megolm `session_id`, if present.
    pub session_id: Option<&'a str>,
    /// The full `m.room.encrypted` `content` object.
    pub ciphertext: Value,
}

/// Cryptographic provenance of a successfully-decrypted event, derived from the
/// SDK's `EncryptionInfo`. Written for every decrypted event — on the live
/// dispatch path and again on re-decryption — into the megolm-session and
/// sender-device-key sibling tables (ADR 0015).
pub struct EventCrypto<'a> {
    /// Axon account this event belongs to.
    pub account_id: Uuid,
    /// Matrix event ID.
    pub event_id: &'a str,
    /// Megolm session id that decrypted the event.
    pub session_id: Option<&'a str>,
    /// Curve25519 key of the device that created the megolm session — also the
    /// sending device's identity curve25519 key.
    pub curve25519_key: Option<&'a str>,
    /// Claimed ed25519 signing key of the sending device.
    pub ed25519_key: Option<&'a str>,
    /// Whether the key reached us via forwarding rather than direct sharing.
    pub forwarded: bool,
    /// Forwarder user id, if the key was forwarded.
    pub forwarder_user_id: Option<&'a str>,
    /// Forwarder device id, if the key was forwarded.
    pub forwarder_device_id: Option<&'a str>,
    /// The sending device's id, if known.
    pub device_id: Option<&'a str>,
    /// Verification state of the sending device at decrypt time: `verified` or
    /// `unverified`. A snapshot — it can change later as devices are verified.
    pub verification_state: &'a str,
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

/// A position in a room timeline — the `(origin_ts, id)` sort key of a row —
/// used for stable reverse-chronological pagination. Pass it as the `before`
/// argument to [`Store::room_timeline`] to fetch the page of older events. `id`
/// (a monotonic `BIGSERIAL`) breaks ties between events sharing an `origin_ts`,
/// so pages never overlap or skip.
///
/// This is **not** an opaque token: its fields are public so the store and its
/// callers can construct one directly. The HTTP layer (`axon-api`) serializes it
/// into an opaque cursor string, so the wire contract can stay fixed even if the
/// internal sort key changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineCursor {
    /// `origin_server_ts` in milliseconds — the primary sort key.
    pub origin_ts: i64,
    /// The row's monotonic `id`, the tiebreaker within an `origin_ts`.
    pub id: i64,
}

/// One row of a room timeline read. Content is masked at read time for redacted
/// events: when [`redaction_event_id`](Self::redaction_event_id) is set, `content`
/// and `decrypted_body_text` are `None` even though the underlying row (and its
/// ciphertext sibling) still hold the original data.
#[derive(Debug, Clone)]
pub struct TimelineRow {
    /// The row's monotonic store id (the cursor tiebreaker).
    pub id: i64,
    /// Matrix event ID.
    pub event_id: String,
    /// Matrix room ID.
    pub room_id: String,
    /// Matrix user ID of the sender.
    pub sender: String,
    /// Matrix state key for state events.
    pub state_key: Option<String>,
    /// `origin_server_ts` in milliseconds.
    pub origin_ts: i64,
    /// Matrix event type.
    pub event_type: String,
    /// Decrypted content JSON — `None` for UTDs and for redacted events.
    pub content: Option<Value>,
    /// Plaintext body — `None` when absent or masked by redaction.
    pub decrypted_body_text: Option<String>,
    /// The event's `m.relates_to` object, if any.
    pub relates_to: Option<Value>,
    /// For a redaction event, the `event_id` it redacts.
    pub redacts: Option<String>,
    /// The `event_id` of the redaction that masked this row, if it was redacted.
    pub redaction_event_id: Option<String>,
}

impl TimelineRow {
    /// The cursor pointing at this row, for fetching the next (older) page.
    pub fn cursor(&self) -> TimelineCursor {
        TimelineCursor {
            origin_ts: self.origin_ts,
            id: self.id,
        }
    }
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for TimelineRow {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(TimelineRow {
            id: row.try_get("id")?,
            event_id: row.try_get("event_id")?,
            room_id: row.try_get("room_id")?,
            sender: row.try_get("sender")?,
            state_key: row.try_get("state_key")?,
            origin_ts: row.try_get("origin_ts")?,
            event_type: row.try_get("event_type")?,
            content: row.try_get("content")?,
            decrypted_body_text: row.try_get("decrypted_body_text")?,
            relates_to: row.try_get("relates_to")?,
            redacts: row.try_get("redacts")?,
            redaction_event_id: row.try_get("redaction_event_id")?,
        })
    }
}

/// Shared SELECT projection for timeline reads, with read-time redaction
/// masking. A `LEFT JOIN LATERAL … LIMIT 1` finds at most one redaction
/// targeting each event (a plain JOIN would duplicate a multiply-redacted row);
/// when one exists, `content` and `decrypted_body_text` are masked to `NULL` and
/// `redaction_event_id` is set. Callers append their own `WHERE` (binding
/// `account_id` as `$1`) plus any ordering / pagination. Selects exactly the
/// columns [`TimelineRow`] reads.
const TIMELINE_SELECT: &str =
    "SELECT e.id, e.event_id, e.room_id, e.sender, e.raw_event->>'state_key' AS state_key, \
            e.origin_ts, e.event_type, \
            CASE WHEN r.event_id IS NULL THEN e.content END AS content, \
            CASE WHEN r.event_id IS NULL THEN e.decrypted_body_text END AS decrypted_body_text, \
            e.relates_to, e.redacts, r.event_id AS redaction_event_id \
     FROM events e \
     LEFT JOIN LATERAL ( \
         SELECT rr.event_id FROM events rr \
         WHERE rr.account_id = e.account_id \
           AND rr.event_type = 'm.room.redaction' \
           AND rr.redacts = e.event_id \
         LIMIT 1 \
     ) r ON TRUE";

impl Store {
    /// Insert a Matrix event. Idempotent: if `(account_id, event_id)` already
    /// exists the existing row is left unchanged and no error is returned.
    pub async fn upsert_event(&self, ev: &NewEvent<'_>) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "INSERT INTO events \
             (event_id, room_id, account_id, sender, origin_ts, event_type, \
              content, raw_event, megolm_session_id, redacts, relates_to, \
              decrypted_body_text) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
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
        .bind(ev.redacts)
        .bind(&ev.relates_to)
        .bind(ev.decrypted_body_text)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Persist the original ciphertext envelope for a UTD. Idempotent: the
    /// ciphertext never changes, so a re-insert for the same `(account_id,
    /// event_id)` is a no-op.
    pub async fn insert_event_ciphertext(&self, c: &EventCiphertext<'_>) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "INSERT INTO event_ciphertext \
             (account_id, event_id, room_id, algorithm, sender_key, session_id, ciphertext) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (account_id, event_id) DO NOTHING",
        )
        .bind(c.account_id)
        .bind(c.event_id)
        .bind(c.room_id)
        .bind(c.algorithm)
        .bind(c.sender_key)
        .bind(c.session_id)
        .bind(&c.ciphertext)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record the crypto provenance of a decrypted event into both sibling
    /// tables (megolm session + sender device keys). Upserts: a UTD has no
    /// provenance at persist time, so the authoritative write happens on
    /// re-decryption; a later, better state overwrites an earlier one.
    pub async fn upsert_event_crypto(&self, c: &EventCrypto<'_>) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "INSERT INTO event_megolm_session \
             (account_id, event_id, session_id, sender_curve25519_key, \
              sender_claimed_ed25519_key, forwarded, forwarder_user_id, forwarder_device_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (account_id, event_id) DO UPDATE SET \
               session_id = EXCLUDED.session_id, \
               sender_curve25519_key = EXCLUDED.sender_curve25519_key, \
               sender_claimed_ed25519_key = EXCLUDED.sender_claimed_ed25519_key, \
               forwarded = EXCLUDED.forwarded, \
               forwarder_user_id = EXCLUDED.forwarder_user_id, \
               forwarder_device_id = EXCLUDED.forwarder_device_id",
        )
        .bind(c.account_id)
        .bind(c.event_id)
        .bind(c.session_id)
        .bind(c.curve25519_key)
        .bind(c.ed25519_key)
        .bind(c.forwarded)
        .bind(c.forwarder_user_id)
        .bind(c.forwarder_device_id)
        .execute(&self.pool)
        .await?;

        sqlx_core::query::query(
            "INSERT INTO event_sender_device_keys \
             (account_id, event_id, device_id, curve25519_key, ed25519_key, verification_state) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT (account_id, event_id) DO UPDATE SET \
               device_id = EXCLUDED.device_id, \
               curve25519_key = EXCLUDED.curve25519_key, \
               ed25519_key = EXCLUDED.ed25519_key, \
               verification_state = EXCLUDED.verification_state",
        )
        .bind(c.account_id)
        .bind(c.event_id)
        .bind(c.device_id)
        .bind(c.curve25519_key)
        .bind(c.ed25519_key)
        .bind(c.verification_state)
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

    /// Back-fill a successfully re-decrypted event: set its `content`, real
    /// `event_type`, and the plaintext-derived hot columns (`decrypted_body_text`,
    /// `relates_to`) that were unknown while it was a UTD. The `content IS NULL`
    /// guard keeps this idempotent and avoids clobbering a row another path
    /// already decrypted (e.g. a live dispatch that raced the queue).
    pub async fn update_decrypted_event(
        &self,
        account_id: Uuid,
        event_id: &str,
        content: &Value,
        event_type: &str,
        decrypted_body_text: Option<&str>,
        relates_to: Option<&Value>,
    ) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "UPDATE events \
             SET content = $3, event_type = $4, decrypted_body_text = $5, relates_to = $6 \
             WHERE account_id = $1 AND event_id = $2 AND content IS NULL",
        )
        .bind(account_id)
        .bind(event_id)
        .bind(content)
        .bind(event_type)
        .bind(decrypted_body_text)
        .bind(relates_to)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read a room's timeline, newest first, with redaction masking.
    ///
    /// Returns up to `limit` events in `room_id` for `account_id`, ordered
    /// reverse-chronologically by `(origin_ts, id)`. Pass `before = None` for the
    /// most recent page, then the [`cursor`](TimelineRow::cursor) of the last row
    /// to fetch successively older pages. Because the cursor includes the
    /// monotonic `id`, pagination is stable: pages never overlap or skip even
    /// when several events share an `origin_ts`.
    ///
    /// A row targeted by an `m.room.redaction` comes back with `content` and
    /// `decrypted_body_text` masked to `None` and `redaction_event_id` set to the
    /// redaction's `event_id`. The masking is read-time only — the stored row and
    /// its ciphertext sibling are untouched.
    pub async fn room_timeline(
        &self,
        account_id: Uuid,
        room_id: &str,
        before: Option<TimelineCursor>,
        limit: i64,
    ) -> Result<Vec<TimelineRow>, StoreError> {
        // The redaction match (in TIMELINE_SELECT) is a LATERAL subselect with
        // LIMIT 1 so a target event redacted more than once still yields a single
        // row (a plain JOIN would duplicate it and corrupt the page size).
        let mut sql = format!("{TIMELINE_SELECT} WHERE e.account_id = $1 AND e.room_id = $2");
        if before.is_some() {
            sql.push_str(" AND (e.origin_ts, e.id) < ($3, $4)");
        }
        sql.push_str(" ORDER BY e.origin_ts DESC, e.id DESC LIMIT ");
        // `limit` is bound, not interpolated, below.
        sql.push_str(if before.is_some() { "$5" } else { "$3" });

        let mut q = sqlx_core::query_as::query_as::<Postgres, TimelineRow>(&sql)
            .bind(account_id)
            .bind(room_id);
        if let Some(cursor) = before {
            q = q.bind(cursor.origin_ts).bind(cursor.id);
        }
        let rows = q.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows)
    }

    /// Read a single event by `(account_id, event_id)`, with the same read-time
    /// redaction masking as [`room_timeline`](Self::room_timeline): if the event
    /// has been redacted, `content` and `decrypted_body_text` come back `None`
    /// and `redaction_event_id` is set. Returns `None` when no such event exists
    /// for the account.
    pub async fn get_event(
        &self,
        account_id: Uuid,
        event_id: &str,
    ) -> Result<Option<TimelineRow>, StoreError> {
        let sql = format!("{TIMELINE_SELECT} WHERE e.account_id = $1 AND e.event_id = $2");
        let row = sqlx_core::query_as::query_as::<Postgres, TimelineRow>(&sql)
            .bind(account_id)
            .bind(event_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }
}
