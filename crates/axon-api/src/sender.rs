//! The outbound-message port the mutation handlers depend on.
//!
//! `axon-api` defines this trait — the capability it *needs* — rather than
//! depending on whatever provides it. The real implementation lives in
//! `axon-sync` (its SDK gateway), adapted onto this port by `axon-server` (the
//! composition root). So this crate stays free of `axon-sync` and `matrix-sdk`:
//! handlers speak only [`MessageSender`] and plain types. This mirrors how the
//! read side stays decoupled via the wire-neutral `LiveEvent` in `axon-core`.
//!
//! Every operation returns the resulting Matrix event id on success; failures
//! are [`SendError`], whose variants map 1:1 to HTTP status in
//! [`response`](crate::response).

use async_trait::async_trait;
use axon_core::{Formatted, MediaAttachment, Relation};
use uuid::Uuid;

/// What can go wrong issuing a mutation. Deliberately small and HTTP-shaped: the
/// adapter that implements [`MessageSender`] collapses its richer backend error
/// into one of these so the handler layer maps a stable set of statuses.
#[derive(Debug)]
pub enum SendError {
    /// The addressed account or room doesn't exist / isn't joined. → `404`.
    NotFound(String),
    /// The operation isn't permitted (e.g. editing a message the account didn't
    /// author, or a homeserver permission denial). → `403`.
    Forbidden(String),
    /// The account couldn't be brought online (homeserver unreachable, auth
    /// failure). Transient and retryable. → `503`.
    Unavailable(String),
    /// A malformed parameter (e.g. an unparseable room or event id). → `400`.
    Invalid(String),
    /// The upstream homeserver rejected or failed the operation. → `502`.
    Upstream(String),
}

/// Sends message-like events (message / edit / redact / react) on behalf of an
/// account. Implemented outside this crate; held in [`AppState`](crate::AppState)
/// as `Arc<dyn MessageSender>`.
#[async_trait]
pub trait MessageSender: Send + Sync {
    /// Send a message to a room; returns the new event id. `body` is the
    /// plain-text content; `formatted`, when present, carries the rich-text
    /// rendering (validated at the handler so both its fields are set).
    /// `relation` attaches an `m.relates_to` (reply and/or thread); its default
    /// (no targets) sends a plain, unrelated message.
    async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
        relation: Relation<'_>,
    ) -> Result<String, SendError>;

    /// Send a staged media attachment as an `m.image` or `m.file`. The
    /// attachment bytes are already claimed from the staging service; the sender
    /// owns only the Matrix SDK upload/send operation. `caption`, when present,
    /// becomes the media event body, otherwise the filename is the body.
    async fn send_media(
        &self,
        account_id: Uuid,
        room_id: &str,
        attachment: MediaAttachment,
        caption: Option<&str>,
        relation: Relation<'_>,
    ) -> Result<String, SendError>;

    /// Edit an existing message (`m.replace`); returns the replacement event id.
    /// `formatted` sets rich text on the replacement (see [`send_message`]).
    ///
    /// [`send_message`]: MessageSender::send_message
    async fn edit(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
    ) -> Result<String, SendError>;

    /// Redact an event, optionally with a reason; returns the redaction event id.
    async fn redact(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String, SendError>;

    /// React to an event with `key` (an emoji/short string); returns the
    /// reaction event id.
    async fn react(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<String, SendError>;
}
