-- Short-term bound for permanently-undecryptable UTDs (issue 9).
--
-- The room-key arrival stream remains keyed by (account, room, session) and
-- ignores this marker. This column only gates the account-wide startup sweep so
-- a row that failed once is not retried on every boot unless the operator opts
-- into that behavior or triggers an explicit retry.
ALTER TABLE events ADD COLUMN utd_startup_redecrypt_attempted_at TIMESTAMPTZ;

CREATE INDEX events_pending_utd_startup_unattempted_idx
    ON events (account_id)
    WHERE content IS NULL AND utd_startup_redecrypt_attempted_at IS NULL;
