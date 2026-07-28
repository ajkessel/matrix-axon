# ADR 0008 — Access tokens encrypted at rest via pgcrypto

## Context

Each provisioned account yields a Matrix access token that Axon must persist to
restore the session on restart (ADR 0007). A Matrix access token is a bearer
credential: anyone holding it can act fully as that user against the homeserver.
Storing it as plaintext in Postgres means a database dump — a backup leak, a
read-replica compromise, a `SELECT` by an over-privileged operator — hands over
the account.

## Decision

Store the token encrypted, in a `BYTEA` column `accounts.access_token_encrypted`,
using pgcrypto symmetric encryption:

- Write: `pgp_sym_encrypt($token, $key)` (token + key bound as parameters).
- Read: `pgp_sym_decrypt(access_token_encrypted, $key)`.

The key is `sync.store_key` from config/env; it is **never** stored in the
database. Decryption happens inside SQL, so the plaintext token exists in
application memory only transiently while building the SDK session, and the
ciphertext is what lives at rest. `pgcrypto` is already enabled by the baseline
migration. The same `store_key` passphrases the SDK's SQLite store, so one secret
governs all of an account's at-rest credentials.

## Consequences

**Pros**
- A database dump alone does not yield usable tokens; the attacker also needs
  `store_key`, which lives outside the database (env/secrets manager).
- No application-side crypto code to get wrong — pgcrypto does the work, and the
  plaintext never sits in a column.

**Cons / risks**
- **Key management is now the crux.** If `store_key` is lost, every stored token
  is unrecoverable (re-login required). If `store_key` leaks alongside a DB dump,
  the protection is void. The key must be managed as a real secret.
- **Symmetric, single key, no rotation in M3.** Rotating `store_key` would require
  re-encrypting every row; not implemented yet (see "When to revisit").
- **In-process exposure remains.** This protects data at rest, not a compromised
  running process — the token is plaintext in memory while in use, by necessity.
- This protects only the *token*. Message content at rest is a separate concern
  handled at the disk/filesystem layer (operator's encryption), not here.

**When to revisit**
- Key rotation: add a key-id column and re-encrypt on rotation.
- Moving decryption out of SQL into the app (e.g. envelope encryption with a KMS)
  if we want the database to never see the key material at all.
- MSC2918 refresh tokens (ADR 0010) change what we store and when.

## Alternatives considered

- **Plaintext column.** Rejected: a DB leak is a full account compromise.
- **Application-layer AES-GCM in Rust.** Equivalent protection but more code to
  own and audit; pgcrypto keeps the secret out of the column with no bespoke
  crypto. Revisit if we want the DB to never receive the key (KMS envelope).
- **Don't persist the token; re-login every boot.** Rejected: not all credentials
  allow scripted re-login, and it defeats restore-based startup.
