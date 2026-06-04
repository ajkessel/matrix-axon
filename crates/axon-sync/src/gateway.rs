//! The message gateway: turns high-level mutation requests into matrix-rust-sdk
//! send calls on the right account's [`Client`](matrix_sdk::Client).
//!
//! [`SdkGateway`] owns *message semantics only* — building the ruma content for a
//! send / edit / redact / react and issuing it. It resolves the account's client
//! through the [`ClientManager`] (lazily connecting if needed) but knows nothing
//! about connection retry or caching; that is the manager's job. This is the
//! concrete capability `axon-server` adapts onto the API layer's `MessageSender`
//! port — `axon-api` never sees this type or any SDK type.
//!
//! Each method returns the resulting Matrix event id as a `String`. Errors are
//! [`GatewayError`], chosen so the composition-root adapter can map them onto
//! HTTP status without this crate knowing about HTTP.

use matrix_sdk::ruma::api::error::ErrorKind;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
use matrix_sdk::ruma::{EventId, RoomId};
use matrix_sdk::Room;
use serde_json::json;
use uuid::Uuid;

use crate::error::GatewayError;
use crate::manager::ClientManager;

/// Map an SDK error from a send/fetch into a [`GatewayError`]. A homeserver
/// `M_FORBIDDEN` (e.g. redacting without the required power level) becomes
/// [`GatewayError::Forbidden`] so it surfaces as `403`, not a generic `502`;
/// everything else is an upstream failure.
fn map_sdk_err(err: matrix_sdk::Error) -> GatewayError {
    match err.client_api_error_kind() {
        Some(ErrorKind::Forbidden) => GatewayError::Forbidden(err.to_string()),
        _ => GatewayError::Upstream(err.to_string()),
    }
}

/// Sends Matrix message-like events on behalf of an account, routed through that
/// account's SDK client. Cheap to [`Clone`] (holds only a [`ClientManager`]).
#[derive(Clone)]
pub struct SdkGateway {
    manager: ClientManager,
}

impl SdkGateway {
    /// Build a gateway over a client manager. Constructed by the sync engine and
    /// exposed via [`SyncEngine::gateway`](crate::SyncEngine::gateway).
    pub(crate) fn new(manager: ClientManager) -> Self {
        Self { manager }
    }

    /// Resolve the joined [`Room`] for `(account_id, room_id)`, connecting the
    /// account's client on demand. A malformed room id is a `400`-class
    /// [`GatewayError::Invalid`]; a room the client doesn't know is a `404`-class
    /// [`GatewayError::RoomNotFound`].
    async fn room(&self, account_id: Uuid, room_id: &str) -> Result<Room, GatewayError> {
        let client = self.manager.get_or_connect(account_id).await?;
        let parsed =
            RoomId::parse(room_id).map_err(|e| GatewayError::Invalid(format!("room id: {e}")))?;
        client
            .get_room(&parsed)
            .ok_or_else(|| GatewayError::RoomNotFound(room_id.to_owned()))
    }

    /// Send a plain-text `m.room.message`. Returns the new event id.
    pub async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let resp = room
            .send(RoomMessageEventContent::text_plain(body))
            .await
            .map_err(map_sdk_err)?;
        Ok(resp.response.event_id.to_string())
    }

    /// Edit a message by sending an `m.replace` replacement of `event_id`.
    /// Built as a raw envelope (`m.new_content` + `m.relates_to`) so we don't
    /// need the original event in hand. Returns the replacement event's id.
    pub async fn edit(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        // Validate the target id up front so a bad id is a clean 400, not a 502.
        let target_id = EventId::parse(event_id)
            .map_err(|e| GatewayError::Invalid(format!("event id: {e}")))?;

        // A Matrix edit (m.replace) is only valid from the *original author*, but
        // the homeserver does not enforce that — it accepts an m.replace pointing
        // at anyone's event. So we enforce it: fetch the target and refuse to send
        // a forged edit of a message this account didn't write (which would
        // otherwise return 200 and could be rendered by a naive client).
        let target = room.event(&target_id, None).await.map_err(map_sdk_err)?;
        if target.sender().as_deref() != Some(room.own_user_id()) {
            return Err(GatewayError::Forbidden(
                "can only edit your own messages".to_owned(),
            ));
        }

        let content = json!({
            "msgtype": "m.text",
            // The fallback body convention for clients that don't understand edits.
            "body": format!("* {body}"),
            "m.new_content": { "msgtype": "m.text", "body": body },
            "m.relates_to": { "rel_type": "m.replace", "event_id": event_id },
        });
        let resp = room
            .send_raw("m.room.message", content)
            .await
            .map_err(map_sdk_err)?;
        Ok(resp.response.event_id.to_string())
    }

    /// Redact `event_id`, optionally with a reason. Returns the redaction event's id.
    pub async fn redact(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let event_id = EventId::parse(event_id)
            .map_err(|e| GatewayError::Invalid(format!("event id: {e}")))?;
        let resp = room
            .redact(&event_id, reason, None)
            .await
            .map_err(|e| map_sdk_err(matrix_sdk::Error::from(e)))?;
        Ok(resp.event_id.to_string())
    }

    /// Send an `m.reaction` annotating `event_id` with `key` (an emoji or short
    /// string). Returns the reaction event's id.
    pub async fn react(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<String, GatewayError> {
        let room = self.room(account_id, room_id).await?;
        let event_id = EventId::parse(event_id)
            .map_err(|e| GatewayError::Invalid(format!("event id: {e}")))?;
        let content = ReactionEventContent::new(Annotation::new(event_id, key.to_owned()));
        let resp = room.send(content).await.map_err(map_sdk_err)?;
        Ok(resp.response.event_id.to_string())
    }
}
