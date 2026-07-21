-- Server-derived per-room unread counts (issue #313, ADR 0070).
--
-- This table caches a value Axon does not compute: `notification_count` and
-- `highlight_count` are matrix-sdk's already-derived read of the upstream
-- homeserver's own sync room-summary (`unread_notifications`), itself the
-- product of the homeserver's push-rule evaluation. Axon's sync engine writes
-- here on every notable room-info update purely so a fresh client load can
-- read a number without waiting to observe a live event first; the source of
-- truth remains the homeserver.
--
-- Inherits matrix-sdk's documented encrypted-room caveat: in an encrypted
-- room, other users' reactions/edits/redactions travel as opaque
-- `m.room.encrypted` at the transport type, so the homeserver can't apply the
-- content-based push rules that would normally suppress notifications for
-- those event kinds and falls back to `.m.rule.encrypted` (notify on most
-- events from others). See ADR 0070.
CREATE TABLE room_unread_counts (
    account_id          UUID        NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    room_id              TEXT        NOT NULL,
    notification_count   BIGINT      NOT NULL DEFAULT 0,
    highlight_count       BIGINT      NOT NULL DEFAULT 0,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, room_id)
);

-- Reuse the shared trigger from the accounts migration.
CREATE TRIGGER room_unread_counts_set_updated_at
    BEFORE UPDATE ON room_unread_counts
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();
