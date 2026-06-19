//! Single-event endpoint and its per-event verification bundle.

use std::sync::Arc;

use axon_store::Store;
use axum::extract::State;
use uuid::Uuid;

use crate::dto::{EventDto, VerificationBundleDto};
use crate::extract::Path;
use crate::response::{ApiError, ApiResponse};
use crate::trust::SenderTrustService;

/// Read a single event by `(account_id, event_id)`. Redacted events come back
/// masked (`content`/`body` null, `redacted` true); an unknown id is a 404.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/events/{event_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("event_id" = String, Path, description = "Matrix event id"),
    ),
    responses(
        (status = 200, description = "The event", body = ApiResponse<EventDto>),
        (status = 404, description = "No such event", body = crate::response::ErrorResponse),
    ),
    tag = "events",
)]
pub async fn get_event(
    State(store): State<Store>,
    Path((account_id, event_id)): Path<(Uuid, String)>,
) -> Result<ApiResponse<EventDto>, ApiError> {
    let row = store
        .get_event(account_id, &event_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("event {event_id} not found")))?;
    Ok(ApiResponse::new(EventDto::from_row(account_id, row)))
}

/// Read the per-event verification bundle (M7c): the durable at-decrypt sender-
/// trust snapshot plus live cross-signing evidence for the sender's device and
/// identity. An unknown account/event is a 404; a logged-out account is a 409
/// (no live client to read evidence from). Gated by the bearer-token auth layer
/// like every `/v1/` route.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}/events/{event_id}/verification",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
        ("event_id" = String, Path, description = "Matrix event id"),
    ),
    responses(
        (status = 200, description = "The event's verification bundle", body = ApiResponse<VerificationBundleDto>),
        (status = 404, description = "No such account or event", body = crate::response::ErrorResponse),
        (status = 409, description = "The account is not active", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream error reading the sender's keys", body = crate::response::ErrorResponse),
    ),
    tag = "events",
)]
pub async fn get_verification_bundle(
    State(trust): State<Arc<dyn SenderTrustService>>,
    Path((account_id, event_id)): Path<(Uuid, String)>,
) -> Result<ApiResponse<VerificationBundleDto>, ApiError> {
    let bundle = trust.bundle(account_id, &event_id).await?;
    Ok(ApiResponse::new(VerificationBundleDto::from_bundle(
        event_id, bundle,
    )))
}
