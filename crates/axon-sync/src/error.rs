//! Sync-engine errors.

use thiserror::Error;

/// Errors raised by the sync engine.
#[derive(Debug, Error)]
pub enum SyncError {
    /// An error surfaced by matrix-rust-sdk (login, restore, client build, or
    /// the running sync service). The SDK's own error type is large and not
    /// `'static`-friendly to carry around, so we keep its string form.
    #[error("matrix sdk error: {0}")]
    Sdk(String),

    /// A storage-layer error while provisioning accounts or persisting tokens.
    #[error("store error: {0}")]
    Store(#[from] axon_store::StoreError),

    /// A configuration value was invalid (e.g. an account with neither or both
    /// of `password` / `access_token`).
    #[error("configuration error: {0}")]
    Config(#[from] axon_core::ConfigError),

    /// An account is configured but `sync.store_key` is absent, so there is no
    /// key to encrypt the access token at rest or to passphrase the SDK store.
    #[error("sync.store_key is required when an account is configured")]
    MissingStoreKey,

    /// A pre-provisioned `access_token` was supplied without the `device_id` it
    /// belongs to, so the session can't be restored.
    #[error("account {0}: a device_id is required to restore a pre-provisioned access token")]
    MissingDeviceId(String),

    /// Neither a stored session nor a login credential is available for an
    /// account, so it cannot authenticate.
    #[error("account {0}: no stored session and no login credential configured")]
    NoCredential(String),
}

impl From<SyncError> for axon_core::Error {
    fn from(err: SyncError) -> Self {
        axon_core::Error::Sync(err.to_string())
    }
}

/// Shorthand for turning an SDK error into [`SyncError::Sdk`].
pub(crate) fn sdk_err(err: impl std::fmt::Display) -> SyncError {
    SyncError::Sdk(err.to_string())
}
