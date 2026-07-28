-- The `axon oauth bind` browser leg's OIDC nonce (M14, ADR 0054 "CLI bind
-- command"). The bind flow reuses `oauth_bind_requests.device_code` itself as
-- the `state` value sent upstream (already a unique, unguessable UUID — no
-- new column needed for that), but still needs somewhere to stash a
-- server-generated nonce between "GET /v1/oauth/bind redirects upstream" and
-- "the callback verifies the returned id_token's nonce claim", the same role
-- `oauth_authorization_requests.upstream_nonce` plays for Path A.
ALTER TABLE oauth_bind_requests ADD COLUMN upstream_nonce TEXT;
