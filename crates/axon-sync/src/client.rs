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
