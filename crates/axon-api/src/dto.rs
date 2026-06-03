//! Wire DTOs for the read API, mapped from `axon-store` row types.
//!
//! The store rows (`RoomSummary`, `TimelineRow`) are store-internal and don't
//! derive `Serialize`; these are the public JSON shapes, owned by the API layer.

use axon_store::{RoomSummary, TimelineRow};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// A room in the cross-account list (`GET /v1/rooms`). Identity is
/// `(account_id, room_id)` — a room joined by two accounts appears twice.
#[derive(Debug, Serialize, ToSchema)]
pub struct RoomDto {
    /// Axon account this room belongs to.
    pub account_id: Uuid,
    /// Matrix room ID.
    pub room_id: String,
    /// Room name (`m.room.name`), if set.
    pub name: Option<String>,
    /// Room topic (`m.room.topic`), if set.
    pub topic: Option<String>,
    /// Avatar `mxc://` URI (`m.room.avatar`), if set.
    pub avatar_url: Option<String>,
    /// Canonical alias (`m.room.canonical_alias`), if set.
    pub canonical_alias: Option<String>,
    /// `origin_server_ts` of the most recent event, in milliseconds — the sort key.
    pub last_activity_ts: i64,
    /// The most recent event's id, if the room has any events.
    pub last_event_id: Option<String>,
}

impl From<RoomSummary> for RoomDto {
    fn from(r: RoomSummary) -> Self {
        RoomDto {
            account_id: r.account_id,
            room_id: r.room_id,
            name: r.name,
            topic: r.topic,
            avatar_url: r.avatar_url,
            canonical_alias: r.canonical_alias,
            last_activity_ts: r.last_activity_ts,
            last_event_id: r.last_event_id,
        }
    }
}

/// A single timeline event — used both as a timeline element and as the
/// single-event payload. `content`/`body` are `null` for UTDs and for redacted
/// events; `redacted` is the convenience flag derived from `redaction_event_id`.
#[derive(Debug, Serialize, ToSchema)]
pub struct EventDto {
    /// Axon account this event belongs to.
    pub account_id: Uuid,
    /// Matrix event ID.
    pub event_id: String,
    /// Matrix room ID.
    pub room_id: String,
    /// Matrix user ID of the sender.
    pub sender: String,
    /// `origin_server_ts` in milliseconds.
    pub origin_ts: i64,
    /// Matrix event type, e.g. `m.room.message`.
    #[serde(rename = "type")]
    pub r#type: String,
    /// Decrypted `content` JSON. `null` for UTDs and redacted events.
    #[schema(value_type = Option<Object>)]
    pub content: Option<Value>,
    /// Plaintext body. `null` when absent or masked by redaction.
    pub body: Option<String>,
    /// The event's `m.relates_to` object, if any.
    #[schema(value_type = Option<Object>)]
    pub relates_to: Option<Value>,
    /// `true` when this event has been redacted (content/body masked).
    pub redacted: bool,
    /// The `event_id` of the redaction that masked this event, if redacted.
    pub redaction_event_id: Option<String>,
}

impl EventDto {
    /// Map a store [`TimelineRow`] into the wire DTO. `account_id` is threaded in
    /// from the request path because the store row doesn't carry it.
    pub fn from_row(account_id: Uuid, row: TimelineRow) -> Self {
        EventDto {
            account_id,
            event_id: row.event_id,
            room_id: row.room_id,
            sender: row.sender,
            origin_ts: row.origin_ts,
            r#type: row.event_type,
            content: row.content,
            body: row.decrypted_body_text,
            relates_to: row.relates_to,
            redacted: row.redaction_event_id.is_some(),
            redaction_event_id: row.redaction_event_id,
        }
    }
}

/// One page of a room timeline: the events plus the cursor to fetch the next
/// (older) page. `next_cursor` is `null` when the last page has been reached.
#[derive(Debug, Serialize, ToSchema)]
pub struct TimelinePage {
    /// The page of events, newest first.
    pub events: Vec<EventDto>,
    /// Opaque cursor for the next (older) page, or `null` at the end.
    pub next_cursor: Option<String>,
}
