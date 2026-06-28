//! Room endpoints: the cross-account list and a room's paginated timeline.

use axon_store::Store;
use axum::extract::State;
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::cursor;
use crate::dto::{EventDto, MemberDto, RoomDto, ThreadSummaryDto, TimelinePage};
use crate::extract::{Path, Query};
use crate::response::{ApiError, ApiResponse};

/// Default timeline page size when `limit` is omitted.
const DEFAULT_LIMIT: i64 = 50;
/// Hard cap on timeline page size, regardless of the requested `limit`.
const MAX_LIMIT: i64 = 200;

/// Query parameters for `GET /v1/rooms`.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RoomsQuery {
    /// Narrow the list to a single account. Omit for all accounts.
    pub account_id: Option<Uuid>,
}

/// List rooms across accounts, most-recent-activity first.
#[utoipa::path(
    get,
    path = "/v1/rooms",
    params(RoomsQuery),
    responses(
        (status = 200, description = "Rooms, newest activity first", body = ApiResponse<Vec<RoomDto>>),
    ),
    tag = "rooms",
)]
pub async fn list_rooms(
    State(store): State<Store>,
    Query(q): Query<RoomsQuery>,
) -> Result<ApiResponse<Vec<RoomDto>>, ApiError> {
    let rooms = store.list_rooms(q.account_id).await?;
    Ok(ApiResponse::new(
        rooms.into_iter().map(RoomDto::from).collect(),
    ))
}

/// Query parameters for the timeline read.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimelineQuery {
    /// Opaque cursor from a previous page's `next_cursor`; omit for the newest page.
    pub cursor: Option<String>,
    /// Page size (default 50, max 200).
    pub limit: Option<i64>,
}

/// Read a room's timeline, newest first, with cursor pagination.
///
/// An unknown `account_id`/`room_id` yields an empty page (200), not a 404 — an
/// empty timeline and a non-existent room are indistinguishable here and an
/// empty page is the natural answer.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/timeline",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        TimelineQuery,
    ),
    responses(
        (status = 200, description = "A page of timeline events", body = ApiResponse<TimelinePage>),
        (status = 400, description = "Malformed cursor", body = crate::response::ErrorResponse),
    ),
    tag = "rooms",
)]
pub async fn room_timeline(
    State(store): State<Store>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
    Query(q): Query<TimelineQuery>,
) -> Result<ApiResponse<TimelinePage>, ApiError> {
    let before = match q.cursor.as_deref() {
        Some(c) => Some(cursor::decode(c).ok_or_else(|| ApiError::bad_request("invalid cursor"))?),
        None => None,
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let rows = store
        .room_timeline(account_id, &room_id, before, limit)
        .await?;
    // A full page implies there may be more; a short page is the end.
    let next_cursor = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| cursor::encode(r.cursor())))
        .flatten();
    let events = rows
        .into_iter()
        .map(|r| EventDto::from_row(account_id, r))
        .collect();

    Ok(ApiResponse::new(TimelinePage {
        events,
        next_cursor,
    }))
}

/// List the current members of a room — the resolved `m.room.member` state,
/// one entry per user. Useful for seeding a display-name map without requiring
/// membership events to appear in the loaded timeline. An unknown
/// `account_id`/`room_id` yields an empty list (200), not a 404.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/members",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    responses(
        (status = 200, description = "Current room members", body = ApiResponse<Vec<MemberDto>>),
    ),
    tag = "rooms",
)]
pub async fn room_members(
    State(store): State<Store>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
) -> Result<ApiResponse<Vec<MemberDto>>, ApiError> {
    let rows = store
        .room_state_of_type(account_id, &room_id, "m.room.member")
        .await?;
    Ok(ApiResponse::new(
        rows.into_iter().map(MemberDto::from_state_row).collect(),
    ))
}

/// List the threads in a room (M8): one summary per distinct `m.thread` root,
/// most-recently-active first, each with its reply count and latest reply. The
/// root events themselves are read via the single-event endpoint; a thread's
/// members are paged via [`thread_timeline`]. An unknown `account_id`/`room_id`
/// (or a room with no threads) yields an empty list (200), not a 404. The list is
/// hard-capped in the store against a room with a pathological thread count.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/threads",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    responses(
        (status = 200, description = "Threads, most-recently-active first", body = ApiResponse<Vec<ThreadSummaryDto>>),
    ),
    tag = "rooms",
)]
pub async fn room_threads(
    State(store): State<Store>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
) -> Result<ApiResponse<Vec<ThreadSummaryDto>>, ApiError> {
    let threads = store.room_threads(account_id, &room_id).await?;
    Ok(ApiResponse::new(
        threads.into_iter().map(ThreadSummaryDto::from).collect(),
    ))
}

/// Read a thread's timeline (M8): the `m.thread` members whose root is
/// `root_id`, newest first, with the same cursor pagination as the room
/// timeline. The thread root itself is not included (fetch it via the
/// single-event endpoint). Reactions and edits on thread members are aggregated
/// onto their rows like any other timeline read. An unknown thread yields an
/// empty page (200); a malformed cursor is a 400.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/threads/{root_id}/timeline",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        ("root_id" = String, Path, description = "Matrix event id of the thread root"),
        TimelineQuery,
    ),
    responses(
        (status = 200, description = "A page of thread events", body = ApiResponse<TimelinePage>),
        (status = 400, description = "Malformed cursor", body = crate::response::ErrorResponse),
    ),
    tag = "rooms",
)]
pub async fn thread_timeline(
    State(store): State<Store>,
    Path((account_id, room_id, root_id)): Path<(Uuid, String, String)>,
    Query(q): Query<TimelineQuery>,
) -> Result<ApiResponse<TimelinePage>, ApiError> {
    let before = match q.cursor.as_deref() {
        Some(c) => Some(cursor::decode(c).ok_or_else(|| ApiError::bad_request("invalid cursor"))?),
        None => None,
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let rows = store
        .thread_timeline(account_id, &room_id, &root_id, before, limit)
        .await?;
    // A full page implies there may be more; a short page is the end.
    let next_cursor = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| cursor::encode(r.cursor())))
        .flatten();
    let events = rows
        .into_iter()
        .map(|r| EventDto::from_row(account_id, r))
        .collect();

    Ok(ApiResponse::new(TimelinePage {
        events,
        next_cursor,
    }))
}
