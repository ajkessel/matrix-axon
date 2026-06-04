//! The live-event bus payload.
//!
//! [`LiveEvent`] is what the sync engine publishes after persisting a timeline
//! event, and what the `/v1/ws` WebSocket handler fans out to connected clients.
//! It is deliberately **wire-neutral**: it carries the same fields the read
//! API's event DTO needs, but the HTTP/WebSocket envelope shape is owned by
//! `axon-api`. Keeping it here — the lowest crate — lets the two sibling crates
//! (`axon-sync` produces, `axon-api` consumes) share one type without either
//! depending on the other.

use serde_json::Value;
use uuid::Uuid;

/// A timeline event freshly persisted by the sync engine, ready to fan out over
/// the live-event bus. `Clone` is required because it travels a
/// [`tokio::sync::broadcast`](https://docs.rs/tokio/latest/tokio/sync/broadcast)
/// channel, which clones each message to every receiver.
///
/// Fields mirror the read API's event shape. A live event is never
/// already-redacted at arrival (a redaction is a separate event that arrives
/// later), so there is no redaction state here — the API maps it to a
/// non-redacted DTO.
#[derive(Debug, Clone)]
pub struct LiveEvent {
    /// Axon account this event belongs to.
    pub account_id: Uuid,
    /// Matrix event ID.
    pub event_id: String,
    /// Matrix room ID.
    pub room_id: String,
    /// Matrix user ID of the sender.
    pub sender: String,
    /// Matrix state key for state events. `None` for message-like events.
    pub state_key: Option<String>,
    /// `origin_server_ts` in milliseconds.
    pub origin_ts: i64,
    /// Matrix event type, e.g. `m.room.message`.
    pub event_type: String,
    /// Decrypted `content` JSON. `None` for events that arrived as UTDs.
    pub content: Option<Value>,
    /// Plaintext body, when the content carried one.
    pub body: Option<String>,
    /// The event's `m.relates_to` object, if any.
    pub relates_to: Option<Value>,
}
