//! Authenticated media download via the matrix-rust-sdk client.
//!
//! [`SdkMediaProxy`] is the concrete implementation of the media-fetch
//! capability that `axon-api` defines as the [`MediaProxy`] trait. It resolves
//! the account's live SDK client through the [`ClientManager`] (connecting if
//! needed), then uses the SDK's built-in media API — which carries the
//! account's Bearer token automatically — to download MXC content.
//!
//! `axon-server` adapts this onto the `axon-api` `MediaProxy` port via the
//! usual composition-root adapter newtype, so this crate stays free of
//! `axon-api`.

use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::ruma::{
    api::error::ErrorKind,
    events::room::{EncryptedFile, MediaSource},
};
use uuid::Uuid;

use crate::error::GatewayError;
use crate::manager::ClientManager;

/// Returned by [`SdkMediaProxy::get_media`] on success.
pub struct SdkMediaContent {
    /// Raw media bytes from the homeserver.
    pub data: Vec<u8>,
    /// MIME type. The matrix-rust-sdk media API does not surface the
    /// `Content-Type` header, so this always returns `application/octet-stream`
    /// for now; TUI renderers can fall back to magic-byte detection.
    pub content_type: String,
}

/// Downloads MXC media through the account's live SDK client.
///
/// Cheap to [`Clone`] — holds only a [`ClientManager`].
#[derive(Clone)]
pub struct SdkMediaProxy {
    manager: ClientManager,
}

impl SdkMediaProxy {
    pub fn new(manager: ClientManager) -> Self {
        Self { manager }
    }

    /// Download `mxc_url` using `account_id`'s authenticated SDK client.
    ///
    /// Errors use [`GatewayError`] so the composition-root adapter can map them
    /// onto `axon-api`'s `MediaError` with the same translation it already uses
    /// for other sync-layer errors.
    pub async fn get_media(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        encrypted_file: Option<serde_json::Value>,
    ) -> Result<SdkMediaContent, GatewayError> {
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

        let data = client
            .media()
            .get_media_content(&request, true)
            .await
            .map_err(|error| {
                if error.client_api_error_kind() == Some(&ErrorKind::NotFound) {
                    GatewayError::MediaNotFound(mxc_url.to_owned())
                } else {
                    GatewayError::Upstream(error.to_string())
                }
            })?;

        // Temporary response-size guard until the bounded LRU cache (#97) adds
        // streaming and proper resource limits. The SDK has already buffered
        // the full response at this point, so this prevents forwarding oversized
        // media but does not bound the peak-memory spike during download.
        const MAX_MEDIA_BYTES: usize = 50 * 1024 * 1024;
        if data.len() > MAX_MEDIA_BYTES {
            return Err(GatewayError::Upstream(format!(
                "media response too large ({} bytes); limit is {} bytes",
                data.len(),
                MAX_MEDIA_BYTES
            )));
        }

        Ok(SdkMediaContent {
            data,
            content_type: "application/octet-stream".to_owned(),
        })
    }
}
