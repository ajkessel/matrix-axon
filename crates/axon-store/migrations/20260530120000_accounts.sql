-- Accounts: one row per Matrix account this Axon process syncs.
--
-- "One human per Axon process, N Matrix accounts inside" — every
-- account-scoped table (events, room state, …) references account_id.
--
-- The access token is stored encrypted at rest via pgcrypto's
-- pgp_sym_encrypt (see ADR 0008); the column holds the BYTEA ciphertext, and
-- the symmetric key lives in config/env (sync.store_key), never in the DB. The
-- login password is consumed once at first login and is never persisted — only
-- the resulting token is kept.
CREATE TABLE accounts (
    account_id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id                 TEXT NOT NULL,
    homeserver_url          TEXT NOT NULL,
    device_id               TEXT,
    access_token_encrypted  BYTEA,
    -- Reserved: the SyncService manages its sync position in its own SQLite
    -- store, so this stays NULL for M3. Kept for forward compatibility.
    sync_token              TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, homeserver_url)
);
