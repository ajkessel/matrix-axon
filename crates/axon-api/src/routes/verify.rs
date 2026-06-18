//! Interactive SAS device-verification endpoints.
//!
//! These drive the asynchronous SAS state machine over HTTP, with live progress
//! pushed as `verification.*` frames on `/v1/ws`. Like every `/v1/` route they
//! sit behind the bearer-token gate (M7b, ADR 0029); the two GET reads expose no
//! secrets and a reconnecting client polls them to resume a flow. See
//! [`crate::verification`] for the port these delegate to and the error → status
//! mapping.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use uuid::Uuid;

use crate::dto::{FlowDto, StartVerifyRequest, StartVerifyResponse};
use crate::extract::{Json, Path};
use crate::response::{ApiError, ApiResponse};
use crate::verification::VerificationService;

/// Start a SAS verification against one of the account's trusted devices,
/// returning the new flow's `flow_id`. The exchange then proceeds
/// asynchronously: watch `verification.*` frames over `/v1/ws`, or poll
/// `GET …/verify/{flow_id}`.
///
/// Trust-bearing; gated by the bearer-token auth layer like every `/v1/` route
/// (M7b, ADR 0029).
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/verify",
    params(("account_id" = Uuid, Path, description = "Axon account id")),
    request_body = StartVerifyRequest,
    responses(
        (status = 200, description = "The started flow's transaction id", body = ApiResponse<StartVerifyResponse>),
        (status = 400, description = "The named device is not a known device of this account", body = crate::response::ErrorResponse),
        (status = 409, description = "The account is not active (logged out or being deleted)", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error sending the verification request", body = crate::response::ErrorResponse),
    ),
    tag = "verification",
)]
pub async fn start_verification(
    State(verify): State<Arc<dyn VerificationService>>,
    Path(account_id): Path<Uuid>,
    Json(req): Json<StartVerifyRequest>,
) -> Result<ApiResponse<StartVerifyResponse>, ApiError> {
    let flow_id = verify.start(account_id, &req.device_id).await?;
    Ok(ApiResponse::new(StartVerifyResponse { flow_id }))
}

/// List the account's currently-tracked verification flows (live, plus
/// recently-terminal ones still within their grace window).
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/verify",
    params(("account_id" = Uuid, Path, description = "Axon account id")),
    responses(
        (status = 200, description = "The account's tracked flows", body = ApiResponse<Vec<FlowDto>>),
        (status = 404, description = "No such account", body = crate::response::ErrorResponse),
    ),
    tag = "verification",
)]
pub async fn list_flows(
    State(verify): State<Arc<dyn VerificationService>>,
    Path(account_id): Path<Uuid>,
) -> Result<ApiResponse<Vec<FlowDto>>, ApiError> {
    let flows = verify.list(account_id).await?;
    Ok(ApiResponse::new(
        flows.into_iter().map(FlowDto::from).collect(),
    ))
}

/// Read one verification flow's replayable state — the endpoint a reconnecting
/// client reads to resume (current stage, target device, and the SAS
/// emoji/decimals once they're available).
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/verify/{flow_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("flow_id" = String, Path, description = "Verification transaction id"),
    ),
    responses(
        (status = 200, description = "The flow's current state", body = ApiResponse<FlowDto>),
        (status = 404, description = "No such account, or no such (live or recently-terminal) flow", body = crate::response::ErrorResponse),
    ),
    tag = "verification",
)]
pub async fn get_flow(
    State(verify): State<Arc<dyn VerificationService>>,
    Path((account_id, flow_id)): Path<(Uuid, String)>,
) -> Result<ApiResponse<FlowDto>, ApiError> {
    let flow = verify.get(account_id, &flow_id).await?;
    Ok(ApiResponse::new(FlowDto::from(flow)))
}

/// Confirm that the SAS matches, sending this side's MAC. Returns `204` once the
/// confirm is sent; completion (`verification.done`) arrives asynchronously.
/// Idempotent. Trust-bearing; gated by the bearer-token auth layer (M7b).
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/verify/{flow_id}/confirm",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("flow_id" = String, Path, description = "Verification transaction id"),
    ),
    responses(
        (status = 204, description = "Confirm sent (or already confirmed)"),
        (status = 404, description = "No such account or flow", body = crate::response::ErrorResponse),
        (status = 409, description = "The flow has not reached the SAS stage, or is already terminal", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error sending the MAC", body = crate::response::ErrorResponse),
    ),
    tag = "verification",
)]
pub async fn confirm(
    State(verify): State<Arc<dyn VerificationService>>,
    Path((account_id, flow_id)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    verify.confirm(account_id, &flow_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Cancel the flow. Returns `204`. Idempotent — cancelling an already-terminal
/// flow succeeds. Trust-bearing; gated by the bearer-token auth layer (M7b).
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/verify/{flow_id}/cancel",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("flow_id" = String, Path, description = "Verification transaction id"),
    ),
    responses(
        (status = 204, description = "Cancelled (or already terminal)"),
        (status = 404, description = "No such account or flow", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error sending the cancellation", body = crate::response::ErrorResponse),
    ),
    tag = "verification",
)]
pub async fn cancel(
    State(verify): State<Arc<dyn VerificationService>>,
    Path((account_id, flow_id)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    verify.cancel(account_id, &flow_id).await?;
    Ok(StatusCode::NO_CONTENT)
}
