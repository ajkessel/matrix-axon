//! Bound OAuth/OIDC identities (M14b, ADR 0054).
//!
//! What `axon oauth bind` (M14c) populates: proof that a specific upstream
//! provider subject (`sub`) belongs to this axon instance's one human owner.
//! Path A/B's token-minting handlers only ever *read* this table (matching a
//! verified id_token's `sub` against it); the write path (bind) is a
//! separate, deliberately out-of-band CLI verb, not something an
//! unauthenticated `/v1/oauth/*` request can trigger on its own.

use chrono::{DateTime, Utc};
use sqlx_core::row::Row;
use sqlx_postgres::{PgRow, Postgres};
use uuid::Uuid;

use crate::{Store, StoreError};

/// One bound identity: an upstream provider subject linked to this axon
/// instance's owner.
#[derive(Debug, Clone)]
pub struct OauthIdentity {
    pub id: Uuid,
    pub provider: String,
    pub subject: String,
    pub email: Option<String>,
    pub linked_at: DateTime<Utc>,
}

const OAUTH_IDENTITY_COLUMNS: &str = "id, provider, subject, email, linked_at";

impl sqlx_core::from_row::FromRow<'_, PgRow> for OauthIdentity {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(OauthIdentity {
            id: row.try_get("id")?,
            provider: row.try_get("provider")?,
            subject: row.try_get("subject")?,
            email: row.try_get("email")?,
            linked_at: row.try_get("linked_at")?,
        })
    }
}

impl Store {
    /// Bind a `(provider, subject)` identity, or update its stored `email` if
    /// already bound (an upstream email change should not require re-binding).
    /// The bind CLI (M14c) is the only caller; Path A/B's login handlers never
    /// call this — they only read via [`find_identity`](Self::find_identity).
    pub async fn bind_identity(
        &self,
        provider: &str,
        subject: &str,
        email: Option<&str>,
    ) -> Result<OauthIdentity, StoreError> {
        let sql = format!(
            "INSERT INTO oauth_identities (provider, subject, email) VALUES ($1, $2, $3) \
             ON CONFLICT (provider, subject) DO UPDATE SET email = EXCLUDED.email \
             RETURNING {OAUTH_IDENTITY_COLUMNS}"
        );
        let identity = sqlx_core::query_as::query_as::<Postgres, OauthIdentity>(&sql)
            .bind(provider)
            .bind(subject)
            .bind(email)
            .fetch_one(&self.pool)
            .await?;
        Ok(identity)
    }

    /// Look up a bound identity by its upstream `(provider, subject)` — the
    /// check every Path A/B login performs after verifying an id_token.
    pub async fn find_identity(
        &self,
        provider: &str,
        subject: &str,
    ) -> Result<Option<OauthIdentity>, StoreError> {
        let sql =
            format!("SELECT {OAUTH_IDENTITY_COLUMNS} FROM oauth_identities WHERE provider = $1 AND subject = $2");
        let identity = sqlx_core::query_as::query_as::<Postgres, OauthIdentity>(&sql)
            .bind(provider)
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?;
        Ok(identity)
    }

    /// Look up a bound identity by its row id — used when finishing a
    /// refresh-token redemption, which only carries the identity's id (not
    /// its provider/subject).
    pub async fn find_identity_by_id(&self, id: Uuid) -> Result<Option<OauthIdentity>, StoreError> {
        let sql = format!("SELECT {OAUTH_IDENTITY_COLUMNS} FROM oauth_identities WHERE id = $1");
        let identity = sqlx_core::query_as::query_as::<Postgres, OauthIdentity>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(identity)
    }

    /// All bound identities, newest first — the management read
    /// (`axon oauth identities list`, M14c).
    pub async fn list_identities(&self) -> Result<Vec<OauthIdentity>, StoreError> {
        let sql = format!(
            "SELECT {OAUTH_IDENTITY_COLUMNS} FROM oauth_identities ORDER BY linked_at DESC"
        );
        let identities = sqlx_core::query_as::query_as::<Postgres, OauthIdentity>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(identities)
    }

    /// Unbind an identity by id. Returns `true` if a row was deleted.
    /// Callers (the `identities unbind` CLI verb, M14c) are responsible for
    /// revoking any tokens/refresh-tokens tied to this identity *first* — the
    /// `tokens`/`oauth_refresh_tokens` foreign keys have no `ON DELETE`
    /// action, so a still-referenced identity fails to delete rather than
    /// silently orphaning live credentials.
    pub async fn delete_identity(&self, id: Uuid) -> Result<bool, StoreError> {
        let result = sqlx_core::query::query("DELETE FROM oauth_identities WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
