-- M4a: sibling tables holding the cryptographic provenance of encrypted events,
-- keyed 1:1 by (account_id, event_id) so a decrypted `events` row stays
-- re-verifiable against Matrix's signatures. See ADR 0015.
--
-- Each references events(account_id, event_id) (its UNIQUE constraint) and
-- cascades on delete, so they live and die with their event row.
--
-- Population (ADR 0015 "EncryptionInfo finding"):
--   * event_ciphertext is written only on the UTD path — the SDK surfaces the
--     original `m.room.encrypted` envelope to our handler only when it could not
--     decrypt. For events the SDK decrypts before dispatch the ciphertext is not
--     available, so those have no ciphertext row (the megolm/device siblings,
--     which come from EncryptionInfo, are still written).
--   * event_megolm_session and event_sender_device_keys are written for every
--     successfully-decrypted event, from the SDK's EncryptionInfo — on the live
--     dispatch path and again on the re-decryption path.

-- The original ciphertext envelope, preserved independently of `events.raw_event`.
CREATE TABLE event_ciphertext (
    account_id UUID        NOT NULL,
    event_id   TEXT        NOT NULL,
    room_id    TEXT        NOT NULL,
    algorithm  TEXT        NOT NULL,                 -- e.g. m.megolm.v1.aes-sha2
    sender_key TEXT,                                 -- curve25519 key on the envelope
    session_id TEXT,                                 -- megolm session id
    ciphertext JSONB       NOT NULL,                 -- the full m.room.encrypted content
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, event_id),
    FOREIGN KEY (account_id, event_id)
        REFERENCES events (account_id, event_id) ON DELETE CASCADE
);

-- Megolm session metadata for the session that decrypted the event, from
-- EncryptionInfo::algorithm_info (MegolmV1AesSha2).
CREATE TABLE event_megolm_session (
    account_id                 UUID NOT NULL,
    event_id                   TEXT NOT NULL,
    session_id                 TEXT,
    sender_curve25519_key      TEXT,                 -- creator of the megolm key
    sender_claimed_ed25519_key TEXT,                 -- claimed signing key
    forwarded                  BOOLEAN NOT NULL DEFAULT FALSE,
    forwarder_user_id          TEXT,
    forwarder_device_id        TEXT,
    PRIMARY KEY (account_id, event_id),
    FOREIGN KEY (account_id, event_id)
        REFERENCES events (account_id, event_id) ON DELETE CASCADE
);

-- The sending device's identity keys + verification state at decrypt time, from
-- EncryptionInfo. "at decrypt time" — verification_state can change later as
-- devices are verified/deleted; this is a snapshot, not a live view.
CREATE TABLE event_sender_device_keys (
    account_id         UUID NOT NULL,
    event_id           TEXT NOT NULL,
    device_id          TEXT,
    curve25519_key     TEXT,
    ed25519_key        TEXT,
    verification_state TEXT NOT NULL,                -- 'verified' | 'unverified'
    PRIMARY KEY (account_id, event_id),
    FOREIGN KEY (account_id, event_id)
        REFERENCES events (account_id, event_id) ON DELETE CASCADE
);
