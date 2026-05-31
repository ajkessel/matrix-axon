CREATE TABLE events (
    id          BIGSERIAL    PRIMARY KEY,
    event_id    TEXT         NOT NULL,
    room_id     TEXT         NOT NULL,
    account_id  UUID         NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    sender      TEXT         NOT NULL,
    origin_ts   BIGINT       NOT NULL,
    event_type  TEXT         NOT NULL,
    content     JSONB,
    raw_event   JSONB        NOT NULL,
    provenance  TEXT         NOT NULL DEFAULT 'upstream_homeserver',
    received_at TIMESTAMPTZ  NOT NULL DEFAULT now(),
    UNIQUE (account_id, event_id)
);

CREATE INDEX events_room_timeline_idx ON events (account_id, room_id, origin_ts DESC);
