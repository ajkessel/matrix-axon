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
    /// The four-valued sender-trust verdict at decrypt time (M7c): `verified`,
    /// `unverified`, `unknown`, or `verification_violation`. `None` when no
    /// verdict could be derived. A snapshot — distinct from the sender's
    /// *current* trust, which the verification bundle reads live.
    pub sender_trust: Option<&'a str>,
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
    /// The sender-trust verdict snapshot from the crypto sibling (M7c), if one
    /// was recorded: `verified`, `unverified`, `unknown`, or
    /// `verification_violation`. `None` for unencrypted events and rows decrypted
    /// before the snapshot existed.
    pub sender_trust: Option<String>,
    /// Relation aggregation (M8), resolved at read time. `true` when at least one
    /// *valid* `m.replace` edit (authored by the original sender, not redacted)
    /// targets this event; the row's [`content`](Self::content) /
    /// [`decrypted_body_text`](Self::decrypted_body_text) are the latest edit's
    /// replacement rather than the original. Always `false` for a redacted row
    /// (redaction masks the edit too) and for reads that don't aggregate.
    pub edited: bool,
    /// Number of valid edits targeting this event (M8). `0` when unedited.
    pub edit_count: i64,
    /// `origin_server_ts` of the winning (latest) edit, in milliseconds (M8).
    /// `None` when unedited or redacted.
    pub latest_edit_ts: Option<i64>,
    /// Per-emoji reaction tally (M8) as a JSON object
    /// `{ "👍": { "count": 2, "senders": [...], "me": true }, … }`, resolved over
    /// every non-redacted `m.annotation` targeting this event regardless of the
    /// timeline window. `None` when the event has no reactions or for reads that
    /// don't aggregate.
    pub reactions: Option<Value>,
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
            sender_trust: row.try_get("sender_trust")?,
            edited: row.try_get("edited")?,
            edit_count: row.try_get("edit_count")?,
            latest_edit_ts: row.try_get("latest_edit_ts")?,
            reactions: row.try_get("reactions")?,
        })
    }
}

