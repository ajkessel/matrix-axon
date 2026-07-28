# axon-crypto

Thin verification surface over matrix-rust-sdk cryptography.

## Responsibility

Exposes opt-in event verification — re-verifying decrypted content against stored ciphertext, megolm session metadata, and sender device keys. Does not reimplement olm/megolm; delegates to matrix-rust-sdk.

## Owns vs. consumes

- **Owns:** nothing external.
- **Consumes:** `axon-core` types; matrix-rust-sdk crypto types.

## Status

Stub — no public API yet.
