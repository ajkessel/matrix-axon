-- Baseline migration.
--
-- No application tables exist yet — the first account-scoped tables (accounts,
-- events, room state) land in Milestones 3–4. This migration only enables the
-- Postgres extensions those tables will rely on, so the migration runner has a
-- real, idempotent first revision.
--
-- pgcrypto provides gen_random_uuid() for the UUID account_id primary keys and
-- digest()/crypt() for hashing CLI tokens in Milestone 8.
CREATE EXTENSION IF NOT EXISTS pgcrypto;
