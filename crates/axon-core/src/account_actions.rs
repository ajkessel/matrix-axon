//! Wire-neutral account-action types, shared by `axon-api`'s
//! `AccountActionsSender` port and `axon-sync`'s `SdkGateway` profile/ignore/
//! directory methods (ADR 0068, M19f). Like [`CreateRoomRequest`](crate::CreateRoomRequest)
//! and [`PowerLevelChanges`](crate::PowerLevelChanges), these live in this
//! lowest crate so the two sibling crates share one type without depending on
//! each other; the composition root in `axon-server` forwards them verbatim.

/// A Matrix user's profile, as returned by `fetch_user_profile_of` (the
/// account's own profile is just this account's own user id). Either field
/// may be absent — a user with no display name or avatar set at all, not a
/// request error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatrixProfile {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// A request to search a homeserver's public-room directory
/// (`public_rooms_filtered`; ADR 0068, M19f). Unlike the other M19f verbs,
/// this is a paginated **read**, not a mutation — its own request/response
/// shape rather than the `{ "data": {} }` empty-object envelope the mutations
/// share.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicRoomsQuery {
    /// Search the directory of this server instead of the account's own
    /// homeserver. `None` means the account's own homeserver.
    pub server: Option<String>,
    /// Free-text filter over room name, topic, and canonical alias.
    pub search_term: Option<String>,
    /// Maximum rooms to return in this page.
    pub limit: Option<u32>,
    /// Pagination token from a previous page's `next_batch`.
    pub since: Option<String>,
}

/// One room in a [`PublicRoomsPage`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicRoomSummary {
    pub room_id: String,
    pub canonical_alias: Option<String>,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub avatar_url: Option<String>,
    pub num_joined_members: u64,
    pub world_readable: bool,
    pub guest_can_join: bool,
    /// The room's join rule wire form (e.g. `public`, `knock`, `invite`).
    pub join_rule: String,
    /// The room's `m.room.create` `type`, if any (e.g. `m.space`).
    pub room_type: Option<String>,
}

/// One page of a public-room directory search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicRoomsPage {
    pub chunk: Vec<PublicRoomSummary>,
    pub next_batch: Option<String>,
    pub prev_batch: Option<String>,
    pub total_room_count_estimate: Option<u64>,
}
