//! Mutation endpoints: send, edit, redact, and react.
//!
//! Each handler routes through the [`MessageSender`] port (injected via
//! `AppState`), turns its `event_id` result into a [`SendResultDto`], and maps a
//! [`SendError`](crate::sender::SendError) onto the shared error envelope. The
//! created event then round-trips back through sync and appears in the timeline
//! read and over `/v1/ws` — these handlers do not echo it.

use std::sync::Arc;

use axum::extract::State;
use uuid::Uuid;

use crate::dto::{EditRequest, ReactRequest, RedactQuery, SendMessageRequest, SendResultDto};
use crate::extract::{Json, Path, Query};
use crate::response::{ApiError, ApiResponse};
use crate::sender::MessageSender;

/// Send a plain-text message to a room.
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/send",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    request_body = SendMessageRequest,
    responses(
        (status = 200, description = "Message sent", body = ApiResponse<SendResultDto>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "messages",
)]
pub async fn send_message(
    State(sender): State<Arc<dyn MessageSender>>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
    Json(req): Json<SendMessageRequest>,
) -> Result<ApiResponse<SendResultDto>, ApiError> {
    let event_id = sender.send_message(account_id, &room_id, &req.body).await?;
    Ok(ApiResponse::new(SendResultDto { event_id }))
}

/// Edit an existing message (sends an `m.replace`).
#[utoipa::path(
    put,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        ("event_id" = String, Path, description = "Event id being edited"),
    ),
    request_body = EditRequest,
    responses(
        (status = 200, description = "Edit sent", body = ApiResponse<SendResultDto>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "messages",
)]
pub async fn edit_message(
    State(sender): State<Arc<dyn MessageSender>>,
    Path((account_id, room_id, event_id)): Path<(Uuid, String, String)>,
    Json(req): Json<EditRequest>,
) -> Result<ApiResponse<SendResultDto>, ApiError> {
    let new_id = sender
        .edit(account_id, &room_id, &event_id, &req.body)
        .await?;
    Ok(ApiResponse::new(SendResultDto { event_id: new_id }))
}

/// Redact an event, optionally with a reason (`?reason=`).
#[utoipa::path(
    delete,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        ("event_id" = String, Path, description = "Event id being redacted"),
        RedactQuery,
    ),
    responses(
        (status = 200, description = "Redaction sent", body = ApiResponse<SendResultDto>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "messages",
)]
pub async fn redact_event(
    State(sender): State<Arc<dyn MessageSender>>,
    Path((account_id, room_id, event_id)): Path<(Uuid, String, String)>,
    Query(q): Query<RedactQuery>,
) -> Result<ApiResponse<SendResultDto>, ApiError> {
    let redaction_id = sender
        .redact(account_id, &room_id, &event_id, q.reason.as_deref())
        .await?;
    Ok(ApiResponse::new(SendResultDto {
        event_id: redaction_id,
    }))
}

/// React to an event with an emoji/short key.
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}/reactions",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        ("event_id" = String, Path, description = "Event id being reacted to"),
    ),
    request_body = ReactRequest,
    responses(
        (status = 200, description = "Reaction sent", body = ApiResponse<SendResultDto>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "messages",
)]
pub async fn react(
    State(sender): State<Arc<dyn MessageSender>>,
    Path((account_id, room_id, event_id)): Path<(Uuid, String, String)>,
    Json(req): Json<ReactRequest>,
) -> Result<ApiResponse<SendResultDto>, ApiError> {
    let reaction_id = sender
        .react(account_id, &room_id, &event_id, &req.key)
        .await?;
    Ok(ApiResponse::new(SendResultDto {
        event_id: reaction_id,
    }))
}
