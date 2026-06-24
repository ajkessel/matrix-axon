-- Durable bookkeeping for the Tantivy full-text index (M9), which is a single
-- combined index across all accounts. Two tables, both serving the convergence
-- contract in ADR 0039: the index is *derived* data that must converge with the
-- authoritative event store, even across drops, disabled periods, and crashes.
--
-- `search_outbox` is a transactional change log. Every store write that can
-- change a resolved search document appends a work item in the *same
-- transaction* as the mutation, so the event change and its indexing obligation
-- commit atomically — the system can never persist one without the other. The
-- indexer drains it in `seq` order, applies each change to Tantivy, and only then
-- advances its durable cursor; a dropped in-memory wakeup therefore costs nothing
-- because the obligation is still on disk.
--
-- Entries are deliberately engine-neutral: `(account_id, event_id)` names *which*
-- resolved document may have changed, never a Tantivy document — the search crate
-- decides how to resolve and index it. `event_id = ''` is the account-purge
-- sentinel (delete every document for the account), written when an account row
-- is deleted.
--
-- IMPORTANT: no foreign key to `accounts`. The purge obligation is written in the
-- same transaction that deletes the account row, and must *outlive* it so boot
-- catch-up can complete the purge after a crash — or after a deletion that
-- happened while search was disabled (no indexer running). An FK would cascade
-- the obligation away with the row, which is exactly the breadcrumb we need kept.
CREATE TABLE search_outbox (
    seq        BIGSERIAL   NOT NULL PRIMARY KEY,
    account_id UUID        NOT NULL,
    event_id   TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- `search_meta` holds the indexer's durable cursor: `outbox_cursor` is the
-- highest `seq` whose effect has been applied to Tantivy *and* committed — a
-- contiguous complete prefix (the indexer advances it only after a Tantivy
-- commit, draining strictly in `seq` order), so it always names work that is
-- truly done, not merely observed. Outbox rows at or below it are prunable: the
-- rebuild source of truth is `events` (a full re-scan via `events_for_index`),
-- never the outbox, so pruning can never remove information needed to reconstruct
-- a lost index.
--
-- The index *schema version* is intentionally NOT stored here: it describes the
-- physical on-disk index, so it lives in a sidecar file inside the index
-- directory (see axon-search). A global DB marker can't tell whether the actual
-- index directory is new, empty, moved, or stale.
CREATE TABLE search_meta (
    key        TEXT        NOT NULL PRIMARY KEY,
    value      TEXT        NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Reuse the shared trigger from the accounts migration so `updated_at` is
-- maintained without the application setting it.
CREATE TRIGGER search_meta_set_updated_at
    BEFORE UPDATE ON search_meta
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();
