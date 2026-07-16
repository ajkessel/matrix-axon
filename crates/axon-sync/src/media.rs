//! Authenticated media download via the matrix-rust-sdk client.
//!
//! [`SdkMediaProxy`] is the [`MediaFetcher`] the bounded on-disk media cache
//! (`axon-media`) calls on a cache miss. It resolves the account's live SDK
//! client through the [`ClientManager`] (connecting if needed), then uses the
//! SDK's built-in media API — which carries the account's Bearer token
//! automatically — to download (and, for encrypted attachments, decrypt) MXC
//! content, returning the plaintext bytes.
//!
//! `axon-server` composes this fetcher behind the `axon-media` cache and adapts
//! the pair onto the `axon-api` `MediaProxy` port, so neither `axon-api` nor
//! `axon-media` depends on `matrix-sdk`.

use std::time::Duration;

use async_trait::async_trait;
use axon_core::media::{ThumbnailMethod, ThumbnailSpec};
use axon_media::{FetchError, MediaFetcher};
use matrix_sdk::media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings};
use matrix_sdk::ruma::{
    api::client::media::get_content_thumbnail::v3::Method as SdkThumbnailMethod,
    api::error::ErrorKind,
    events::room::{EncryptedFile, MediaSource},
};
use uuid::Uuid;

use crate::error::GatewayError;
use crate::manager::ClientManager;

/// Downloads MXC media through an account's live SDK client, on behalf of the
/// `axon-media` cache.
///
/// Cheap to [`Clone`] — holds only a [`ClientManager`] and the fetch timeout.
#[derive(Clone)]
pub struct SdkMediaProxy {
    manager: ClientManager,
    fetch_timeout: Duration,
}

impl SdkMediaProxy {
    pub fn new(manager: ClientManager, fetch_timeout: Duration) -> Self {
        Self {
            manager,
            fetch_timeout,
        }
    }

    /// Download `mxc_url` using `account_id`'s authenticated SDK client, bounded
    /// by the configured fetch timeout so a hung homeserver can't await forever.
    async fn download(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        encrypted_file: Option<serde_json::Value>,
    ) -> Result<Vec<u8>, GatewayError> {
        // Validate the MXC URI before doing any network work.
        axon_media::parse_mxc(mxc_url)
            .ok_or_else(|| GatewayError::Invalid(format!("invalid MXC URI: {mxc_url}")))?;

        let client = self.manager.get_or_connect(account_id).await?;

        // When the event carries a `content.file` object the media is encrypted;
        // deserialize it into ruma's `EncryptedFile` so the SDK can download and
        // decrypt in one step. Fall back to plain download otherwise.
        let source = if let Some(file_json) = encrypted_file {
            let enc: EncryptedFile = serde_json::from_value(file_json)
                .map_err(|e| GatewayError::Invalid(format!("invalid encrypted file: {e}")))?;
            MediaSource::Encrypted(Box::new(enc))
        } else {
            MediaSource::Plain(mxc_url.into())
        };

        let request = MediaRequestParameters {
            source,
            format: MediaFormat::File,
        };

        // `false` disables the SDK's own media cache: axon-media is the cache of
        // record, and letting the SDK also cache would double disk use with no
        // benefit (and outside our LRU accounting).
        let media = client.media();
        let download = media.get_media_content(&request, false);
        let data = tokio::time::timeout(self.fetch_timeout, download)
            .await
            .map_err(|_| {
                GatewayError::Upstream(format!(
                    "media download timed out after {}s",
                    self.fetch_timeout.as_secs()
                ))
            })?
            .map_err(|error| {
                if error.client_api_error_kind() == Some(&ErrorKind::NotFound) {
                    GatewayError::MediaNotFound(mxc_url.to_owned())
                } else {
                    GatewayError::Upstream(error.to_string())
                }
            })?;

        Ok(data)
    }

