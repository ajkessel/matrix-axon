//! Per-account matrix-rust-sdk [`Client`] construction and authentication.
//!
//! Each account gets its own [`Client`] backed by a dedicated SQLite store
//! (under `sync.data_dir/<account_id>`) holding the SDK's state and crypto
//! material — Olm/Megolm sessions, account keys. That store is separate from
//! our Postgres archive and must survive restarts, or we lose historical
//! decryption keys.
//!
//! On first boot we log in with the configured credential and persist the
//! resulting access token (encrypted) plus device ID. On later boots we restore
//! the session from the stored token, so the password is only ever used once.

use std::path::Path;

use axon_core::{Credential, SyncConfig};
use axon_store::{Account, Store};
use matrix_sdk::{
    authentication::matrix::MatrixSession, ruma::OwnedUserId, store::RoomLoadSettings, Client,
    SessionMeta, SessionTokens,
};

use crate::error::{sdk_err, SyncError};

/// Build a [`Client`] for `account` and ensure it is authenticated, returning
/// the ready-to-sync client.
///
/// `credential` is the configured login credential for this account, if any. It
/// is only consulted on first boot (when no token is stored yet).
pub(crate) async fn connect_account(
    store: &Store,
    account: &Account,
    config: &SyncConfig,
    credential: Option<Credential<'_>>,
) -> Result<Client, SyncError> {
    let store_key = config
        .store_key
        .as_deref()
        .ok_or(SyncError::MissingStoreKey)?;

    let data_dir = config.data_dir.join(account.account_id.to_string());
    create_store_dir(&data_dir).await?;

    let client = Client::builder()
        .homeserver_url(&account.homeserver_url)
        .sqlite_store(&data_dir, Some(store_key))
        .build()
        .await
        .map_err(sdk_err)?;

    // Prefer restoring an existing session: the access token is authoritative
    // and lets us skip re-authenticating (and re-using the password).
    if let Some(token) = store.account_token(account.account_id, store_key).await? {
        restore(&client, account, token).await?;
        tracing::info!(account_id = %account.account_id, user_id = %account.user_id, "restored session");
        return Ok(client);
    }

    // First boot: authenticate with the configured credential.
    let credential = credential.ok_or_else(|| SyncError::NoCredential(account.user_id.clone()))?;
    match credential {
        Credential::Password(password) => {
            let response = client
                .matrix_auth()
                .login_username(&account.user_id, password)
                .initial_device_display_name("axon")
                .send()
                .await
                .map_err(sdk_err)?;
            store
                .set_account_session(
                    account.account_id,
                    response.device_id.as_str(),
                    &response.access_token,
                    store_key,
                )
                .await?;
            tracing::info!(account_id = %account.account_id, user_id = %account.user_id, device_id = %response.device_id, "logged in");
        }
        Credential::Token { token, device_id } => {
            let device_id =
                device_id.ok_or_else(|| SyncError::MissingDeviceId(account.user_id.clone()))?;
            restore(&client, account, token.to_owned()).await?;
            store
                .set_account_session(account.account_id, device_id, token, store_key)
                .await?;
            tracing::info!(account_id = %account.account_id, user_id = %account.user_id, "restored pre-provisioned token");
        }
    }

    Ok(client)
}

