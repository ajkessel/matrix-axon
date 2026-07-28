-- Support Store::delete_expired_media_uploads' periodic sweep (M15c, ADR
-- 0059, GH #286) without a full-table scan.
--
-- The sweep filters on `state = 'staged' AND expires_at <= now()` with no
-- `account_id`, so it can't use the leading column of
-- `media_uploads_account_state_expires_idx (account_id, state, expires_at)`.
-- Partial on `state = 'staged'` for the same reason as the oauth
-- refresh-token sweep indexes: `sending` rows are never a sweep target, so
-- there's no reason to carry them in the index.
CREATE INDEX IF NOT EXISTS media_uploads_expiry_sweep_idx
    ON media_uploads (expires_at)
    WHERE state = 'staged';