    /// Request a homeserver-generated thumbnail of `mxc_url` at `spec`'s
    /// dimensions/method, bounded by the same fetch timeout as [`download`].
    ///
    /// **Plain media only.** `Media::get_media_content` (matrix-sdk 0.18.0)
    /// only honors `MediaFormat::Thumbnail` when `request.source` is
    /// `MediaSource::Plain` — the `Encrypted` arm always downloads and
    /// decrypts the full ciphertext regardless of `format`, since a
    /// homeserver never sees encrypted-media plaintext to thumbnail. The API
    /// layer rejects encrypted media with a `400` before this is ever called,
    /// so there is no `encrypted_file` parameter here.
    async fn download_thumbnail(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        spec: ThumbnailSpec,
    ) -> Result<Vec<u8>, GatewayError> {
        axon_media::parse_mxc(mxc_url)
            .ok_or_else(|| GatewayError::Invalid(format!("invalid MXC URI: {mxc_url}")))?;

        let client = self.manager.get_or_connect(account_id).await?;

        let request = MediaRequestParameters {
            source: MediaSource::Plain(mxc_url.into()),
            format: MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(
                to_sdk_method(spec.method),
                spec.width.into(),
                spec.height.into(),
            )),
        };

        let media = client.media();
        let download = media.get_media_content(&request, false);
        let data = tokio::time::timeout(self.fetch_timeout, download)
            .await
            .map_err(|_| {
                GatewayError::Upstream(format!(
                    "media thumbnail download timed out after {}s",
                    self.fetch_timeout.as_secs()
                ))
            })?
            .map_err(|error| {
                if error.client_api_error_kind() == Some(&ErrorKind::NotFound) {
                    GatewayError::MediaNotFound(mxc_url.to_owned())
                } else {
                    GatewayError::Upstream(error.to_string())
                }
            })?;

        Ok(data)
    }
}

/// Map the shared, SDK-free [`ThumbnailMethod`] onto the SDK/ruma `Method`
/// that [`MediaThumbnailSettings`] needs.
fn to_sdk_method(method: ThumbnailMethod) -> SdkThumbnailMethod {
    match method {
        ThumbnailMethod::Crop => SdkThumbnailMethod::Crop,
        ThumbnailMethod::Scale => SdkThumbnailMethod::Scale,
    }
}

#[async_trait]
impl MediaFetcher for SdkMediaProxy {
    async fn fetch(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        encrypted_file: Option<serde_json::Value>,
    ) -> Result<Vec<u8>, FetchError> {
        self.download(account_id, mxc_url, encrypted_file)
            .await
            .map_err(gateway_to_fetch)
    }

    async fn fetch_thumbnail(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        spec: ThumbnailSpec,
    ) -> Result<Vec<u8>, FetchError> {
        self.download_thumbnail(account_id, mxc_url, spec)
            .await
            .map_err(gateway_to_fetch)
    }
}

/// Collapse the sync-layer [`GatewayError`] onto the cache-neutral
/// [`FetchError`] the `axon-media` cache passes back to the API adapter.
fn gateway_to_fetch(err: GatewayError) -> FetchError {
    match err {
        GatewayError::UnknownAccount(id) => {
            FetchError::AccountNotFound(format!("no such account: {id}"))
        }
        GatewayError::AccountNotActive(id) => {
            FetchError::AccountNotFound(format!("account not active: {id}"))
        }
        GatewayError::Invalid(msg) => FetchError::Invalid(msg),
        GatewayError::MediaNotFound(msg) => FetchError::NotFound(format!("media not found: {msg}")),
        GatewayError::RoomNotFound(msg) => FetchError::NotFound(format!("room not found: {msg}")),
        GatewayError::Forbidden(msg) => FetchError::Forbidden(msg),
        GatewayError::NotConnected(msg) => FetchError::NotConnected(msg),
        GatewayError::Upstream(msg) => FetchError::Upstream(msg),
    }
}
