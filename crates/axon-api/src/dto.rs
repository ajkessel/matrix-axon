//! Wire DTOs for the read API, mapped from `axon-store` row types.
//!
//! The store rows (`RoomSummary`, `TimelineRow`) are store-internal and don't
//! derive `Serialize`; these are the public JSON shapes, owned by the API layer.

use axon_store::{Account, AccountState, RoomSummary, TimelineRow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// A room in the cross-account list (`GET /v1/rooms`). Identity is
/// `(account_id, room_id)` — a room joined by two accounts appears twice.
#[derive(Debug, Serialize, ToSchema)]
pub struct RoomDto {
    /// Axon account this room belongs to.
    pub account_id: Uuid,
    /// Matrix user ID for this Axon account.
    pub account_user_id: String,
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
            account_user_id: r.account_user_id,
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
    /// Matrix state key for state events. `null` for message-like events.
    pub state_key: Option<String>,
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

impl From<axon_core::LiveEvent> for EventDto {
    /// Map a live-bus [`LiveEvent`](axon_core::LiveEvent) into the wire DTO — the
    /// `/v1/ws` payload shape matches the read API's. A freshly synced event is
    /// never already-redacted (a redaction arrives as its own later event), so
    /// the redaction fields are always unset here.
    fn from(e: axon_core::LiveEvent) -> Self {
        EventDto {
            account_id: e.account_id,
            event_id: e.event_id,
            room_id: e.room_id,
            sender: e.sender,
            state_key: e.state_key,
            origin_ts: e.origin_ts,
            r#type: e.event_type,
            content: e.content,
            body: e.body,
            relates_to: e.relates_to,
            redacted: false,
            redaction_event_id: None,
        }
    }
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
            state_key: row.state_key,
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

/// Request body for sending a message (`POST …/rooms/{room_id}/send`). Sent as a
/// plain-text `m.room.message`; `account_id`/`room_id` come from the path.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SendMessageRequest {
    /// The message text.
    pub body: String,
}

/// Request body for editing a message (`PUT …/events/{event_id}`). Replaces the
/// target event's text via an `m.replace` relation.
#[derive(Debug, Deserialize, ToSchema)]
pub struct EditRequest {
    /// The new message text.
    pub body: String,
}

/// Request body for reacting to an event (`POST …/events/{event_id}/reactions`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ReactRequest {
    /// The reaction key — typically an emoji.
    pub key: String,
}

/// Query parameters for redaction (`DELETE …/events/{event_id}`).
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RedactQuery {
    /// Optional human-readable reason recorded on the redaction.
    pub reason: Option<String>,
}

/// Result of a successful mutation: the id of the event the homeserver created
/// (the message, the replacement, the redaction, or the reaction).
#[derive(Debug, Serialize, ToSchema)]
pub struct SendResultDto {
    /// The created Matrix event id.
    pub event_id: String,
}

/// Lifecycle state on the wire — the constrained enum form of
/// [`axon_store::AccountState`], so the OpenAPI schema advertises the closed set
/// (`active` / `deactivated` / `deleting`) and generated clients can model it
/// exhaustively rather than seeing an open `string`. Serialize-only: unknown
/// *stored* values are still rejected at the store boundary
/// (`AccountState::from_db`), so widening the wire vocabulary can't smuggle one in.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AccountStateDto {
    Active,
    Deactivated,
    Deleting,
}

impl From<AccountState> for AccountStateDto {
    fn from(s: AccountState) -> Self {
        match s {
            AccountState::Active => AccountStateDto::Active,
            AccountState::Deactivated => AccountStateDto::Deactivated,
            AccountState::Deleting => AccountStateDto::Deleting,
        }
    }
}

/// An account in the lifecycle read API (`GET /v1/accounts`,
/// `GET /v1/accounts/{account_id}`). The encrypted access token is deliberately
/// absent — only public lifecycle facts are exposed, never a secret.
#[derive(Debug, Serialize, ToSchema)]
pub struct AccountDto {
    /// Stable Axon account id.
    pub account_id: Uuid,
    /// Full Matrix user ID, e.g. `@alice:example.org`.
    pub user_id: String,
    /// Homeserver base URL.
    pub homeserver_url: String,
    /// Device ID of axon's current session, once it has logged in.
    pub device_id: Option<String>,
    /// Lifecycle state. The list endpoint returns only `active` accounts; a by-id
    /// read can return any state.
    pub state: AccountStateDto,
    /// Whether axon's own device is currently cross-signed (orthogonal to
    /// `state`), derived from the SDK and kept fresh by the verification watcher
    /// (ADR 0026): `false` for a fresh/unverified device, `true` once
    /// `recover`/`verify` has cross-signed it. Kept nullable on the wire for
    /// forward-compatibility, but currently always present.
    pub verified: Option<bool>,
    /// Row creation time, RFC 3339.
    pub created_at: String,
    /// Last update time, RFC 3339.
    pub updated_at: String,
}

impl From<Account> for AccountDto {
    fn from(a: Account) -> Self {
        AccountDto {
            account_id: a.account_id,
            user_id: a.user_id,
            homeserver_url: a.homeserver_url,
            device_id: a.device_id,
            state: a.state.into(),
            // Surface the derived cross-signing state (ADR 0026). Kept `Option`
            // for wire stability; populated from the persisted column.
            verified: Some(a.verified),
            created_at: a.created_at.to_rfc3339(),
            updated_at: a.updated_at.to_rfc3339(),
        }
    }
}

/// Request body for runtime login (`POST /v1/accounts/login`). Adds or
/// reactivates a Matrix account keyed by `(homeserver_url, username)`. The
/// password is used once to authenticate and is never stored or echoed back.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Homeserver base URL, e.g. `https://matrix.example.org`. Optional: when
    /// omitted, the server discovers it from the user ID's server name via
    /// `.well-known/matrix/client` (falling back to `https://<server name>`).
    /// Supply it explicitly to skip discovery (e.g. `http://localhost:8008`
    /// for a local dev homeserver).
    pub homeserver_url: Option<String>,
    /// Full Matrix user ID, e.g. `@alice:example.org`. A user ID written with
    /// the homeserver's hostname as its domain (`@alice:matrix.example.org`)
    /// is rejected with a 400 whose message suggests the canonical spelling.
    pub username: String,
    /// Account password. Consumed once at login; never persisted.
    pub password: String,
}

/// Request body for recovery-key key acquisition
/// (`POST /v1/accounts/{account_id}/recover`). The Secure-Storage (4S) recovery
/// key imports the account's megolm key backup + cross-signing keys, self-verifies
/// axon's device, and unlocks stored UTDs. Like the login password it is a
/// crown-jewel secret: used once to recover and **never persisted** or echoed back.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RecoverRequest {
    /// The account's Secure-Storage (4S) recovery key.
    pub recovery_key: String,
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
