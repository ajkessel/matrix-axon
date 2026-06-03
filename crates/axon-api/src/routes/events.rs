//! Single-event endpoint.

use axon_store::Store;
use axum::extract::State;
use uuid::Uuid;

use crate::dto::EventDto;
use crate::extract::Path;
use crate::response::{ApiError, ApiResponse};

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
