//! The media-proxy port the media handler depends on.
//!
//! `axon-api` defines this trait — the capability it *needs* — rather than
//! depending on whatever provides it. The real implementation lives in
//! `axon-sync` (via the SDK client's authenticated download), adapted onto this
//! port by `axon-server`. So this crate stays free of `axon-sync` and
//! `matrix-sdk`: handlers speak only [`MediaProxy`] and plain types.

use async_trait::async_trait;
use serde_json::Value;
use uuid::Uuid;

/// What can go wrong fetching media. Deliberately small and HTTP-shaped: the
/// adapter that implements [`MediaProxy`] collapses its richer backend error
/// into one of these so the handler maps a stable set of statuses.
#[derive(Debug)]
pub enum MediaError {
    /// The account does not exist or is not active. → `404`.
    AccountNotFound(String),
    /// The media was not found on the homeserver. → `404`.
    NotFound(String),
    /// The MXC URI is syntactically invalid. → `400`.
    Invalid(String),
    /// The homeserver refused the request (e.g. `M_FORBIDDEN`). → `403`.
    Forbidden(String),
    /// The account is not yet reachable; caller should retry. → `503`.
    NotConnected(String),
    /// The homeserver was unreachable or failed the request. → `502`.
    Upstream(String),
}

impl From<MediaError> for crate::response::ApiError {
    fn from(err: MediaError) -> Self {
        match err {
            MediaError::AccountNotFound(msg) | MediaError::NotFound(msg) => Self::not_found(msg),
            MediaError::Invalid(msg) => Self::bad_request(msg),
            MediaError::Forbidden(msg) => Self::forbidden(msg),
            MediaError::NotConnected(msg) => Self::service_unavailable(msg),
            MediaError::Upstream(msg) => Self::bad_gateway(msg),
        }
    }
}

/// The downloaded bytes and their MIME type.
pub struct MediaContent {
    /// Raw media bytes.
    pub data: Vec<u8>,
    /// MIME type, e.g. `image/jpeg`. Falls back to `application/octet-stream`
    /// when the homeserver does not supply a `Content-Type`.
    pub content_type: String,
}

/// Fetches media from a homeserver on behalf of an account. Implemented outside
/// this crate; held in [`AppState`](crate::AppState) as `Arc<dyn MediaProxy>`.
#[async_trait]
pub trait MediaProxy: Send + Sync {
    /// Download the media identified by `mxc_url` (`mxc://server/media_id`)
    /// using the given account's credentials. `encrypted_file` is the matching
    /// `content.file` or `content.info.thumbnail_file` JSON object from the
    /// Matrix event when the media is encrypted; the implementation uses it to
    /// decrypt after downloading. Pass `None` for plain (unencrypted) media.
    /// Returns decrypted bytes and MIME type on success.
    async fn get_media(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        encrypted_file: Option<Value>,
    ) -> Result<MediaContent, MediaError>;
}
