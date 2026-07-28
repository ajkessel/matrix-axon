-- Client→axon bearer tokens (M7b).
--
-- The local-API gate: every `/v1/` request (HTTP and WebSocket) must present a
-- bearer token that hashes to an unrevoked row here. Tokens are minted
-- out-of-band by the `axon token issue` CLI, which prints the raw secret once and
-- stores only its hash — the raw value is never recoverable from this table.
--
-- Tokens are global to the Axon instance, not account-scoped: one human owns all
-- of their Matrix accounts, so there is deliberately no `account_id` column
-- (per-account authorization is a non-goal).
--
--   hash          a one-way hash of the raw token (SHA-256, base64). High-entropy
--                 random secret, so a plain hash — not a password KDF — is the
--                 right primitive; verification is a single indexed equality
--                 lookup. UNIQUE both enforces no-collision and provides the
--                 lookup index.
--   last_used_at  stamped on every successful verification (NULL until first use).
--   revoked_at    set by `axon token revoke`; a non-NULL value fails verification.
--                 Revocation is a tombstone, not a delete, so the audit row (label,
--                 created_at, last_used_at) survives.
CREATE TABLE tokens (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    label        TEXT        NOT NULL,
    hash         TEXT        NOT NULL UNIQUE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at   TIMESTAMPTZ
);
