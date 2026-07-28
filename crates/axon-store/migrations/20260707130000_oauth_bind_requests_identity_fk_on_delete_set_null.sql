-- `axon oauth identities unbind` deletes an `oauth_identities` row directly.
-- `oauth_bind_requests.oauth_identity_id` previously had no `ON DELETE`
-- action (the default, `NO ACTION`), so deleting an identity that a
-- completed bind request still referenced failed with a foreign-key
-- violation — which is every identity ever actually bound through the
-- supported `axon oauth bind` flow, until that row happens to age past its
-- own `expires_at` and get swept by some later, unrelated `create_bind_request`
-- call.
--
-- `oauth_bind_requests` is disposable handshake bookkeeping, not an audit
-- trail (see its own migration's comment), so nulling the reference on
-- delete is correct: the completed row just loses a link to a now-gone
-- identity rather than blocking the delete.
ALTER TABLE oauth_bind_requests
    DROP CONSTRAINT oauth_bind_requests_oauth_identity_id_fkey,
    ADD CONSTRAINT oauth_bind_requests_oauth_identity_id_fkey
        FOREIGN KEY (oauth_identity_id) REFERENCES oauth_identities(id) ON DELETE SET NULL;