/// Log `account` in as a **fresh Matrix device** with a password, returning the
/// ready-to-sync client. Used by the runtime login verb (ADR 0022), not the boot
/// path — `connect_account` prefers a stored session, whereas this deliberately
/// starts clean.
///
/// The account's SDK store dir (`data_dir/<account_id>`) is replaced with a fresh
/// one: a reactivated `deactivated` row reuses its `account_id`, and its old
/// Olm/Megolm store would otherwise carry a dead device's keys into a new device
/// session. The old store is **only dropped once login succeeds** — until then it
/// is moved aside and restored on failure, so a rejected password or an
/// unreachable homeserver leaves the account exactly as it was (the durable
/// Postgres archive is never touched here regardless). The new session (device id
/// and access token) is persisted via [`Store::set_account_session`], so a later
/// restart restores it like any other. The password is consumed here, never stored.
///
/// `account.user_id` must be the full MXID (the login verb resolves identity
/// before minting the row), so it is used directly as the login username.
pub(crate) async fn login_new_device(
    store: &Store,
    account: &Account,
    config: &SyncConfig,
    password: &str,
) -> Result<Client, SyncError> {
    let store_key = config
        .store_key
        .as_deref()
        .ok_or(SyncError::MissingStoreKey)?;

    let data_dir = config.data_dir.join(account.account_id.to_string());
    let backup = config.data_dir.join(format!("{}.prev", account.account_id));

    with_staged_store_dir(&data_dir, &backup, || async {
        let client = Client::builder()
            .homeserver_url(&account.homeserver_url)
            .sqlite_store(&data_dir, Some(store_key))
            .build()
            .await
            .map_err(sdk_err)?;

        let response = client
            .matrix_auth()
            .login_username(&account.user_id, password)
            .initial_device_display_name("axon")
            .send()
            .await
            .map_err(login_err)?;
        store
            .set_account_session(
                account.account_id,
                response.device_id.as_str(),
                &response.access_token,
                store_key,
            )
            .await?;
        tracing::info!(
            account_id = %account.account_id,
            user_id = %account.user_id,
            device_id = %response.device_id,
            "logged in new device"
        );

        Ok(client)
    })
    .await
}

/// Classify a login failure: a homeserver `M_FORBIDDEN` / `M_UNAUTHORIZED` /
/// `M_USER_DEACTIVATED` means the credentials were rejected
/// ([`SyncError::AuthFailed`] → `401`); anything else (homeserver unreachable, a
/// 5xx, a parse failure) is a transient upstream error ([`SyncError::Sdk`]).
fn login_err(err: matrix_sdk::Error) -> SyncError {
    use matrix_sdk::ruma::api::error::ErrorKind;
    match err.client_api_error_kind() {
        Some(ErrorKind::Forbidden | ErrorKind::Unauthorized | ErrorKind::UserDeactivated) => {
            SyncError::AuthFailed(err.to_string())
        }
        _ => SyncError::Sdk(err.to_string()),
    }
}

/// Restore a Matrix session onto `client` from a stored or provided access token.
/// Requires the account row to carry the `device_id` the token belongs to.
async fn restore(
    client: &Client,
    account: &Account,
    access_token: String,
) -> Result<(), SyncError> {
    let device_id = account
        .device_id
        .clone()
        .ok_or_else(|| SyncError::MissingDeviceId(account.user_id.clone()))?;
    let user_id = OwnedUserId::try_from(account.user_id.as_str()).map_err(sdk_err)?;

    let session = MatrixSession {
        meta: SessionMeta {
            user_id,
            device_id: device_id.as_str().into(),
        },
        tokens: SessionTokens {
            access_token,
            refresh_token: None,
        },
    };

    client
        .matrix_auth()
        .restore_session(session, RoomLoadSettings::default())
        .await
        .map_err(sdk_err)
}

/// Create the SDK store directory (and parents) if it doesn't exist.
async fn create_store_dir(path: &Path) -> Result<(), SyncError> {
    tokio::fs::create_dir_all(path)
        .await
        .map_err(|e| SyncError::Sdk(format!("creating SDK store dir {}: {e}", path.display())))
}

/// `remove_dir_all` that treats an absent directory as success.
async fn remove_dir_if_present(path: &Path) -> Result<(), SyncError> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SyncError::Sdk(format!(
            "removing dir {}: {e}",
            path.display()
        ))),
    }
}

/// Remove an account's on-disk SDK store: both the live store dir
/// (`data_dir/<account_id>/`) and any staging backup (`data_dir/<account_id>.prev`)
/// left by [`with_staged_store_dir`]. Used by the account-delete teardown (it owns
/// the same `<account_id>` / `<account_id>.prev` naming as login's staging, so the
/// removal is colocated here). Idempotent — an absent dir is success — so a delete
/// retry or the boot reconcile can re-run it. The two paths mirror the
/// construction in [`connect_account`] and [`login_new_device`].
pub(crate) async fn remove_account_store_dirs(
    config: &SyncConfig,
    account_id: uuid::Uuid,
) -> Result<(), SyncError> {
    let data_dir = config.data_dir.join(account_id.to_string());
    let backup = config.data_dir.join(format!("{account_id}.prev"));
    remove_dir_if_present(&data_dir).await?;
    remove_dir_if_present(&backup).await?;
    Ok(())
}

