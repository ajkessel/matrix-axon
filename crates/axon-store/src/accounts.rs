//! Account rows: one per Matrix account this Axon process syncs.
//!
//! "One human per Axon process, N Matrix accounts inside" — every
//! account-scoped table references [`Account::account_id`]. The access token is
//! encrypted at rest with pgcrypto's `pgp_sym_encrypt` (ADR 0008); the
//! symmetric key lives in config (`sync.store_key`) and is passed in per call,
//! never stored in the database.
//!
//! Queries use sqlx's runtime `query`/`query_as` API rather than the
//! compile-time macros (the macros require the `sqlx` umbrella we dropped — see
//! `migrations.rs` — and a database at build time). `FromRow` is implemented by
//! hand for the same reason.

use chrono::{DateTime, Utc};
use sqlx_core::row::Row;
use sqlx_postgres::{PgRow, Postgres};
use uuid::Uuid;

use crate::{Store, StoreError};

/// A Matrix account row. The encrypted access token is deliberately absent —
/// it is only ever read back through [`Store::account_token`], which decrypts
/// in SQL, so the plaintext never lingers on this struct.
#[derive(Debug, Clone)]
pub struct Account {
    /// Stable primary key, referenced by every account-scoped table.
    pub account_id: Uuid,
    /// Full Matrix user ID, e.g. `@alice:example.org`.
    pub user_id: String,
    /// Homeserver base URL.
    pub homeserver_url: String,
    /// Device ID assigned at login (or supplied with a pre-provisioned token).
    pub device_id: Option<String>,
    /// Reserved sync-position cursor; the SyncService manages its own position
    /// in its SQLite store, so this currently stays `NULL`.
    pub sync_token: Option<String>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last update time.
    pub updated_at: DateTime<Utc>,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for Account {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        Ok(Account {
            account_id: row.try_get("account_id")?,
            user_id: row.try_get("user_id")?,
            homeserver_url: row.try_get("homeserver_url")?,
            device_id: row.try_get("device_id")?,
            sync_token: row.try_get("sync_token")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Columns selected for an [`Account`] (no encrypted token).
const ACCOUNT_COLUMNS: &str = "account_id, user_id, homeserver_url, device_id, \
    sync_token, created_at, updated_at";

impl Store {
    /// Insert the account for `(user_id, homeserver_url)`, or return the
    /// existing row if it is already provisioned. Idempotent, so it is safe to
    /// call on every boot.
    pub async fn upsert_account(
        &self,
        user_id: &str,
        homeserver_url: &str,
    ) -> Result<Account, StoreError> {
        let sql = format!(
            "INSERT INTO accounts (user_id, homeserver_url) VALUES ($1, $2) \
             ON CONFLICT (user_id, homeserver_url) \
             DO UPDATE SET updated_at = now() \
             RETURNING {ACCOUNT_COLUMNS}"
        );
        let account = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .bind(user_id)
            .bind(homeserver_url)
            .fetch_one(&self.pool)
            .await?;
        Ok(account)
    }

    /// All provisioned accounts, oldest first. The sync engine iterates these to
    /// spawn one task per account.
    pub async fn list_accounts(&self) -> Result<Vec<Account>, StoreError> {
        let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts ORDER BY created_at ASC");
        let accounts = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(accounts)
    }

    /// Persist a login session: encrypt the access token with `key` (pgcrypto
    /// `pgp_sym_encrypt`) and store it alongside the device ID. The plaintext
    /// token is bound as a parameter and never logged.
    pub async fn set_account_session(
        &self,
        account_id: Uuid,
        device_id: &str,
        access_token: &str,
        key: &str,
    ) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "UPDATE accounts \
             SET device_id = $2, \
                 access_token_encrypted = pgp_sym_encrypt($3, $4) \
             WHERE account_id = $1",
        )
        .bind(account_id)
        .bind(device_id)
        .bind(access_token)
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Decrypt and return the stored access token, or `None` if the account has
    /// no session yet (first boot before login). Decryption happens in SQL via
    /// `pgp_sym_decrypt` so the ciphertext never reaches application memory.
    pub async fn account_token(
        &self,
        account_id: Uuid,
        key: &str,
    ) -> Result<Option<String>, StoreError> {
        let row = sqlx_core::query::query(
            "SELECT pgp_sym_decrypt(access_token_encrypted, $2) AS token \
             FROM accounts WHERE account_id = $1",
        )
        .bind(account_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(row.try_get::<Option<String>, _>("token")?),
            None => Ok(None),
        }
    }

    /// Update the reserved sync-position cursor. Currently unused (the
    /// SyncService owns its position) but kept for a future sync model that
    /// manages its own cursor.
    pub async fn update_sync_token(
        &self,
        account_id: Uuid,
        sync_token: &str,
    ) -> Result<(), StoreError> {
        sqlx_core::query::query("UPDATE accounts SET sync_token = $2 WHERE account_id = $1")
            .bind(account_id)
            .bind(sync_token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
