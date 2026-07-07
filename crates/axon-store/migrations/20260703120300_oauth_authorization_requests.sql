-- Path A (web-redirect) server-side bookkeeping (M14b, ADR 0054).
--
-- Path A is a double-redirect (client -> axon -> upstream provider -> axon),
-- so axon cannot rely on any client-side session surviving the hop through
-- the upstream IdP's domain; this table is that state, keyed by the `state`
-- value axon itself mints and hands to the client at `/v1/oauth/authorize`.
--
-- `status` is an explicit state machine, not inferred from which columns are
-- set, so every transition is a single auditable `WHERE status = '<from>'`
-- conditional UPDATE (the concurrency control — never read-then-write):
--   pending     -> row just created by GET /v1/oauth/authorize.
--   code_issued -> the provider's callback landed, the id_token verified, and
--                  axon minted its own authorization code, all in the same
--                  atomic UPDATE (oauth_identity_id and axon_code_hash are
--                  set together) — deliberately not two separate
--                  pending->upstream_returned->code_issued steps, so a crash
--                  or error between "identity verified" and "code minted"
--                  can never strand a row in an intermediate state with no
--                  path back out except waiting for `expires_at` to lapse.
--   redeemed    -> terminal: the code was exchanged at
--                  POST /v1/oauth/token for an access/refresh pair.
--   expired     -> terminal: the row's TTL lapsed before reaching `redeemed`.
CREATE TABLE oauth_authorization_requests (
    id                     UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    client_id              TEXT        NOT NULL,
    redirect_uri           TEXT        NOT NULL,
    code_challenge         TEXT        NOT NULL,
    code_challenge_method  TEXT        NOT NULL,
    -- The client's own `state` (RFC 6749 §4.1.1), if it sent one. Opaque to
    -- axon; echoed back verbatim on the final redirect to `redirect_uri` once
    -- Path A completes (§4.1.2) — distinct from `upstream_state` below, which
    -- is axon's *own* CSRF-binding state on the axon<->upstream-provider leg.
    client_state           TEXT,
    provider               TEXT        NOT NULL,
    upstream_state         TEXT        NOT NULL,
    upstream_nonce         TEXT        NOT NULL,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at             TIMESTAMPTZ NOT NULL,
    status                 TEXT        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'code_issued', 'redeemed', 'expired')),
    oauth_identity_id      UUID REFERENCES oauth_identities(id),
    axon_code_hash         TEXT
);

-- Every live lookup against this table is scoped to one status, so a partial
-- index over just that status's rows stays small regardless of how many
-- terminal (`redeemed`/`expired`) rows accumulate:
--   * the callback (`find_authorization_request_by_upstream_state`) looks up
--     a still-`pending` row by (provider, upstream_state);
--   * token redemption (`redeem_authorization_code`) looks up a
--     `code_issued` row by `axon_code_hash`.
CREATE INDEX oauth_authorization_requests_pending_lookup_idx
    ON oauth_authorization_requests (provider, upstream_state)
    WHERE status = 'pending';

CREATE INDEX oauth_authorization_requests_code_issued_lookup_idx
    ON oauth_authorization_requests (axon_code_hash)
    WHERE status = 'code_issued';

-- Backs the opportunistic expiry sweep in `Store::delete_expired_authorization_requests`
-- (`/v1/oauth/authorize` fires it on every new flow — this table sees no other
-- write path, and a 10-minute TTL keeps each sweep cheap).
CREATE INDEX oauth_authorization_requests_expires_at_idx
    ON oauth_authorization_requests (expires_at);
