-- M4b: account data — both global (account-wide) and per-room. See ADR 0016.
--
-- Account data events carry only a `type` and a `content` blob (no event_id,
-- sender, or origin_ts), and there is exactly one current value per type per
-- scope. Global data (push rules, m.direct, ignored users, …) is account-wide;
-- room data (fully-read markers, tags, …) is scoped to a room. We model both in
-- one table, distinguished by `room_id`.
--
-- `room_id` is '' (not NULL) for global account data: a real Matrix room id
-- always starts with '!', so '' is an unambiguous sentinel, and keeping the
-- column NOT NULL lets the natural PK (account_id, room_id, event_type) carry
-- the uniqueness directly — no expression index, and ON CONFLICT targets the PK
-- cleanly. (A nullable room_id would make the two global rows for one type
-- "distinct" under SQL NULL semantics, defeating the upsert.)
CREATE TABLE account_data (
    account_id UUID        NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    room_id    TEXT        NOT NULL DEFAULT '',   -- '' = global; '!room:hs' = per-room
    event_type TEXT        NOT NULL,
    content    JSONB       NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, room_id, event_type)
);

CREATE TRIGGER account_data_set_updated_at
    BEFORE UPDATE ON account_data
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();
