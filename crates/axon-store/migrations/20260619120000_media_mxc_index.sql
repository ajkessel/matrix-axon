-- Partial expression indexes for get_event_by_mxc_url. Its separate lookup
-- branches let the planner use the matching index for each JSONB path.
CREATE INDEX IF NOT EXISTS events_content_url_idx
    ON events (account_id, (content->>'url'))
    WHERE (content->>'url') IS NOT NULL;

CREATE INDEX IF NOT EXISTS events_content_file_url_idx
    ON events (account_id, (content->'file'->>'url'))
    WHERE (content->'file'->>'url') IS NOT NULL;

CREATE INDEX IF NOT EXISTS events_content_thumbnail_url_idx
    ON events (account_id, (content->'info'->>'thumbnail_url'))
    WHERE (content->'info'->>'thumbnail_url') IS NOT NULL;

CREATE INDEX IF NOT EXISTS events_content_thumbnail_file_url_idx
    ON events (account_id, (content->'info'->'thumbnail_file'->>'url'))
    WHERE (content->'info'->'thumbnail_file'->>'url') IS NOT NULL;
