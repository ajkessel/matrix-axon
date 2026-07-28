-- Extend `tokens` for OAuth-minted bearer tokens (M14b, ADR 0054).
--
-- All four columns are nullable and default NULL, so every existing
-- CLI-minted row (`axon token issue`) is byte-for-byte unaffected: NULL
-- `expires_at` means "never expires" (the CLI path's existing behavior),
-- and the other three columns simply don't apply to a non-OAuth token.
--
--   expires_at         when this access token stops verifying. `verify_token`
--                       gains `AND (expires_at IS NULL OR expires_at > now())`.
--   provider           which upstream OIDC provider (if any) backed the login
--                       that minted this token — audit/debugging only.
--   oauth_identity_id   the bound identity this token was minted for.
--   client_id           which registered OAuth client (`[[oauth.clients]]`)
--                       redeemed the code/identity-token that minted this row.
ALTER TABLE tokens
    ADD COLUMN expires_at        TIMESTAMPTZ,
    ADD COLUMN provider          TEXT,
    ADD COLUMN oauth_identity_id UUID REFERENCES oauth_identities(id),
    ADD COLUMN client_id         TEXT;
