-- Megolm session id for UTDs, promoted to a first-class hot column so the
-- re-decryption queue can find pending rows without parsing the raw envelope.
-- For a UTD (an `m.room.encrypted` row with content IS NULL), this holds the
-- `content.session_id` of the megolm session the message was encrypted with;
-- it is NULL for events that arrived already decrypted. See ADR 0014.
-- No backfill: NULL is correct for already-decrypted rows (no session to
-- track), and any pre-existing UTD left at NULL is still found by the startup
-- sweep (`pending_utds_for_account`, which filters only on `content IS NULL`).
-- A NULL only costs such a row the arrival-stream fast path, never correctness.
ALTER TABLE events ADD COLUMN megolm_session_id TEXT;

-- The queue's hot path: given a megolm session that just arrived, find every
-- pending UTD in that room waiting on it. Partial (content IS NULL) so the
-- index stays small — it only covers undecrypted rows, which drain over time.
CREATE INDEX events_pending_utd_idx
    ON events (account_id, room_id, megolm_session_id)
    WHERE content IS NULL;
