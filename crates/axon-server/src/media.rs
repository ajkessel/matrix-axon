//! Composition-root adapter: binds `axon-sync`'s concrete [`SdkMediaProxy`] to
//! `axon-api`'s [`MediaProxy`] port.
//!
//! Same shape as the [`GatewayAdapter`](crate::gateway::GatewayAdapter): this
//! binary is the one place that knows both crates, so the adapter and the error
//! translation live here. `axon-api` and `axon-sync` never depend on each other.

use async_trait::async_trait;
use axon_api::{MediaContent, MediaError, MediaProxy};
use axon_sync::{GatewayError, SdkMediaProxy};
use uuid::Uuid;

/// Wraps the sync engine's media proxy so it satisfies the API's `MediaProxy`
/// port. The orphan rule requires a local newtype to carry the impl.
pub struct MediaProxyAdapter(pub SdkMediaProxy);

/// Map a sync-layer gateway error onto the API-layer media error: an unknown
/// account → account-not-found, a bad MXC URI → invalid, a homeserver
/// failure → upstream.
fn map_err(err: GatewayError) -> MediaError {
    match err {
        GatewayError::UnknownAccount(id) => {
            MediaError::AccountNotFound(format!("no such account: {id}"))
        }
        GatewayError::AccountNotActive(id) => {
            MediaError::AccountNotFound(format!("account not active: {id}"))
        }
        GatewayError::Invalid(msg) => MediaError::Invalid(msg),
        GatewayError::MediaNotFound(msg) => MediaError::NotFound(format!("media not found: {msg}")),
        GatewayError::RoomNotFound(msg) => MediaError::NotFound(format!("room not found: {msg}")),
        GatewayError::Forbidden(msg) => MediaError::Forbidden(msg),
        GatewayError::NotConnected(msg) => MediaError::NotConnected(msg),
        GatewayError::Upstream(msg) => MediaError::Upstream(msg),
    }
}

#[async_trait]
impl MediaProxy for MediaProxyAdapter {
    async fn get_media(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        encrypted_file: Option<serde_json::Value>,
    ) -> Result<MediaContent, MediaError> {
        self.0
            .get_media(account_id, mxc_url, encrypted_file)
            .await
            .map(|c| MediaContent {
                data: c.data,
                content_type: c.content_type,
            })
            .map_err(map_err)
    }
}
