-- OAuth refresh tokens (M14b, ADR 0054).
--
-- Deliberately a separate table from `tokens`, not an extension of it: access
-- tokens are checked on every `/v1/` request (the hot path `TokenVerifier`
-- walks), while refresh tokens are only ever looked up during a
-- `POST /v1/oauth/token` redemption. Keeping them apart means the hot-path
-- verifier never has to know refresh tokens exist.
--
-- Single-use with rotation: redeeming a refresh token revokes it and mints a
-- replacement, linked via `replaced_by`. Redeeming an already-revoked token
-- (reuse — a strong theft/replay signal) revokes every active refresh token
-- for that `(oauth_identity_id, client_id)` pair in one statement (not a
-- chain-walk) — a deliberately blunt "sign this client out everywhere for
-- this identity" response, simple to implement correctly under concurrency.
--
--   hash          same treatment as `tokens.hash` (SHA-256, base64): a
--                 high-entropy secret, so a plain hash suffices.
--   replaced_by   set on rotation to the row that superseded this one; NULL
--                 while still the live token in its chain.
CREATE TABLE oauth_refresh_tokens (
    id                UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    hash              TEXT        NOT NULL UNIQUE,
    oauth_identity_id UUID        NOT NULL REFERENCES oauth_identities(id),
    client_id         TEXT        NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at        TIMESTAMPTZ NOT NULL,
    revoked_at        TIMESTAMPTZ,
    replaced_by       UUID REFERENCES oauth_refresh_tokens(id)
);

-- Serves the reuse-detection revoke: "every active refresh token for this
-- identity+client".
CREATE INDEX oauth_refresh_tokens_identity_client_idx
    ON oauth_refresh_tokens (oauth_identity_id, client_id)
    WHERE revoked_at IS NULL;
