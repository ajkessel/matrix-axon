-- Current room state, one row per state tuple. See ADR 0016.
--
-- Matrix room state is the set of state events keyed by (type, state_key); the
-- *current* value of each is the latest such event. This is an ordinary table the
-- sync handlers keep current on every write: each sync upsert overwrites the
-- tuple's row, so a read is a point lookup rather than a fold over history. The
-- raw state events still land in `events` (state events are part of the timeline)
-- — this table is the resolved current-value view a room-summary read needs
-- (name, topic, avatar, members, …).
--
-- The PK is the Matrix state identity (account_id, room_id, event_type,
-- state_key). `state_key` is '' for singleton state (m.room.name, m.room.topic)
-- and the target user id for m.room.member, exactly as Matrix defines it.
CREATE TABLE room_state (
    account_id  UUID        NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    room_id     TEXT        NOT NULL,
    event_type  TEXT        NOT NULL,
    state_key   TEXT        NOT NULL,
    event_id    TEXT        NOT NULL,           -- the event currently holding this tuple
    sender      TEXT        NOT NULL,
    origin_ts   BIGINT      NOT NULL,           -- guards against older replays clobbering newer state
    content     JSONB,                          -- NULL for a redacted state event
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, room_id, event_type, state_key)
);

-- Reuse the shared trigger from the accounts migration so reads can rely on a
-- maintained `updated_at` without the application setting it.
CREATE TRIGGER room_state_set_updated_at
    BEFORE UPDATE ON room_state
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();
