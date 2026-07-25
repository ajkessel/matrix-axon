-- Staged client-originated media uploads (M15a, ADR 0059).
--
-- Unlike the M11 media cache, these rows point at durable local files that are
-- in-flight mutations. They survive restart until sent, deleted, or expired.
CREATE TABLE media_uploads (
    upload_id    UUID        PRIMARY KEY,
    account_id   UUID        NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    kind         TEXT        NOT NULL CHECK (kind IN ('image', 'file')),
    filename     TEXT        NOT NULL CHECK (filename <> ''),
    content_type TEXT,
    size_bytes   BIGINT      NOT NULL CHECK (size_bytes >= 0),
    path         TEXT        NOT NULL CHECK (path <> ''),
    state        TEXT        NOT NULL DEFAULT 'staged' CHECK (state IN ('staged', 'sending')),
    expires_at   TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (account_id, upload_id)
);

CREATE TRIGGER media_uploads_set_updated_at
    BEFORE UPDATE ON media_uploads
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

-- Serves account-scoped lookup/delete. The M15c expiry sweep is unscoped by
-- account, so it needs its own index — see
-- 20260725120000_media_uploads_expiry_sweep_index.sql.
CREATE INDEX media_uploads_account_state_expires_idx
    ON media_uploads (account_id, state, expires_at);
