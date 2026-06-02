-- M4a: promote the remaining hot columns the timeline read path needs and add
-- the indexes for room listing and redaction lookup. See ADR 0015.
--
-- `redacts`            — for an `m.room.redaction` event, the target event_id it
--                        redacts. NULL for every other event. The timeline read
--                        masks a target row when a redaction pointing at it exists.
-- `relates_to`         — the event's `m.relates_to` object (rel_type + event_id),
--                        captured generically so replies, edits, annotations and
--                        `m.thread` relations are all preserved. No thread-specific
--                        indexing in the MVP (threads deferred).
-- `decrypted_body_text`— the plaintext `content.body` of a message event, lifted
--                        into its own column for fast timeline render and as the
--                        source the M9 search index ingests. NULL for events with
--                        no textual body (state events, UTDs, etc.).
ALTER TABLE events ADD COLUMN redacts             TEXT;
ALTER TABLE events ADD COLUMN relates_to          JSONB;
ALTER TABLE events ADD COLUMN decrypted_body_text TEXT;

-- Room listing / most-recent-activity scans across an account.
CREATE INDEX events_room_idx ON events (account_id, room_id);

-- Redaction lookup: given a target event, is there a redaction pointing at it?
-- Partial so the index only covers the (rare) redaction rows, not every event.
CREATE INDEX events_redacts_idx
    ON events (account_id, redacts)
    WHERE redacts IS NOT NULL;
