-- M7c: the sender-device trust verdict snapshot, recorded at decrypt time.
--
-- `event_sender_device_keys.verification_state` is a coarse 'verified'|'unverified'
-- flag. M7c adds a richer four-valued verdict derived from the SDK's
-- VerificationState/VerificationLevel (see crate axon-sync `meta::sender_trust`):
--   * verified              — sender device cross-signed by the sender's master key
--   * unverified            — sender identity/device not verified (unsigned, etc.)
--   * unknown               — couldn't link the message back to a device
--   * verification_violation — sender identity was already in conflict at receipt
--
-- It is a SNAPSHOT at decrypt time (like verification_state) — distinct from the
-- sender's *current* trust, which the verification-bundle endpoint reads live.
-- Nullable: existing rows (decrypted before this milestone) carry no verdict and
-- need no backfill; new/re-decrypted rows populate it via upsert.
ALTER TABLE event_sender_device_keys
    ADD COLUMN sender_trust TEXT
        CHECK (sender_trust IN ('verified', 'unverified', 'unknown', 'verification_violation'));
