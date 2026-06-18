//! Client→axon bearer tokens: the local-API gate (M7b).
//!
//! A token authenticates a *client* to this Axon instance (distinct from the
//! Matrix access token in [`accounts`](crate::accounts), which authenticates
//! axon to a homeserver). Tokens are global to the instance, not account-scoped:
//! one human owns all their accounts.
//!
//! Only the **hash** of a token is stored — the raw secret is shown once at mint
//! and never recoverable. A token is high-entropy random, so a single SHA-256 is
//! the right primitive (the model used by e.g. GitHub personal access tokens),
//! rather than the recoverable `pgp_sym_encrypt` used for the access token or a
//! slow password KDF. Verification is one indexed equality lookup on the hash.
//!
//! The verification path lives behind [`Store::verify_token`] so a future OAuth
//! 2.0 issuer can replace the mint path (and this table) without changing the
//! on-the-wire `Authorization: Bearer` contract or any consumer code.
//!
//! As elsewhere in this crate the queries use sqlx's runtime API and `FromRow`
//! is hand-implemented (see `migrations.rs` for why the macros are unavailable).

use base64::Engine;
use chrono::{DateTime, Utc};
use sqlx_core::row::Row;
use sqlx_postgres::{PgRow, Postgres};
use uuid::Uuid;

use crate::{Store, StoreError};

/// A token row as surfaced to the management CLI. The secret `hash` is
/// deliberately absent — listing tokens never exposes anything secret-bearing.
#[derive(Debug, Clone)]
pub struct Token {
    /// Stable id, used to revoke the token.
    pub id: Uuid,
    /// Human-supplied label, e.g. the device or purpose the token is for.
    pub label: String,
    /// When the token was minted.
    pub created_at: DateTime<Utc>,
    /// When the token was last accepted on a request, or `None` if never used.
    pub last_used_at: Option<DateTime<Utc>>,
    /// When the token was revoked, or `None` if still active.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Token {
    /// Whether the token has been revoked (and so fails verification).
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for Token {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Token {
            id: row.try_get("id")?,
            label: row.try_get("label")?,
            created_at: row.try_get("created_at")?,
            last_used_at: row.try_get("last_used_at")?,
            revoked_at: row.try_get("revoked_at")?,
        })
    }
}

/// A freshly minted token: the row id plus the **raw** secret, returned exactly
/// once from [`Store::issue_token`] so the CLI can print it. The raw value is
/// never stored or recoverable afterward.
#[derive(Debug, Clone)]
pub struct IssuedToken {
    /// The stored token's id.
    pub id: Uuid,
    /// Its label.
    pub label: String,
    /// The raw bearer token. Show once; only its hash is persisted.
    pub token: String,
}

/// Columns selected for a [`Token`] (never the `hash`).
const TOKEN_COLUMNS: &str = "id, label, created_at, last_used_at, revoked_at";

impl Store {
    /// Mint a new bearer token with the given label: generate a random secret,
    /// store its hash, and return the raw secret once (it is never recoverable
    /// afterward). This is the CLI bootstrap path (`axon token issue`).
    pub async fn issue_token(&self, label: &str) -> Result<IssuedToken, StoreError> {
        let token = generate_token();
        let hash = hash_token(&token);
        let row = sqlx_core::query::query(
            "INSERT INTO tokens (label, hash) VALUES ($1, $2) RETURNING id",
        )
        .bind(label)
        .bind(&hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(IssuedToken {
            id: row.try_get("id")?,
            label: label.to_owned(),
            token,
        })
    }

    /// Verify a presented bearer token. Hashes it, looks up an **unrevoked** row,
    /// and (on a match) stamps `last_used_at`. Returns the token's id on success
    /// or `None` if the token is unknown or revoked. The match and the
    /// `last_used_at` touch happen in one statement so verification is a single
    /// round-trip on the hot path (every `/v1/` request goes through here).
    pub async fn verify_token(&self, raw: &str) -> Result<Option<Uuid>, StoreError> {
        let hash = hash_token(raw);
        let row = sqlx_core::query::query(
            "UPDATE tokens SET last_used_at = now() \
             WHERE hash = $1 AND revoked_at IS NULL \
             RETURNING id",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(row) => Ok(Some(row.try_get("id")?)),
            None => Ok(None),
        }
    }

    /// All tokens, newest first — the management read (`axon token list`).
    /// Includes revoked tokens so the audit trail is visible.
    pub async fn list_tokens(&self) -> Result<Vec<Token>, StoreError> {
        let sql = format!("SELECT {TOKEN_COLUMNS} FROM tokens ORDER BY created_at DESC");
        let tokens = sqlx_core::query_as::query_as::<Postgres, Token>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(tokens)
    }

    /// Revoke a token by id (stamp `revoked_at`). Returns `true` if a still-active
    /// token was revoked, `false` if no such id exists or it was already revoked —
    /// so the CLI can report the difference. Idempotent: the first revocation's
    /// timestamp is preserved (the `revoked_at IS NULL` guard makes a re-revoke a
    /// no-op).
    pub async fn revoke_token(&self, id: Uuid) -> Result<bool, StoreError> {
        let result = sqlx_core::query::query(
            "UPDATE tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

/// Generate a fresh bearer token: an `axon_` prefix (so it is recognizable and
/// greppable, like a GitHub `ghp_…` token) over 256 bits of CSPRNG entropy,
/// base64url-encoded without padding.
fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "axon_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// Hash a raw token for storage / lookup: SHA-256, base64-encoded. A plain hash
/// (not a password KDF) is correct here because the input is high-entropy —
/// there is nothing to brute-force — and it must be cheap to run on every request.
fn hash_token(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(raw.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_prefixed_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert!(a.starts_with("axon_"));
        assert!(b.starts_with("axon_"));
        assert_ne!(a, b, "two mints must not collide");
    }

    #[test]
    fn hash_is_stable_and_distinguishes_inputs() {
        assert_eq!(hash_token("axon_abc"), hash_token("axon_abc"));
        assert_ne!(hash_token("axon_abc"), hash_token("axon_abd"));
        // The hash is not the raw token.
        assert_ne!(hash_token("axon_abc"), "axon_abc");
    }
}
