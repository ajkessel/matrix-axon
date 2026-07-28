-- Bound OAuth/OIDC identities (M14b, ADR 0054).
--
-- What `axon oauth bind` (M14c) populates: proof that a specific upstream
-- provider subject (`sub`) belongs to this axon instance's one human owner.
-- Like `tokens`, this is global to the instance rather than owned by any
-- Matrix account row — `accounts` is the *Matrix* account model (MXID,
-- homeserver), an orthogonal concept from "who is allowed to sign in to
-- axon itself."
--
-- `UNIQUE (provider, subject)` is the natural key: the same upstream identity
-- can only ever be bound once, and it is what Path A/B lookups join against
-- after verifying an id_token's `sub`.
CREATE TABLE oauth_identities (
    id         UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    provider   TEXT        NOT NULL,
    subject    TEXT        NOT NULL,
    email      TEXT,
    linked_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, subject)
);
