-- Account lifecycle state machine (ADR 0022).
--
-- `state` makes an account's lifecycle explicit and is the gate the sync engine
-- and the mutations gateway use to decide whether to connect and serve an
-- account: only `active` accounts are brought online. Rationale and history are
-- in ADR 0022.
--
--   active       the normal, syncing-and-sending state
--   deactivated  a reversible pause that retains all data (logout, or an
--                internal token failure); does not sync or send; a fresh login
--                reactivates the same row
--   deleting     a transient teardown breadcrumb set while a delete is in
--                flight; a boot-time reconcile drives any row left here to
--                completion. Never a resting state a client observes long-term;
--                deletion is a hard row removal, not a resting tombstone.
--
-- `verified` caches whether axon's own device is cross-signed. It is kept
-- orthogonal to `state` (a device can be active but not yet verified: it syncs
-- and shows UTDs until it acquires keys) and is derived from the SDK's current
-- cross-signing state rather than written once. That derivation is wired up in
-- a later subphase; it defaults false here.
--
-- Existing rows default to `active`: this migration introduces the gate but does
-- not on its own retire stale accounts (the reconcile + orphan-dir GC that do
-- land in 7a-4, per ADR 0022). Backfilling some rows to `deactivated` here would
-- need a liveness signal we don't have at migration time.
ALTER TABLE accounts
    ADD COLUMN state TEXT NOT NULL DEFAULT 'active'
        CHECK (state IN ('active', 'deactivated', 'deleting')),
    ADD COLUMN verified BOOLEAN NOT NULL DEFAULT false;
