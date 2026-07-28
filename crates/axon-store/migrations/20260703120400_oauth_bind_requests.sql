-- `axon oauth bind` device-code handshake bookkeeping (M14c, ADR 0054).
--
-- Table only in M14b: the CLI verb that populates and polls it (`axon oauth
-- bind`) lands in M14c, so there is no caller yet — created now because the
-- shape is stable and cheap, matching the ADR's schema.
--
-- The CLI (talking directly to `Store`, like `axon token issue`) inserts a
-- pending row and prints a URL containing `user_code`; an admin opens it in
-- any browser, which drives Path A to bind an identity; the CLI polls this
-- row directly via `Store` until `completed`/`expired`.
CREATE TABLE oauth_bind_requests (
    device_code       UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_code         TEXT        NOT NULL UNIQUE,
    provider          TEXT        NOT NULL,
    status            TEXT        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'completed', 'expired')),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at        TIMESTAMPTZ NOT NULL,
    oauth_identity_id UUID REFERENCES oauth_identities(id)
);
