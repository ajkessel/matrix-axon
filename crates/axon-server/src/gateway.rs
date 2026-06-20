//! Composition-root adapter: binds `axon-sync`'s concrete [`SdkGateway`] to
//! `axon-api`'s [`MessageSender`] port.
//!
//! `axon-api` and `axon-sync` never depend on each other; this binary is the one
//! place that knows both, so the adapter lives here. It delegates each call to
//! the SDK gateway and maps the sync-layer [`GatewayError`] onto the API-layer
//! [`SendError`] — a mechanical 1:1 translation, the small cost of keeping the
//! two crates decoupled.

use async_trait::async_trait;
use axon_api::{Formatted, MessageSender, SendError};
use axon_sync::{GatewayError, SdkGateway};
use uuid::Uuid;

/// Wraps the sync engine's gateway so it satisfies the API's `MessageSender`
/// port. The orphan rule requires a local newtype to carry the impl.
pub struct GatewayAdapter(pub SdkGateway);

/// Map a sync-layer gateway error onto the API-layer send error (and thus an
/// HTTP status): unknown account / room → not found, a failed connect →
/// unavailable, bad input → invalid, a homeserver failure → upstream.
fn map_err(err: GatewayError) -> SendError {
    match err {
        GatewayError::UnknownAccount(id) => SendError::NotFound(format!("no such account: {id}")),
        GatewayError::AccountNotActive(id) => {
            SendError::Forbidden(format!("account not active: {id}"))
        }
        GatewayError::RoomNotFound(room) => SendError::NotFound(format!("room not found: {room}")),
        GatewayError::MediaNotFound(media) => {
            SendError::NotFound(format!("media not found: {media}"))
        }
        GatewayError::Forbidden(msg) => SendError::Forbidden(msg),
        GatewayError::NotConnected(msg) => SendError::Unavailable(msg),
        GatewayError::Invalid(msg) => SendError::Invalid(msg),
        GatewayError::Upstream(msg) => SendError::Upstream(msg),
    }
}

#[async_trait]
impl MessageSender for GatewayAdapter {
    async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
    ) -> Result<String, SendError> {
        self.0
            .send_message(account_id, room_id, body, formatted)
            .await
            .map_err(map_err)
    }

    async fn edit(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
    ) -> Result<String, SendError> {
        self.0
            .edit(account_id, room_id, event_id, body, formatted)
            .await
            .map_err(map_err)
    }

    async fn redact(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String, SendError> {
        self.0
            .redact(account_id, room_id, event_id, reason)
            .await
            .map_err(map_err)
    }

    async fn react(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<String, SendError> {
        self.0
            .react(account_id, room_id, event_id, key)
            .await
            .map_err(map_err)
    }
}
