//! The live-event bus payload.
//!
//! [`LiveFrame`] is what the sync engine publishes onto the live-event bus, and
//! what the `/v1/ws` WebSocket handler fans out to connected clients. It is an
//! enum so one broadcast channel carries every kind of live frame — timeline
//! events ([`LiveEvent`]) today, and interactive-verification frames
//! ([`VerificationFrame`]) — without a second channel or lag domain. The wire
//! `type` tag that distinguishes them on the socket is owned by `axon-api`.
//!
//! These types are deliberately **wire-neutral**: they carry the fields the
//! read/WS API needs, but the HTTP/WebSocket envelope shape is owned by
//! `axon-api`. Keeping them here — the lowest crate — lets the two sibling
//! crates (`axon-sync` produces, `axon-api` consumes) share them without either
//! depending on the other.

use serde_json::Value;
use uuid::Uuid;

/// Everything the live-event bus carries. A single
/// [`tokio::sync::broadcast`](https://docs.rs/tokio/latest/tokio/sync/broadcast)
/// channel of these fans out to every `/v1/ws` subscriber, so all live traffic
/// shares one ordering and one lag signal. `Clone` is required because the
/// channel clones each message to every receiver.
#[derive(Debug, Clone)]
pub enum LiveFrame {
    /// A freshly persisted timeline event.
    Timeline(LiveEvent),
    /// A state change in an interactive (SAS) device-verification flow.
    Verification(VerificationFrame),
}

impl From<LiveEvent> for LiveFrame {
    fn from(event: LiveEvent) -> Self {
        LiveFrame::Timeline(event)
    }
}

impl From<VerificationFrame> for LiveFrame {
    fn from(frame: VerificationFrame) -> Self {
        LiveFrame::Verification(frame)
    }
}

/// A state change in an interactive SAS verification flow, ready to fan out over
/// the live-event bus. Carries everything a client needs to render the flow's
/// new stage without an out-of-band read — though the same values stay
/// re-readable via `GET …/verify/{flow_id}` while the flow is live, which is how
/// a reconnecting client that missed a frame recovers (see ADR 0011 / M7a PR6).
#[derive(Debug, Clone)]
pub struct VerificationFrame {
    /// Axon account this flow belongs to.
    pub account_id: Uuid,
    /// The verification transaction id, stable for the life of the flow.
    pub flow_id: String,
    /// Which stage this frame reports.
    pub kind: VerificationFrameKind,
    /// The other device in the flow (the user's trusted device).
    pub target_device_id: String,
    /// The SAS emoji as `(symbol, description)` pairs — present once keys are
    /// exchanged (a [`VerificationFrameKind::Sas`] frame).
    pub emoji: Option<Vec<(String, String)>>,
    /// The SAS decimal triple — the alternative representation of the same SAS,
    /// present alongside `emoji`.
    pub decimals: Option<(u16, u16, u16)>,
    /// A human-readable outcome — the cancel reason for a
    /// [`VerificationFrameKind::Cancelled`] frame.
    pub outcome: Option<String>,
}

/// Which stage of a verification flow a [`VerificationFrame`] reports. Maps 1:1
/// to the `verification.*` wire `type` tag in `axon-api`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationFrameKind {
    /// A verification was requested (including peer-initiated requests, which
    /// have no HTTP kickoff).
    Requested,
    /// The SAS is ready to compare — `emoji`/`decimals` are populated.
    Sas,
    /// The flow completed successfully; the device is now cross-signed.
    Done,
    /// The flow was cancelled (timeout, mismatch, or either side cancelling).
    Cancelled,
}

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
