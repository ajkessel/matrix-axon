-- Path B (native identity token) single-use replay defense (M14b, ADR 0054).
--
-- Path B hands axon a provider-signed identity token directly (no redirect
-- leg, so no PKCE to protect it); the explicit mitigation is single-use
-- consumption. `replay_key` is the token's `jti` claim, or a hash of the
-- token itself when the provider omits `jti`. A second presentation of the
-- same token is rejected outright: the insert is the concurrency control
-- (`ON CONFLICT DO NOTHING`, then check rows-affected), not a read-then-write
-- check, so two concurrent redemptions of the same stolen token can't both
-- win the race.
--
-- No expiry column, and deliberately no sweep: a row here is never deleted,
-- so this table grows without bound over the server's lifetime (one row per
-- Path B redemption, forever) — it is *not* naturally bounded by the token's
-- own `exp`, since expiry only governs whether axon accepts the token before
-- ever reaching this table, not whether the row it leaves behind is cleaned
-- up. Accepted explicitly for now: at personal-server request volume this
-- table stays tiny for years, and a `jti`/hash primary key keeps lookups
-- O(1) regardless of row count. Revisit with a TTL sweep (mirroring
-- `oauth_authorization_requests`'s) if usage patterns change.
CREATE TABLE oauth_consumed_identity_tokens (
    provider     TEXT        NOT NULL,
    replay_key   TEXT        NOT NULL,
    consumed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, replay_key)
);
