//! Wire-neutral room-creation request, shared by `axon-api`'s `RoomEntrySender`
//! port and `axon-sync`'s `SdkGateway::create_room` (ADR 0068, M19c). Like
//! [`MediaAttachment`](crate::MediaAttachment), it lives in this lowest crate
//! so the two sibling crates share one type without depending on each other;
//! the composition root in `axon-server` forwards it verbatim.

/// A room-creation preset, mirroring the three Matrix defines. Left unset on
/// [`CreateRoomRequest::preset`], the homeserver falls back to its own
/// create-room default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomPreset {
    Private,
    Public,
    TrustedPrivate,
}

/// A new room to create — the minimal-but-useful subset of the Matrix
/// `create_room` endpoint surfaced to API clients. Deferred fields (room
/// alias, room version, power-level overrides, raw creation content) aren't
/// exposed yet; add them alongside a matching field here if a caller needs
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateRoomRequest {
    pub name: Option<String>,
    pub topic: Option<String>,
    pub invite: Vec<String>,
    pub is_direct: bool,
    /// Whether the room is published in the room directory. Defaults to
    /// `false` (private), matching the Matrix spec's own default.
    pub public: bool,
    pub preset: Option<RoomPreset>,
    /// When true, an `m.room.encryption` event is included in the room's
    /// `initial_state` so it is encrypted from its very first
    /// (`m.room.create`) transaction — never via a post-hoc
    /// `Room::enable_encryption()`, which has a real send-before-encrypted
    /// race window (ADR 0068's M19c verification item).
    pub encrypted: bool,
}
