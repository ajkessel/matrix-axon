//! Server status endpoint (M10).
//!
//! `GET /v1/status` reports the history-backfill engine's disk-space health via
//! the [`BackfillStatusProvider`] port, so a client can tell when backfill has
//! paused (and re-poll to see it resume). Authenticated like the rest of `/v1`.

use std::sync::Arc;

use axon_store::Store;
use axum::extract::State;

use crate::backfill::BackfillStatusProvider;
use crate::dto::{BackfillStatusDto, StatusDto};
use crate::response::{ApiError, ApiResponse};

/// Report backfill status: the disk-space valve state (live free space, whether
/// paused) plus per-account backfill progress.
#[utoipa::path(
    get,
    path = "/v1/status",
    responses(
        (status = 200, description = "Current server status", body = ApiResponse<StatusDto>),
    ),
    tag = "status",
)]
pub async fn get_status(
    State(store): State<Store>,
    State(provider): State<Arc<dyn BackfillStatusProvider>>,
) -> Result<ApiResponse<StatusDto>, ApiError> {
    let progress = store.backfill_progress().await?;
    Ok(ApiResponse::new(StatusDto {
        backfill: BackfillStatusDto::new(provider.snapshot(), progress),
    }))
}
