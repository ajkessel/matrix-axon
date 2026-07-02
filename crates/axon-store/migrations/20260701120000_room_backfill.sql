-- Per-room history-backfill progress (M10). See ADR 0043.
--
-- Live sync only ingests events going forward (plus the shallow
-- `sync.timeline_limit` window on a room's first sync — ADR 0015). The backfill
-- engine pages each room's pre-existing history *backward* through the SDK's
-- `/messages` pagination, persisting through the same ingestion path as live
-- sync. This table records, per room, where that backward paging has reached and
-- whether the room's upstream history is exhausted, so backfill resumes across
-- restarts and never re-pages a completed room.
--
-- One row per (account_id, room_id):
--   * oldest_seen_token — the SDK `Messages.end` from the last page: the token to
--     resume backward paging from. NULL before the first page (start from the
--     room's live timeline end).
--   * complete — upstream history is exhausted (`Messages.end` came back NULL), so
--     there is nothing older to fetch. A completed room is skipped.
--   * events_backfilled — running count of events pulled by backfill, so a bounded
--     target depth (`sync.backfill_target_depth`) can stop paging a room without
--     marking it complete; raising the cap later resumes it.
--
-- The row is a resume cursor, not derived data: it is deleted when the room is
-- purged (see `purge_room`), and cascades away with the account.
CREATE TABLE room_backfill (
    account_id        UUID        NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    room_id           TEXT        NOT NULL,
    oldest_seen_token TEXT,                                 -- SDK Messages.end; NULL before first page
    complete          BOOLEAN     NOT NULL DEFAULT false,   -- upstream history exhausted
    events_backfilled BIGINT      NOT NULL DEFAULT 0,       -- for the optional bounded target depth
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, room_id)
);

-- Reuse the shared trigger from the accounts migration so `updated_at` is
-- maintained without the application setting it.
CREATE TRIGGER room_backfill_set_updated_at
    BEFORE UPDATE ON room_backfill
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();
