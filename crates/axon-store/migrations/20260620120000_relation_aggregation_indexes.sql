-- Indexes that make server-side relation aggregation (M8) an indexed lookup
-- rather than a window scan. Both are additive and backfill-free: they cover
-- the relations already captured in the `events.relates_to` JSONB column
-- (ADR 0015), so they apply retroactively to every row stored before this
-- migration and to the deep history M10 backfills in later.
--
-- Matrix relation events come in two shapes, and aggregation must index both or
-- a whole class of relation is silently unfindable:
--
--   * `rel_type` relations — edits (`m.replace`), reactions (`m.annotation`),
--     and thread members (`m.thread`) all carry
--     `relates_to.rel_type` + `relates_to.event_id` (the target/root).
--   * replies carry NO `rel_type`; they nest the target under
--     `relates_to.m.in_reply_to.event_id`.
--
-- Each index keys "all relations pointing at event X" by `(account_id, …)` so
-- the aggregation reads stay account-scoped like every other table.

-- (a) rel_type relations, keyed by type + target. Generalizes the thread index
-- sketched in ADR 0017 to cover edits and reactions too. Partial so it only
-- covers the relation rows, not every event.
CREATE INDEX events_relation_target_idx
    ON events (account_id, (relates_to->>'rel_type'), (relates_to->>'event_id'))
    WHERE relates_to->>'rel_type' IS NOT NULL;

-- (b) reply target, the nested `m.in_reply_to.event_id`. Replies have no
-- `rel_type`, so index (a) never sees them; this is the only way "all replies
-- to event X" is an index lookup. Partial on the nested key's presence.
CREATE INDEX events_reply_target_idx
    ON events (account_id, (relates_to->'m.in_reply_to'->>'event_id'))
    WHERE relates_to->'m.in_reply_to'->>'event_id' IS NOT NULL;
