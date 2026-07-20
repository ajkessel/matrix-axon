//! Composition-root adapter: binds `axon-sync`'s concrete [`SdkGateway`] to
//! `axon-api`'s [`MessageSender`], [`EphemeralSender`], [`MembershipSender`],
//! and [`RoomEntrySender`] ports.
//!
//! `axon-api` and `axon-sync` never depend on each other; this binary is the one
//! place that knows both, so the adapter lives here. It delegates each call to
//! the SDK gateway and maps the sync-layer [`GatewayError`] onto the API-layer
//! [`SendError`] — a mechanical 1:1 translation, the small cost of keeping the
//! two crates decoupled.

use std::time::Duration;

use async_trait::async_trait;
use axon_api::{
    EphemeralSender, Formatted, MediaAttachment, MembershipSender, MessageSender, Relation,
    RoomEntrySender, SendError,
};
use axon_core::CreateRoomRequest;
use axon_sync::{GatewayError, SdkGateway};
use uuid::Uuid;

/// Wraps the sync engine's gateway so it satisfies the API's `MessageSender`
/// port. The orphan rule requires a local newtype to carry the impl.
pub struct GatewayAdapter {
    gateway: SdkGateway,
    upstream_upload_timeout: Duration,
    ephemeral_send_timeout: Duration,
    membership_mutation_timeout: Duration,
    room_entry_timeout: Duration,
}

impl GatewayAdapter {
    pub fn new(
        gateway: SdkGateway,
        upstream_upload_timeout: Duration,
        ephemeral_send_timeout: Duration,
        membership_mutation_timeout: Duration,
        room_entry_timeout: Duration,
    ) -> Self {
        Self {
            gateway,
            upstream_upload_timeout,
            ephemeral_send_timeout,
            membership_mutation_timeout,
            room_entry_timeout,
        }
    }
}

/// The `SendError::Upstream` an outbound call reports when it hits its
/// configured per-call `timeout` before the homeserver responds. Shared by
/// every timeout site in this adapter (media upload, ephemeral sends,
/// membership mutations, room-entry mutations) so the message format has one
/// definition instead of one per port.
fn timed_out(verb: &str, timeout: Duration) -> SendError {
    SendError::Upstream(format!("{verb} timed out after {}s", timeout.as_secs()))
}

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
        relation: Relation<'_>,
    ) -> Result<String, SendError> {
        self.gateway
            .send_message(account_id, room_id, body, formatted, relation)
            .await
            .map_err(map_err)
    }

    async fn send_media(
        &self,
        account_id: Uuid,
        room_id: &str,
        attachment: MediaAttachment,
        caption: Option<&str>,
        relation: Relation<'_>,
    ) -> Result<String, SendError> {
        tokio::time::timeout(
            self.upstream_upload_timeout,
            self.gateway
                .send_media(account_id, room_id, attachment, caption, relation),
        )
        .await
        .map_err(|_| timed_out("media send", self.upstream_upload_timeout))?
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
        self.gateway
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
        self.gateway
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
        self.gateway
            .react(account_id, room_id, event_id, key)
            .await
            .map_err(map_err)
    }
}

#[async_trait]
impl EphemeralSender for GatewayAdapter {
    async fn send_read_receipt(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
    ) -> Result<(), SendError> {
        tokio::time::timeout(
            self.ephemeral_send_timeout,
            self.gateway
                .send_read_receipt(account_id, room_id, event_id),
        )
        .await
        .map_err(|_| timed_out("read receipt", self.ephemeral_send_timeout))?
        .map_err(map_err)
    }

    async fn send_typing_notice(
        &self,
        account_id: Uuid,
        room_id: &str,
        typing: bool,
    ) -> Result<(), SendError> {
        tokio::time::timeout(
            self.ephemeral_send_timeout,
            self.gateway.send_typing_notice(account_id, room_id, typing),
        )
        .await
        .map_err(|_| timed_out("typing notice", self.ephemeral_send_timeout))?
        .map_err(map_err)
    }
}

#[async_trait]
impl MembershipSender for GatewayAdapter {
    async fn leave(&self, account_id: Uuid, room_id: &str) -> Result<(), SendError> {
        tokio::time::timeout(
            self.membership_mutation_timeout,
            self.gateway.leave(account_id, room_id),
        )
        .await
        .map_err(|_| timed_out("leave", self.membership_mutation_timeout))?
        .map_err(map_err)
    }

    async fn forget(&self, account_id: Uuid, room_id: &str) -> Result<(), SendError> {
        tokio::time::timeout(
            self.membership_mutation_timeout,
            self.gateway.forget(account_id, room_id),
        )
        .await
        .map_err(|_| timed_out("forget", self.membership_mutation_timeout))?
        .map_err(map_err)
    }

    async fn invite(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
    ) -> Result<(), SendError> {
        tokio::time::timeout(
            self.membership_mutation_timeout,
            self.gateway.invite(account_id, room_id, user_id),
        )
        .await
        .map_err(|_| timed_out("invite", self.membership_mutation_timeout))?
        .map_err(map_err)
    }

    async fn kick(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), SendError> {
        tokio::time::timeout(
            self.membership_mutation_timeout,
            self.gateway.kick(account_id, room_id, user_id, reason),
        )
        .await
        .map_err(|_| timed_out("kick", self.membership_mutation_timeout))?
        .map_err(map_err)
    }

    async fn ban(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), SendError> {
        tokio::time::timeout(
            self.membership_mutation_timeout,
            self.gateway.ban(account_id, room_id, user_id, reason),
        )
        .await
        .map_err(|_| timed_out("ban", self.membership_mutation_timeout))?
        .map_err(map_err)
    }

    async fn unban(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Result<(), SendError> {
        tokio::time::timeout(
            self.membership_mutation_timeout,
            self.gateway.unban(account_id, room_id, user_id, reason),
        )
        .await
        .map_err(|_| timed_out("unban", self.membership_mutation_timeout))?
        .map_err(map_err)
    }
}

#[async_trait]
impl RoomEntrySender for GatewayAdapter {
    async fn join(
        &self,
        account_id: Uuid,
        room_id_or_alias: &str,
        server_names: &[String],
    ) -> Result<String, SendError> {
        tokio::time::timeout(
            self.room_entry_timeout,
            self.gateway
                .join(account_id, room_id_or_alias, server_names),
        )
        .await
        .map_err(|_| timed_out("join", self.room_entry_timeout))?
        .map_err(map_err)
    }

    async fn knock(
        &self,
        account_id: Uuid,
        room_id_or_alias: &str,
        reason: Option<&str>,
        server_names: &[String],
    ) -> Result<String, SendError> {
        tokio::time::timeout(
            self.room_entry_timeout,
            self.gateway
                .knock(account_id, room_id_or_alias, reason, server_names),
        )
        .await
        .map_err(|_| timed_out("knock", self.room_entry_timeout))?
        .map_err(map_err)
    }

    async fn create_room(
        &self,
        account_id: Uuid,
        request: CreateRoomRequest,
    ) -> Result<String, SendError> {
        tokio::time::timeout(
            self.room_entry_timeout,
            self.gateway.create_room(account_id, request),
        )
        .await
        .map_err(|_| timed_out("create_room", self.room_entry_timeout))?
        .map_err(map_err)
    }

    async fn create_dm(&self, account_id: Uuid, user_id: &str) -> Result<String, SendError> {
        tokio::time::timeout(
            self.room_entry_timeout,
            self.gateway.create_dm(account_id, user_id),
        )
        .await
        .map_err(|_| timed_out("create_dm", self.room_entry_timeout))?
        .map_err(map_err)
    }
}
