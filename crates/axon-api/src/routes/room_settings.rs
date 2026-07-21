//! Room settings mutations (ADR 0068, M19d): `name`, `topic`, `avatar`,
//! `tags`.
//!
//! Each handler routes through the [`RoomSettingsSender`] port (injected via
//! `AppState`), mirroring [`membership`](crate::routes::membership) — none of
//! these six return an id a caller needs back, so the success envelope
//! carries an empty object. `name`/`topic`/`avatar` write `m.room.*` **state**
//! (visible to every room member); `tags` writes `m.tag` room **account
//! data** instead — private to this account, no state event, despite ADR
//! 0068 grouping all four fields under one "room settings" batch. Every call
//! is logged with `account_id`, `room_id`, the target field/tag where
//! applicable, elapsed time, and — on failure — the mapped error.
//!
//! `set_room_avatar` is the one handler here shaped differently: it claims a
//! staged upload from [`StagedUploadService`] the same way
//! [`messages::send_media`](crate::routes::messages::send_media) does,
//! rather than taking a value directly in its request body — Axon has no
//! route that hands a client an `mxc://` URI to reference, so avatar-set
//! reuses the existing staged-upload flow instead of inventing one.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use uuid::Uuid;

use axon_core::MediaAttachment;

use crate::dto::{
    SetRoomAvatarRequest, SetRoomNameRequest, SetRoomTagRequest, SetRoomTopicRequest,
};
use crate::extract::{Json, Path};
use crate::response::{ApiError, ApiResponse};
use crate::sender::RoomSettingsSender;
use crate::uploads::StagedUploadService;

/// Set this room's name. An empty `name` clears it.
#[utoipa::path(
    put,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/name",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    request_body = SetRoomNameRequest,
    responses(
        (status = 200, description = "Name set", body = ApiResponse<serde_json::Value>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 403, description = "Operation not permitted", body = crate::response::ErrorResponse),
        (status = 404, description = "Account or room not found", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "room-settings",
)]
pub async fn set_room_name(
    State(room_settings): State<Arc<dyn RoomSettingsSender>>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
    Json(req): Json<SetRoomNameRequest>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let started_at = Instant::now();
    match room_settings
        .set_name(account_id, &room_id, &req.name)
        .await
    {
        Ok(()) => {
            tracing::info!(
                account_id = %account_id,
                room_id = %room_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                "room_settings: set_name succeeded"
            );
            Ok(ApiResponse::new(serde_json::json!({})))
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                room_id = %room_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "room_settings: set_name failed"
            );
            Err(err.into())
        }
    }
}

/// Set this room's topic. An empty `topic` clears it.
#[utoipa::path(
    put,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/topic",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    request_body = SetRoomTopicRequest,
    responses(
        (status = 200, description = "Topic set", body = ApiResponse<serde_json::Value>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 403, description = "Operation not permitted", body = crate::response::ErrorResponse),
        (status = 404, description = "Account or room not found", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "room-settings",
)]
pub async fn set_room_topic(
    State(room_settings): State<Arc<dyn RoomSettingsSender>>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
    Json(req): Json<SetRoomTopicRequest>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let started_at = Instant::now();
    match room_settings
        .set_topic(account_id, &room_id, &req.topic)
        .await
    {
        Ok(()) => {
            tracing::info!(
                account_id = %account_id,
                room_id = %room_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                "room_settings: set_topic succeeded"
            );
            Ok(ApiResponse::new(serde_json::json!({})))
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                room_id = %room_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "room_settings: set_topic failed"
            );
            Err(err.into())
        }
    }
}

/// Set this room's avatar from an already-staged upload.
#[utoipa::path(
    put,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/avatar",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    request_body = SetRoomAvatarRequest,
    responses(
        (status = 200, description = "Avatar set", body = ApiResponse<serde_json::Value>),
        (status = 400, description = "Malformed request, or the upload isn't an image", body = crate::response::ErrorResponse),
        (status = 403, description = "Operation not permitted", body = crate::response::ErrorResponse),
        (status = 404, description = "Account, room, or upload not found", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "room-settings",
)]
pub async fn set_room_avatar(
    State(room_settings): State<Arc<dyn RoomSettingsSender>>,
    State(uploads): State<Arc<dyn StagedUploadService>>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
    Json(req): Json<SetRoomAvatarRequest>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let started_at = Instant::now();
    let upload = uploads.claim_upload(account_id, req.upload_id).await?;
    let attachment: MediaAttachment = upload.into();

    match room_settings
        .set_avatar(account_id, &room_id, attachment)
        .await
    {
        Ok(()) => {
            tracing::info!(
                account_id = %account_id,
                room_id = %room_id,
                upload_id = %req.upload_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                "room_settings: set_avatar succeeded"
            );
            if let Err(err) = uploads.complete_upload(account_id, req.upload_id).await {
                tracing::warn!(
                    account_id = %account_id,
                    upload_id = %req.upload_id,
                    error = ?err,
                    "set avatar but failed to consume staged upload"
                );
            }
            Ok(ApiResponse::new(serde_json::json!({})))
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                room_id = %room_id,
                upload_id = %req.upload_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "room_settings: set_avatar failed"
            );
            if let Err(release_err) = uploads.release_upload(account_id, req.upload_id).await {
                tracing::warn!(
                    account_id = %account_id,
                    upload_id = %req.upload_id,
                    error = ?release_err,
                    "failed to release staged upload after avatar-set failure"
                );
            }
            Err(err.into())
        }
    }
}

