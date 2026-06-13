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

/// An account's lifecycle state, kept orthogonal to verification (ADR 0022).
///
/// The sync engine and the mutations gateway connect and serve **only**
/// `Active` accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    /// The normal state: the account syncs and can send.
    Active,
    /// A reversible pause that retains all of axon's data, reached via logout or
    /// an internal token failure. Does not sync or send; a fresh login
    /// reactivates the same row.
    Deactivated,
    /// A transient teardown breadcrumb set while a delete is in flight. A
    /// boot-time reconcile drives any row left here to completion; it is never a
    /// resting state a client observes long-term.
    Deleting,
}

impl AccountState {
    /// The on-disk / on-the-wire string form (matches the migration's `CHECK`).
    pub fn as_str(self) -> &'static str {
        match self {
            AccountState::Active => "active",
            AccountState::Deactivated => "deactivated",
            AccountState::Deleting => "deleting",
        }
    }

    /// Parse the stored string back into a state, failing as a column-decode
    /// error so an unexpected value surfaces as a read error rather than a
    /// silent default.
    fn from_db(s: &str) -> Result<Self, sqlx_core::Error> {
        match s {
            "active" => Ok(AccountState::Active),
            "deactivated" => Ok(AccountState::Deactivated),
            "deleting" => Ok(AccountState::Deleting),
            other => Err(sqlx_core::Error::ColumnDecode {
                index: "state".to_owned(),
                source: format!("unknown account state {other:?}").into(),
            }),
        }
    }
}

