-- Partial expression index for get_event_by_mxc_url's member-avatar branch.
CREATE INDEX IF NOT EXISTS events_content_member_avatar_url_idx
    ON events (account_id, (content->>'avatar_url'))
    WHERE event_type = 'm.room.member'
      AND (content->>'avatar_url') IS NOT NULL;