/// Clear this room's avatar.
#[utoipa::path(
    delete,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/avatar",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
    ),
    responses(
        (status = 200, description = "Avatar cleared", body = ApiResponse<serde_json::Value>),
        (status = 400, description = "Malformed request", body = crate::response::ErrorResponse),
        (status = 403, description = "Operation not permitted", body = crate::response::ErrorResponse),
        (status = 404, description = "Account or room not found", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "room-settings",
)]
pub async fn remove_room_avatar(
    State(room_settings): State<Arc<dyn RoomSettingsSender>>,
    Path((account_id, room_id)): Path<(Uuid, String)>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let started_at = Instant::now();
    match room_settings.remove_avatar(account_id, &room_id).await {
        Ok(()) => {
            tracing::info!(
                account_id = %account_id,
                room_id = %room_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                "room_settings: remove_avatar succeeded"
            );
            Ok(ApiResponse::new(serde_json::json!({})))
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                room_id = %room_id,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "room_settings: remove_avatar failed"
            );
            Err(err.into())
        }
    }
}

/// Add or update a tag on this room (room account data, not a state event).
#[utoipa::path(
    put,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/tags/{tag}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        ("tag" = String, Path, description = "Tag wire form: m.favourite, m.lowpriority, m.server_notice, or a u.-prefixed custom tag"),
    ),
    request_body = SetRoomTagRequest,
    responses(
        (status = 200, description = "Tag set", body = ApiResponse<serde_json::Value>),
        (status = 400, description = "Malformed request or unrecognized tag", body = crate::response::ErrorResponse),
        (status = 403, description = "Operation not permitted", body = crate::response::ErrorResponse),
        (status = 404, description = "Account or room not found", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "room-settings",
)]
pub async fn set_room_tag(
    State(room_settings): State<Arc<dyn RoomSettingsSender>>,
    Path((account_id, room_id, tag)): Path<(Uuid, String, String)>,
    Json(req): Json<SetRoomTagRequest>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let started_at = Instant::now();
    match room_settings
        .set_tag(account_id, &room_id, &tag, req.order)
        .await
    {
        Ok(()) => {
            tracing::info!(
                account_id = %account_id,
                room_id = %room_id,
                tag = %tag,
                elapsed_ms = started_at.elapsed().as_millis(),
                "room_settings: set_tag succeeded"
            );
            Ok(ApiResponse::new(serde_json::json!({})))
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                room_id = %room_id,
                tag = %tag,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "room_settings: set_tag failed"
            );
            Err(err.into())
        }
    }
}

/// Remove a tag from this room.
#[utoipa::path(
    delete,
    path = "/v1/accounts/{account_id}/rooms/{room_id}/tags/{tag}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("room_id" = String, Path, description = "Matrix room id"),
        ("tag" = String, Path, description = "Tag wire form: m.favourite, m.lowpriority, m.server_notice, or a u.-prefixed custom tag"),
    ),
    responses(
        (status = 200, description = "Tag removed", body = ApiResponse<serde_json::Value>),
        (status = 400, description = "Malformed request or unrecognized tag", body = crate::response::ErrorResponse),
        (status = 403, description = "Operation not permitted", body = crate::response::ErrorResponse),
        (status = 404, description = "Account or room not found", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error", body = crate::response::ErrorResponse),
        (status = 503, description = "Account not reachable", body = crate::response::ErrorResponse),
    ),
    tag = "room-settings",
)]
pub async fn remove_room_tag(
    State(room_settings): State<Arc<dyn RoomSettingsSender>>,
    Path((account_id, room_id, tag)): Path<(Uuid, String, String)>,
) -> Result<ApiResponse<serde_json::Value>, ApiError> {
    let started_at = Instant::now();
    match room_settings.remove_tag(account_id, &room_id, &tag).await {
        Ok(()) => {
            tracing::info!(
                account_id = %account_id,
                room_id = %room_id,
                tag = %tag,
                elapsed_ms = started_at.elapsed().as_millis(),
                "room_settings: remove_tag succeeded"
            );
            Ok(ApiResponse::new(serde_json::json!({})))
        }
        Err(err) => {
            tracing::warn!(
                account_id = %account_id,
                room_id = %room_id,
                tag = %tag,
                elapsed_ms = started_at.elapsed().as_millis(),
                error = ?err,
                "room_settings: remove_tag failed"
            );
            Err(err.into())
        }
    }
}