/// Shared SELECT projection for timeline reads, with read-time redaction masking
/// **and M8 relation aggregation** (edit collapse + reaction tally). Three
/// `LEFT JOIN LATERAL`s enrich each event `e`:
///
/// * `r` — the redaction match (`LIMIT 1` so a multiply-redacted row stays a
///   single row; a plain JOIN would duplicate it and corrupt page size). When
///   set, `content`/`decrypted_body_text` are masked to `NULL` and
///   `redaction_event_id` is the redaction's id.
/// * `edit` — the latest *valid* `m.replace` targeting `e`, resolved by the M8
///   rules: edit and target must be in the same room (relations are room-local);
///   edit `sender` must equal `e`'s sender (authorship, unenforced upstream per
///   ADR 0021); edit and target must share the same `event_type`; neither may be
///   a state event and the target must not itself be an `m.replace` (MSC2676);
///   the replacement `msgtype` must match the target's; and the edit must be
///   well-formed (`m.new_content` present) and not itself redacted.
///   `ORDER BY origin_ts DESC, id DESC LIMIT 1` makes the winner deterministic
///   when timestamps collide; `COUNT(*) OVER ()` carries the total valid-edit
///   count alongside the winner. The replacement body/content override the
///   original, **unless the target is redacted** — redaction wins (and zeroes
///   `edit_count` too).
/// * `react` — every non-redacted `m.reaction` (`m.annotation`) in the same room
///   targeting `e`, grouped by `key` into a `{key: {count, senders, me}}` JSON
///   object; `(sender, key)` is deduplicated via `COUNT(DISTINCT sender)`, and
///   `me` compares against the account's own `user_id` (the `accounts` join
///   `ac`). Restricted to the `m.reaction` event type — generic `m.annotation`
///   aggregation is tracked separately (GH issue #112).
///
/// Callers append their own `WHERE` (binding `account_id` as `$1`) plus ordering
/// / pagination. Selects exactly the columns [`TimelineRow`] reads.
const TIMELINE_SELECT: &str =
    "SELECT e.id, e.event_id, e.room_id, e.sender, e.raw_event->>'state_key' AS state_key, \
            e.origin_ts, e.event_type, \
            CASE WHEN r.event_id IS NOT NULL THEN NULL \
                 WHEN edit.new_content IS NOT NULL THEN edit.new_content \
                 ELSE e.content END AS content, \
            CASE WHEN r.event_id IS NOT NULL THEN NULL \
                 WHEN edit.new_body IS NOT NULL THEN edit.new_body \
                 ELSE e.decrypted_body_text END AS decrypted_body_text, \
            e.relates_to, e.redacts, r.event_id AS redaction_event_id, \
            sd.sender_trust, \
            (r.event_id IS NULL AND edit.new_content IS NOT NULL) AS edited, \
            CASE WHEN r.event_id IS NULL THEN COALESCE(edit.edit_count, 0) ELSE 0 END \
                AS edit_count, \
            CASE WHEN r.event_id IS NULL THEN edit.latest_edit_ts END AS latest_edit_ts, \
            react.reactions AS reactions \
     FROM events e \
     JOIN accounts ac ON ac.account_id = e.account_id \
     LEFT JOIN LATERAL ( \
         SELECT rr.event_id FROM events rr \
         WHERE rr.account_id = e.account_id \
           AND rr.event_type = 'm.room.redaction' \
           AND rr.redacts = e.event_id \
         LIMIT 1 \
     ) r ON TRUE \
     LEFT JOIN LATERAL ( \
         SELECT ev.content->'m.new_content'          AS new_content, \
                ev.content->'m.new_content'->>'body' AS new_body, \
                ev.origin_ts                          AS latest_edit_ts, \
                COUNT(*) OVER ()                       AS edit_count \
         FROM events ev \
         WHERE ev.account_id = e.account_id \
           AND ev.room_id = e.room_id \
           AND ev.relates_to->>'rel_type' = 'm.replace' \
           AND ev.relates_to->>'event_id' = e.event_id \
           AND ev.sender = e.sender \
           AND ev.event_type = e.event_type \
           AND ev.raw_event->>'state_key' IS NULL \
           AND e.raw_event->>'state_key' IS NULL \
           AND e.relates_to->>'rel_type' IS DISTINCT FROM 'm.replace' \
           AND ev.content->'m.new_content' IS NOT NULL \
           AND (ev.content->'m.new_content'->>'msgtype' \
                IS NOT DISTINCT FROM e.content->>'msgtype') \
           AND NOT EXISTS ( \
               SELECT 1 FROM events rr \
               WHERE rr.account_id = ev.account_id \
                 AND rr.event_type = 'm.room.redaction' \
                 AND rr.redacts = ev.event_id \
           ) \
         ORDER BY ev.origin_ts DESC, ev.id DESC \
         LIMIT 1 \
     ) edit ON TRUE \
     LEFT JOIN LATERAL ( \
         SELECT jsonb_object_agg(t.key, t.tally) AS reactions \
         FROM ( \
             SELECT rx.relates_to->>'key' AS key, \
                    jsonb_build_object( \
                        'count', COUNT(DISTINCT rx.sender), \
                        'senders', jsonb_agg(DISTINCT rx.sender), \
                        'me', bool_or(rx.sender = ac.user_id) \
                    ) AS tally \
             FROM events rx \
             WHERE rx.account_id = e.account_id \
               AND rx.room_id = e.room_id \
               AND rx.event_type = 'm.reaction' \
               AND rx.relates_to->>'rel_type' = 'm.annotation' \
               AND rx.relates_to->>'event_id' = e.event_id \
               AND rx.relates_to->>'key' IS NOT NULL \
               AND NOT EXISTS ( \
                   SELECT 1 FROM events rr \
                   WHERE rr.account_id = rx.account_id \
                     AND rr.event_type = 'm.room.redaction' \
                     AND rr.redacts = rx.event_id \
               ) \
             GROUP BY rx.relates_to->>'key' \
         ) t \
     ) react ON TRUE \
     LEFT JOIN event_sender_device_keys sd \
         ON sd.account_id = e.account_id AND sd.event_id = e.event_id";

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
    ///
    /// **Exception — `sender_trust` is write-once-non-null.** The four-valued
    /// verdict is an *immutable at-decrypt snapshot* (M7c, ADR 0031): the trust
    /// Matrix's evidence reported when the event first decrypted. A duplicate
    /// delivery or a later re-decryption after the sender's trust changed must
    /// not rewrite history, so it is `COALESCE`d — the first recorded non-null
    /// verdict wins, while a still-`NULL` value (the initial UTD never decrypted)
    /// is still populated by the re-decryption that first reads the event. The
    /// coarse legacy `verification_state` keeps its overwrite contract untouched.
    ///
    /// Returns the **effective stored `sender_trust`** after the upsert (the
    /// `COALESCE`d value), so a caller emitting a live `timeline.event` frame can
    /// advertise the *persisted* verdict rather than the freshly-derived one —
    /// the two diverge on a duplicate delivery whose trust changed, and the wire
    /// contract is that the live frame carries the same frozen snapshot as the
    /// HTTP read (ADR 0031).
    pub async fn upsert_event_crypto(
        &self,
        c: &EventCrypto<'_>,
    ) -> Result<Option<String>, StoreError> {
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

        let stored = sqlx_core::query::query(
            "INSERT INTO event_sender_device_keys \
             (account_id, event_id, device_id, curve25519_key, ed25519_key, verification_state, sender_trust) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (account_id, event_id) DO UPDATE SET \
               device_id = EXCLUDED.device_id, \
               curve25519_key = EXCLUDED.curve25519_key, \
               ed25519_key = EXCLUDED.ed25519_key, \
               verification_state = EXCLUDED.verification_state, \
               sender_trust = COALESCE(event_sender_device_keys.sender_trust, EXCLUDED.sender_trust) \
             RETURNING sender_trust",
        )
        .bind(c.account_id)
        .bind(c.event_id)
        .bind(c.device_id)
        .bind(c.curve25519_key)
        .bind(c.ed25519_key)
        .bind(c.verification_state)
        .bind(c.sender_trust)
        .fetch_one(&self.pool)
        .await?;
        Ok(stored.try_get("sender_trust")?)
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
    ///
    /// Relations are **collapsed** (M8): standalone `m.replace` edit events and
    /// `m.annotation` reaction events are excluded from the page — they are
    /// surfaced instead on their target row (the latest edited body in place,
    /// plus [`edited`](TimelineRow::edited) / [`edit_count`](TimelineRow::edit_count)
    /// / [`reactions`](TimelineRow::reactions)), so a client gets the resolved view
    /// regardless of where the relation landed in the timeline. Replies and thread
    /// members keep their own rows. The forensic edit events stay on disk; only
    /// the read collapses them.
    pub async fn room_timeline(
        &self,
        account_id: Uuid,
        room_id: &str,
        before: Option<TimelineCursor>,
        limit: i64,
    ) -> Result<Vec<TimelineRow>, StoreError> {
        // The redaction match (in TIMELINE_SELECT) is a LATERAL subselect with
        // LIMIT 1 so a target event redacted more than once still yields a single
        // row (a plain JOIN would duplicate it and corrupt the page size). The
        // strip drops standalone relation events now folded into their target:
        // every `m.replace` (edited body shown in place), but `m.annotation` only
        // when it is an `m.reaction` (the type we aggregate into the per-event
        // tally). Non-`m.reaction` annotations are neither aggregated (GH #112)
        // nor collapsed, so they must stay visible as raw rows. `COALESCE(...,'')`
        // keeps NULL-`rel_type` rows (ordinary messages, replies) visible.
        let mut sql = format!(
            "{TIMELINE_SELECT} WHERE e.account_id = $1 AND e.room_id = $2 \
             AND COALESCE(e.relates_to->>'rel_type', '') <> 'm.replace' \
             AND NOT (COALESCE(e.relates_to->>'rel_type', '') = 'm.annotation' \
                      AND e.event_type = 'm.reaction')"
        );
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

    /// Read the at-decrypt sender-trust snapshot for `(account_id, event_id)` —
    /// the event's `sender` plus, if the event was decrypted, the crypto
    /// sibling's recorded device keys and trust verdicts. Returns `None` only
    /// when no such event exists for the account (a `404`); an event with no
    /// crypto sibling (unencrypted, or a UTD not yet re-decrypted) comes back
    /// with the `sender` set and the sibling fields `None`. The first production
    /// reader of `event_sender_device_keys` (M7c verification bundle).
    pub async fn event_sender_trust(
        &self,
        account_id: Uuid,
        event_id: &str,
    ) -> Result<Option<EventSenderTrust>, StoreError> {
        let row = sqlx_core::query_as::query_as::<Postgres, EventSenderTrust>(
            "SELECT e.sender, sd.device_id, sd.curve25519_key, sd.ed25519_key, \
                    sd.verification_state, sd.sender_trust, \
                    ms.session_id, ms.forwarded, ms.forwarder_user_id, ms.forwarder_device_id \
             FROM events e \
             LEFT JOIN event_sender_device_keys sd \
                 ON sd.account_id = e.account_id AND sd.event_id = e.event_id \
             LEFT JOIN event_megolm_session ms \
                 ON ms.account_id = e.account_id AND ms.event_id = e.event_id \
             WHERE e.account_id = $1 AND e.event_id = $2",
        )
        .bind(account_id)
        .bind(event_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// The forensic edit history of an event (M8): every `m.replace` targeting
    /// `event_id`, oldest first, *unfiltered* by the resolution rules. Unlike the
    /// collapsed timeline (which surfaces only the latest valid edit), this is the
    /// raw trail — including edits from a non-sender or an incompatible
    /// `msgtype` — for clients that want to show or audit "edited N times". Each
    /// row's `content` is that edit event's own content (with `m.new_content`).
    /// Redacted edits come back masked like any other redacted row.
    pub async fn event_edits(
        &self,
        account_id: Uuid,
        event_id: &str,
    ) -> Result<Vec<TimelineRow>, StoreError> {
        let sql = format!(
            "{TIMELINE_SELECT} WHERE e.account_id = $1 \
               AND e.relates_to->>'rel_type' = 'm.replace' \
               AND e.relates_to->>'event_id' = $2 \
               AND e.room_id = ( \
                   SELECT t.room_id FROM events t \
                   WHERE t.account_id = $1 AND t.event_id = $2 LIMIT 1 \
               ) \
             ORDER BY e.origin_ts ASC, e.id ASC"
        );
        let rows = sqlx_core::query_as::query_as::<Postgres, TimelineRow>(&sql)
            .bind(account_id)
            .bind(event_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    /// Per-emoji reaction tally for an event (M8): every non-redacted
    /// `m.reaction` (`m.annotation`) in the target's room, grouped by `key`.
    /// `(sender, key)` is deduplicated so a sender who reacted with the same emoji
    /// twice counts once, and `me` reports whether this account's own user
    /// reacted. Restricted to the `m.reaction` event type — generic
    /// `m.annotation` aggregation by `(event_type, key)` is tracked separately
    /// (GH issue #112). Resolves over every reaction on disk regardless of the
    /// timeline window — the fix for the dropped-reaction bug (GH issue #22).
    /// Empty when the event has no reactions.
    pub async fn event_reactions(
        &self,
        account_id: Uuid,
        event_id: &str,
    ) -> Result<Vec<ReactionTally>, StoreError> {
        let rows = sqlx_core::query_as::query_as::<Postgres, ReactionTally>(
            "SELECT rx.relates_to->>'key'        AS key, \
                    COUNT(DISTINCT rx.sender)     AS count, \
                    array_agg(DISTINCT rx.sender) AS senders, \
                    bool_or(rx.sender = ac.user_id) AS me \
             FROM events rx \
             JOIN accounts ac ON ac.account_id = rx.account_id \
             WHERE rx.account_id = $1 \
               AND rx.event_type = 'm.reaction' \
               AND rx.relates_to->>'rel_type' = 'm.annotation' \
               AND rx.relates_to->>'event_id' = $2 \
               AND rx.relates_to->>'key' IS NOT NULL \
               AND rx.room_id = ( \
                   SELECT t.room_id FROM events t \
                   WHERE t.account_id = $1 AND t.event_id = $2 LIMIT 1 \
               ) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM events rr \
                   WHERE rr.account_id = rx.account_id \
                     AND rr.event_type = 'm.room.redaction' \
                     AND rr.redacts = rx.event_id \
               ) \
             GROUP BY rx.relates_to->>'key' \
             ORDER BY count DESC, key ASC",
        )
        .bind(account_id)
        .bind(event_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Direct replies to an event (M8): events whose nested
    /// `m.relates_to.m.in_reply_to.event_id` is `event_id` **and** which carry no
    /// `rel_type` — that last clause is what distinguishes a plain reply from a
    /// thread member (which also nests an `m.in_reply_to` fallback). Oldest first,
    /// redaction-masked: a redacted reply is still returned (structure preserved)
    /// but with its content masked.
    pub async fn event_replies(
        &self,
        account_id: Uuid,
        event_id: &str,
    ) -> Result<Vec<TimelineRow>, StoreError> {
        let sql = format!(
            "{TIMELINE_SELECT} WHERE e.account_id = $1 \
               AND e.relates_to->>'rel_type' IS NULL \
               AND e.relates_to->'m.in_reply_to'->>'event_id' = $2 \
               AND e.room_id = ( \
                   SELECT t.room_id FROM events t \
                   WHERE t.account_id = $1 AND t.event_id = $2 LIMIT 1 \
               ) \
             ORDER BY e.origin_ts ASC, e.id ASC"
        );
        let rows = sqlx_core::query_as::query_as::<Postgres, TimelineRow>(&sql)
            .bind(account_id)
            .bind(event_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    /// The threads in a room (M8): one summary per distinct `m.thread` root, most
    /// recently active first. `reply_count` counts thread members including
    /// redacted ones (structure is preserved so a redacted root doesn't make its
    /// thread vanish); the latest reply is resolved deterministically by
    /// `(origin_ts, id)`. The root event itself is not a member — fetch it with
    /// [`get_event`](Self::get_event) or page the thread with
    /// [`thread_timeline`](Self::thread_timeline).
    pub async fn room_threads(
        &self,
        account_id: Uuid,
        room_id: &str,
    ) -> Result<Vec<ThreadSummary>, StoreError> {
        let rows = sqlx_core::query_as::query_as::<Postgres, ThreadSummary>(
            "SELECT th.root_event_id, th.reply_count, \
                    th.latest_reply_event_id, th.latest_reply_ts \
             FROM ( \
                 SELECT relates_to->>'event_id' AS root_event_id, \
                        COUNT(*)                  AS reply_count, \
                        (array_agg(event_id ORDER BY origin_ts DESC, id DESC))[1] \
                            AS latest_reply_event_id, \
                        MAX(origin_ts)            AS latest_reply_ts \
                 FROM events \
                 WHERE account_id = $1 AND room_id = $2 \
                   AND relates_to->>'rel_type' = 'm.thread' \
                 GROUP BY relates_to->>'event_id' \
             ) th \
             ORDER BY th.latest_reply_ts DESC, th.root_event_id",
        )
        .bind(account_id)
        .bind(room_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// A thread-scoped timeline read (M8): the `m.thread` members whose root is
    /// `root_id`, newest first, with the same `(origin_ts, id)` cursor pagination
    /// and redaction masking as [`room_timeline`](Self::room_timeline). Reactions
    /// and edits to thread members are aggregated onto their rows like any other
    /// timeline read. The thread root is not included.
    pub async fn thread_timeline(
        &self,
        account_id: Uuid,
        room_id: &str,
        root_id: &str,
        before: Option<TimelineCursor>,
        limit: i64,
    ) -> Result<Vec<TimelineRow>, StoreError> {
        let mut sql = format!(
            "{TIMELINE_SELECT} WHERE e.account_id = $1 AND e.room_id = $2 \
               AND e.relates_to->>'rel_type' = 'm.thread' \
               AND e.relates_to->>'event_id' = $3"
        );
        if before.is_some() {
            sql.push_str(" AND (e.origin_ts, e.id) < ($4, $5)");
        }
        sql.push_str(" ORDER BY e.origin_ts DESC, e.id DESC LIMIT ");
        sql.push_str(if before.is_some() { "$6" } else { "$4" });

        let mut q = sqlx_core::query_as::query_as::<Postgres, TimelineRow>(&sql)
            .bind(account_id)
            .bind(room_id)
            .bind(root_id);
        if let Some(cursor) = before {
            q = q.bind(cursor.origin_ts).bind(cursor.id);
        }
        let rows = q.bind(limit).fetch_all(&self.pool).await?;
        Ok(rows)
    }

    /// Find the first event for `account_id` whose media URL matches `mxc_url`.
    /// Checks primary and thumbnail URLs for both plain and encrypted media so
    /// the caller can recover the matching encryption descriptor when needed.
    /// Each JSON path is a separate branch so PostgreSQL can use the matching
    /// partial expression index rather than filtering every event for an
    /// account through one four-way `OR`.
    pub async fn get_event_by_mxc_url(
        &self,
        account_id: Uuid,
        mxc_url: &str,
    ) -> Result<Option<TimelineRow>, StoreError> {
        let sql = format!(
            "{TIMELINE_SELECT} \
             WHERE e.account_id = $1 \
               AND e.id = ( \
                   SELECT id FROM ( \
                       SELECT id FROM events \
                       WHERE account_id = $1 AND content->>'url' = $2 \
                       UNION ALL \
                       SELECT id FROM events \
                       WHERE account_id = $1 AND content->'file'->>'url' = $2 \
                       UNION ALL \
                       SELECT id FROM events \
                       WHERE account_id = $1 AND content->'info'->>'thumbnail_url' = $2 \
                       UNION ALL \
                       SELECT id FROM events \
                       WHERE account_id = $1 \
                         AND content->'info'->'thumbnail_file'->>'url' = $2 \
                   ) media_matches \
                   LIMIT 1 \
               )"
        );
        let row = sqlx_core::query_as::query_as::<Postgres, TimelineRow>(&sql)
            .bind(account_id)
            .bind(mxc_url)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row)
    }
}

/// One emoji's reaction tally for an event (M8), from
/// [`Store::event_reactions`].
#[derive(Debug, Clone)]
pub struct ReactionTally {
    /// The reaction key — typically an emoji, e.g. `👍`.
    pub key: String,
    /// Number of distinct senders who reacted with this key (deduplicated).
    pub count: i64,
    /// The distinct Matrix user ids that reacted with this key.
    pub senders: Vec<String>,
    /// Whether this Axon account's own user is among the senders.
    pub me: bool,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for ReactionTally {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(ReactionTally {
            key: row.try_get("key")?,
            count: row.try_get("count")?,
            senders: row.try_get("senders")?,
            me: row.try_get("me")?,
        })
    }
}

/// A thread summary for a room (M8), from [`Store::room_threads`]: the thread
/// root, its reply count, and the latest reply.
#[derive(Debug, Clone)]
pub struct ThreadSummary {
    /// The `event_id` of the thread root (the event the members relate to).
    pub root_event_id: String,
    /// Number of thread members, counting redacted ones for structure.
    pub reply_count: i64,
    /// The `event_id` of the most recent thread member, if any.
    pub latest_reply_event_id: Option<String>,
    /// `origin_server_ts` of the most recent member, in milliseconds.
    pub latest_reply_ts: Option<i64>,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for ThreadSummary {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(ThreadSummary {
            root_event_id: row.try_get("root_event_id")?,
            reply_count: row.try_get("reply_count")?,
            latest_reply_event_id: row.try_get("latest_reply_event_id")?,
            latest_reply_ts: row.try_get("latest_reply_ts")?,
        })
    }
}

/// The at-decrypt sender-trust snapshot for one event (M7c). The sibling fields
/// are `None` when the event has no `event_sender_device_keys` row (unencrypted
/// events, or a UTD not yet re-decrypted); `verification_state.is_some()` is the
/// signal that a snapshot was recorded (the column is `NOT NULL` in the sibling).
#[derive(Debug, Clone)]
pub struct EventSenderTrust {
    /// Matrix user id of the event's sender (always present).
    pub sender: String,
    /// The sending device's id at decrypt time.
    pub device_id: Option<String>,
    /// The sending device's curve25519 identity key.
    pub curve25519_key: Option<String>,
    /// The sending device's claimed ed25519 signing key.
    pub ed25519_key: Option<String>,
    /// The coarse `verified`/`unverified` verification state at decrypt time.
    pub verification_state: Option<String>,
    /// The four-valued sender-trust verdict at decrypt time.
    pub sender_trust: Option<String>,
    /// The Megolm session id the event was encrypted with (from the sibling
    /// `event_megolm_session` row; `None` for an unencrypted event or a UTD not
    /// yet re-decrypted).
    pub session_id: Option<String>,
    /// Whether the Megolm key reached us forwarded (key-share) rather than
    /// directly from the sender's device. `None` when no session row exists.
    pub forwarded: Option<bool>,
    /// If forwarded, the user id that forwarded the key.
    pub forwarder_user_id: Option<String>,
    /// If forwarded, the device id that forwarded the key.
    pub forwarder_device_id: Option<String>,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for EventSenderTrust {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(EventSenderTrust {
            sender: row.try_get("sender")?,
            device_id: row.try_get("device_id")?,
            curve25519_key: row.try_get("curve25519_key")?,
            ed25519_key: row.try_get("ed25519_key")?,
            verification_state: row.try_get("verification_state")?,
            sender_trust: row.try_get("sender_trust")?,
            session_id: row.try_get("session_id")?,
            forwarded: row.try_get("forwarded")?,
            forwarder_user_id: row.try_get("forwarder_user_id")?,
            forwarder_device_id: row.try_get("forwarder_device_id")?,
        })
    }
}
