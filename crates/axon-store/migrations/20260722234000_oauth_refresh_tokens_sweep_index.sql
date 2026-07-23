-- Support Store::delete_stale_refresh_tokens' sweep (called on every
-- issue_refresh_token/redeem_refresh_token) without a full-table scan.
--
-- The sweep's DELETE is an OR of two mutually-exclusive branches — a revoked
-- row past its sweep window, or a never-revoked row past its own expiry and
-- sweep window — so it needs one partial index per branch; Postgres combines
-- them with a BitmapOr rather than scanning the whole table.
CREATE INDEX IF NOT EXISTS oauth_refresh_tokens_revoked_sweep_idx
    ON oauth_refresh_tokens (revoked_at)
    WHERE revoked_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS oauth_refresh_tokens_expires_sweep_idx
    ON oauth_refresh_tokens (expires_at)
    WHERE revoked_at IS NULL;

-- The `replaced_by` self-reference had no supporting index: deleting a row
-- means Postgres must check whether any other row's `replaced_by` points at
-- it (the FK's ON DELETE NO ACTION check), and without this index that check
-- is a full sequential scan *per deleted row* — the dominant cost once the
-- sweep starts deleting at volume, dwarfing the WHERE-clause scan above.
-- Not partial (unlike the two indexes above): FK-check queries bind
-- `replaced_by` through a prepared-statement parameter, and a plain index
-- reliably supports that lookup without depending on the planner proving a
-- partial predicate against a bind parameter.
CREATE INDEX IF NOT EXISTS oauth_refresh_tokens_replaced_by_idx
    ON oauth_refresh_tokens (replaced_by);
