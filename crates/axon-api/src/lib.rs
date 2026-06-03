//! axum HTTP and WebSocket handlers; OpenAPI spec via utoipa.
//!
//! `axon-api` owns the axum [`Router`] and all HTTP/WebSocket handlers. It
//! consumes a [`Store`](axon_store::Store) handle via [`AppState`] router state
//! rather than opening its own database connections.
//!
//! Versioned application routes live under `/v1/`. Account-scoped resources nest
//! under `/v1/accounts/{account_id}/…`; `/v1/rooms` is the cross-account
//! aggregate list. `/healthz` is an unversioned operational liveness probe. The
//! response envelope (`{data}` / `{error}`) lives in [`response`]; the OpenAPI
//! document is [`ApiDoc`].

mod cursor;
mod dto;
mod extract;
mod openapi;
mod response;
mod routes;
mod state;

pub use openapi::ApiDoc;
pub use response::{ApiError, ApiResponse, ErrorBody, ErrorResponse};
pub use state::AppState;

use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

/// Build the top-level application router over the shared [`AppState`].
///
/// Handlers pull just the state they need (e.g. `State<Store>`) via `FromRef`,
/// so new shared dependencies can be added to `AppState` without touching
/// existing routes.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/rooms", get(routes::rooms::list_rooms))
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/timeline",
            get(routes::rooms::room_timeline),
        )
        .route(
            "/v1/accounts/{account_id}/events/{event_id}",
            get(routes::events::get_event),
        )
        .with_state(state)
}

/// Liveness probe. Always returns `200 OK` with `{"status":"ok"}` — it does not
/// touch the database, so a transient DB outage does not cause restarts.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