impl std::fmt::Display for AccountState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// Lifecycle state (ADR 0022). The sync engine connects only [`AccountState::Active`] rows.
    pub state: AccountState,
    /// Whether axon's own device is currently cross-signed (orthogonal to
    /// [`state`](Self::state)). Derived from the SDK's cross-signing state; the
    /// derivation is wired up in a later subphase, so this is `false` until then.
    pub verified: bool,
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
        let state: String = row.try_get("state")?;
        Ok(Account {
            account_id: row.try_get("account_id")?,
            user_id: row.try_get("user_id")?,
            homeserver_url: row.try_get("homeserver_url")?,
            device_id: row.try_get("device_id")?,
            state: AccountState::from_db(&state)?,
            verified: row.try_get("verified")?,
            sync_token: row.try_get("sync_token")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

/// Columns selected for an [`Account`] (no encrypted token).
const ACCOUNT_COLUMNS: &str = "account_id, user_id, homeserver_url, device_id, \
    state, verified, sync_token, created_at, updated_at";

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

    /// Fetch a single account by id, or `None` if no such row exists. Used by
    /// the sync engine's client manager to cold-connect an account from just its
    /// id (e.g. when an API send arrives before sync has brought it online).
    pub async fn get_account(&self, account_id: Uuid) -> Result<Option<Account>, StoreError> {
        let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE account_id = $1");
        let account = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(account)
    }

    /// The `active` accounts, oldest first — the safe default for the connect
    /// path. The sync engine iterates these to spawn one task per account, so a
    /// `deactivated` or `deleting` row is never brought online (ADR 0022).
    ///
    /// "List accounts" deliberately means "the accounts you act on", not "every
    /// row": surfacing `deactivated`/`deleting` rows (e.g. the read API showing a
    /// logged-out account, or the teardown reconcile finding `deleting` rows) is
    /// a separate, explicitly-named query added when such a caller lands — so the
    /// default can never silently include a stale row.
    pub async fn list_accounts(&self) -> Result<Vec<Account>, StoreError> {
        let sql = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE state = 'active' ORDER BY created_at ASC"
        );
        let accounts = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(accounts)
    }

    /// The client-visible accounts — `active` **and** `deactivated`, oldest
    /// first — backing the lifecycle read API (`GET /v1/accounts`). It includes
    /// logged-out (`deactivated`) accounts so a client that has lost the
    /// `account_id` can still discover one to offer re-login, but excludes the
    /// transient `deleting` teardown state (a row mid-removal is not something to
    /// act on; a by-id [`get_account`](Self::get_account) read still surfaces it).
    ///
    /// Deliberately distinct from [`list_accounts`](Self::list_accounts), which is
    /// active-only for the connect/boot path — the two callers want different
    /// slices, so neither rides on a shared "all rows" accessor (ADR 0022).
    pub async fn list_client_visible_accounts(&self) -> Result<Vec<Account>, StoreError> {
        let sql = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts \
             WHERE state IN ('active', 'deactivated') ORDER BY created_at ASC"
        );
        let accounts = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(accounts)
    }

    /// The accounts mid-teardown (`deleting`), oldest first — the explicitly-named
    /// accessor the boot reconcile uses to re-find rows a crash left in flight
    /// (ADR 0022 / 0024). Distinct from the active-only [`list_accounts`](Self::list_accounts)
    /// and the client-facing [`list_client_visible_accounts`](Self::list_client_visible_accounts),
    /// so a stale `deleting` row can never leak onto the connect or read paths.
    pub async fn list_deleting_accounts(&self) -> Result<Vec<Account>, StoreError> {
        let sql = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts WHERE state = 'deleting' ORDER BY created_at ASC"
        );
        let accounts = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .fetch_all(&self.pool)
            .await?;
        Ok(accounts)
    }

    /// Every account id, in **any** lifecycle state — the set the orphan-store-dir
    /// GC checks each `data_dir/<id>/` against (ADR 0024). Keyed off row existence,
    /// not state: a `deactivated` row is real and its dir must be kept, so GC prunes
    /// only dirs whose id matches *no* row here (the #24-safe distinction).
    pub async fn list_all_account_ids(&self) -> Result<Vec<Uuid>, StoreError> {
        let ids = sqlx_core::query::query("SELECT account_id FROM accounts")
            .try_map(|row: PgRow| row.try_get::<Uuid, _>("account_id"))
            .fetch_all(&self.pool)
            .await?;
        Ok(ids)
    }

    /// Look up an account by its Matrix identity `(user_id, homeserver_url)` —
    /// the natural key the login verb starts from, before any `account_id`
    /// exists. Returns the row in **any** lifecycle state (so a caller can
    /// distinguish "new identity" from a `deactivated` row to reactivate or an
    /// `active` one to reject); `None` means no such account. Read-only — unlike
    /// [`upsert_account`](Self::upsert_account) it never inserts.
    pub async fn find_account_by_identity(
        &self,
        user_id: &str,
        homeserver_url: &str,
    ) -> Result<Option<Account>, StoreError> {
        let sql = format!(
            "SELECT {ACCOUNT_COLUMNS} FROM accounts \
             WHERE user_id = $1 AND homeserver_url = $2"
        );
        let account = sqlx_core::query_as::query_as::<Postgres, Account>(&sql)
            .bind(user_id)
            .bind(homeserver_url)
            .fetch_optional(&self.pool)
            .await?;
        Ok(account)
    }

    /// Move an account to a new lifecycle state (ADR 0022). The lifecycle verbs
    /// (login/logout/delete) own these transitions; clients never set `state`
    /// directly. The `updated_at` trigger maintains the timestamp.
    pub async fn set_account_state(
        &self,
        account_id: Uuid,
        state: AccountState,
    ) -> Result<(), StoreError> {
        sqlx_core::query::query("UPDATE accounts SET state = $2 WHERE account_id = $1")
            .bind(account_id)
            .bind(state.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Hard-delete the account row. The FK cascades (`ON DELETE CASCADE` on
    /// `events`, `room_state`, `account_data`, and the event crypto siblings)
    /// remove all of its Postgres-resident data in the same statement. Idempotent:
    /// deleting an already-gone row affects zero rows and is not an error.
    ///
    /// This is the **last** step of the account-delete teardown (in `axon-sync`) —
    /// the row is the durable key a boot reconcile uses to re-find external
    /// resources, so the external cleanup (SDK store dir, and later the search
    /// index / media cache) runs first and the row is dropped only once it has
    /// (ADR 0024).
    pub async fn delete_account_row(&self, account_id: Uuid) -> Result<(), StoreError> {
        sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        Ok(())
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