/// Run a fresh-device login (`build`) against an empty store at `data_dir`,
/// preserving any existing store until the login is known to have succeeded.
///
/// Staging: move an existing `data_dir` aside to `backup`, create an empty
/// `data_dir`, run `build`. On success the old store (`backup`) is dropped — the
/// fresh device's store is authoritative. On failure the partial fresh store is
/// removed and the old one is moved back, so a rejected/failed login has no side
/// effect on the account.
///
/// **Crash recovery:** the old store is dropped *only* after a successful login,
/// so a `backup` left by an interrupted prior attempt is the only surviving copy
/// of the prior store. It is therefore treated as authoritative and restored (not
/// deleted) at the start: any concurrent `data_dir` (an uncommitted/partial fresh
/// store) is discarded and the backup moved back, before the normal stage begins.
/// Every step is idempotent, so repeated crashes converge rather than lose data.
async fn with_staged_store_dir<F, Fut, T>(
    data_dir: &Path,
    backup: &Path,
    build: F,
) -> Result<T, SyncError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, SyncError>>,
{
    // Recover from an interrupted prior attempt: a surviving `backup` outranks any
    // `data_dir` (which may be a half-built fresh store), so restore it first
    // rather than deleting it — otherwise an interrupted login could discard the
    // prior store even though no fresh login ever succeeded.
    let backup_present = tokio::fs::try_exists(backup)
        .await
        .map_err(|e| SyncError::Sdk(format!("checking {}: {e}", backup.display())))?;
    if backup_present {
        remove_dir_if_present(data_dir).await?;
        tokio::fs::rename(backup, data_dir).await.map_err(|e| {
            SyncError::Sdk(format!(
                "restoring staged SDK store {}: {e}",
                backup.display()
            ))
        })?;
    }

    // Move the current store aside (a no-op on the common first-login case).
    match tokio::fs::rename(data_dir, backup).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(SyncError::Sdk(format!(
                "staging SDK store dir {}: {e}",
                data_dir.display()
            )))
        }
    }
    create_store_dir(data_dir).await?;

    match build().await {
        Ok(value) => {
            // Login succeeded: the fresh store stands; drop the old one (best-effort
            // — a leftover backup is harmless and reclaimed on the next attempt).
            let _ = remove_dir_if_present(backup).await;
            Ok(value)
        }
        Err(err) => {
            // Roll back: discard the partial fresh store, restore the prior one.
            let _ = remove_dir_if_present(data_dir).await;
            let _ = tokio::fs::rename(backup, data_dir).await;
            Err(err)
        }
    }
}

/// Resolve the login credential for `account` from the configured provision,
/// matching on `(user_id, homeserver_url)`. Returns `None` if no provision
/// matches (the account must then have a stored session to authenticate).
pub(crate) fn credential_for<'c>(
    config: &'c SyncConfig,
    account: &Account,
) -> Result<Option<Credential<'c>>, SyncError> {
    let Some(provision) = config
        .account
        .as_ref()
        .filter(|p| matches_account(p, account))
    else {
        return Ok(None);
    };
    Ok(Some(provision.credential()?))
}

/// Whether a configured provision refers to the same account as a stored row.
pub(crate) fn matches_account(provision: &axon_core::AccountProvision, account: &Account) -> bool {
    provision.user_id == account.user_id && provision.homeserver_url == account.homeserver_url
}

#[cfg(test)]
mod tests {
    use super::with_staged_store_dir;
    use crate::error::SyncError;
    use std::path::PathBuf;

