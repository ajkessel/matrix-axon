-- Per-device client state: drafts, read markers, and similar (M12, ADR 0048).
--
-- A generic KV store scoped to (account, device, namespace, key). A "device"
-- here is a *client of axon* (one TUI/web install), identified by a
-- client-supplied UUID whose first PUT is its registration — not a Matrix
-- device. Values are opaque JSONB axon never interprets; NULL is a tombstone
-- (the key was deleted on that device), kept as a row so a cleared draft
-- *wins* the cross-device last-write-wins merge instead of letting another
-- device's older value resurface.
--
-- Last-write-wins is on the server clock: `updated_at` is bumped by the shared
-- trigger on every UPDATE (DEFAULT now() on insert), so the last PUT to arrive
-- wins and client clocks are never trusted. The merged read
-- (per-key newest across an account's devices) is served by the secondary
-- index below.
CREATE TABLE device_state (
    account_id UUID        NOT NULL REFERENCES accounts(account_id) ON DELETE CASCADE,
    device_id  UUID        NOT NULL,
    namespace  TEXT        NOT NULL,
    key        TEXT        NOT NULL,
    value      JSONB,                              -- NULL = tombstone
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, device_id, namespace, key)
);

-- Reuse the shared trigger from the accounts migration.
CREATE TRIGGER device_state_set_updated_at
    BEFORE UPDATE ON device_state
    FOR EACH ROW EXECUTE FUNCTION trigger_set_updated_at();

-- Serves the merged GET: all devices' rows for one (account, namespace).
CREATE INDEX device_state_merge_idx ON device_state (account_id, namespace, key);