    /// A throwaway directory under the OS temp dir, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("axon-stage-test-{}", uuid::Uuid::new_v4()));
            TempRoot(p)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A failed login must leave the prior store untouched — Codex's concern: a
    /// rejected password should not destroy a reactivating account's old store.
    #[tokio::test]
    async fn failed_login_restores_prior_store() {
        let root = TempRoot::new();
        let data_dir = root.0.join("acct");
        let backup = root.0.join("acct.prev");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();
        tokio::fs::write(data_dir.join("crypto.sqlite"), b"old-keys")
            .await
            .unwrap();

        let res: Result<(), SyncError> = with_staged_store_dir(&data_dir, &backup, || async {
            // The fresh store may have been partially built before login failed.
            tokio::fs::write(data_dir.join("partial"), b"x")
                .await
                .unwrap();
            Err(SyncError::AuthFailed("bad password".into()))
        })
        .await;

        assert!(matches!(res, Err(SyncError::AuthFailed(_))));
        // Prior store restored verbatim; the partial fresh store and backup are gone.
        assert_eq!(
            tokio::fs::read(data_dir.join("crypto.sqlite"))
                .await
                .unwrap(),
            b"old-keys"
        );
        assert!(!tokio::fs::try_exists(data_dir.join("partial"))
            .await
            .unwrap());
        assert!(!tokio::fs::try_exists(&backup).await.unwrap());
    }

    /// A successful login keeps the fresh store and drops the old one.
    #[tokio::test]
    async fn successful_login_drops_old_store() {
        let root = TempRoot::new();
        let data_dir = root.0.join("acct");
        let backup = root.0.join("acct.prev");
        tokio::fs::create_dir_all(&data_dir).await.unwrap();
        tokio::fs::write(data_dir.join("old-keys"), b"dead-device")
            .await
            .unwrap();

        let res: Result<(), SyncError> = with_staged_store_dir(&data_dir, &backup, || async {
            tokio::fs::write(data_dir.join("new-keys"), b"fresh")
                .await
                .unwrap();
            Ok(())
        })
        .await;

        assert!(res.is_ok());
        // Fresh store kept; the dead device's store and the backup are gone.
        assert!(tokio::fs::try_exists(data_dir.join("new-keys"))
            .await
            .unwrap());
        assert!(!tokio::fs::try_exists(data_dir.join("old-keys"))
            .await
            .unwrap());
        assert!(!tokio::fs::try_exists(&backup).await.unwrap());
    }

    /// A `backup` left by a crash mid-stage (store moved aside, never restored or
    /// committed) is recovered, not deleted — even when the recovering attempt
    /// itself fails. Codex P2: an interrupted login must not discard the prior
    /// store's only surviving copy.
    #[tokio::test]
    async fn recovers_orphaned_backup_from_interrupted_stage() {
        let root = TempRoot::new();
        let data_dir = root.0.join("acct");
        let backup = root.0.join("acct.prev");
        // Simulate the crash window: backup holds the real store, data_dir is gone.
        tokio::fs::create_dir_all(&backup).await.unwrap();
        tokio::fs::write(backup.join("crypto.sqlite"), b"old-keys")
            .await
            .unwrap();

        // Even a *failing* recovering attempt must end with the store intact.
        let res: Result<(), SyncError> = with_staged_store_dir(&data_dir, &backup, || async {
            Err(SyncError::AuthFailed("bad password".into()))
        })
        .await;

        assert!(matches!(res, Err(SyncError::AuthFailed(_))));
        assert_eq!(
            tokio::fs::read(data_dir.join("crypto.sqlite"))
                .await
                .unwrap(),
            b"old-keys"
        );
        assert!(!tokio::fs::try_exists(&backup).await.unwrap());
    }

    /// First-ever login (no existing store) works and leaves no backup behind.
    #[tokio::test]
    async fn first_login_with_no_prior_store() {
        let root = TempRoot::new();
        let data_dir = root.0.join("acct");
        let backup = root.0.join("acct.prev");

        let res: Result<(), SyncError> = with_staged_store_dir(&data_dir, &backup, || async {
            tokio::fs::write(data_dir.join("keys"), b"fresh")
                .await
                .unwrap();
            Ok(())
        })
        .await;

        assert!(res.is_ok());
        assert!(tokio::fs::try_exists(data_dir.join("keys")).await.unwrap());
        assert!(!tokio::fs::try_exists(&backup).await.unwrap());
    }
}
