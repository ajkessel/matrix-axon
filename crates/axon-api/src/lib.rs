//! axum HTTP and WebSocket handlers; OpenAPI spec via utoipa.
//!
//! `axon-api` owns the axum [`Router`] and all HTTP/WebSocket handlers. It
//! consumes a [`Store`](axon_store::Store) handle via [`AppState`] router state
//! rather than opening its own database connections.
//!
//! Versioned application routes live under `/v1/`. Account-scoped resources nest
//! under `/v1/accounts/{account_id}/…`; `/v1/rooms` is the cross-account
//! aggregate list. Live events stream over the `/v1/ws` WebSocket. `/healthz` is
//! an unversioned operational liveness probe. The response envelope (`{data}` /
//! `{error}`) lives in [`response`]; the OpenAPI document is [`ApiDoc`].

mod cursor;
mod dto;
mod extract;
mod openapi;
mod response;
mod routes;
mod sender;
mod state;
mod ws;

pub use openapi::ApiDoc;
pub use response::{ApiError, ApiResponse, ErrorBody, ErrorResponse};
pub use sender::{MessageSender, SendError};
pub use state::AppState;

use axum::{
    routing::{get, post, put},
    Json, Router,
};
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
        // Mutations (M6). account_id is nested in the path; the response is the
        // created event id. Edit (PUT) and redact (DELETE) share one path.
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/send",
            post(routes::messages::send_message),
        )
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}",
            put(routes::messages::edit_message).delete(routes::messages::redact_event),
        )
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}/reactions",
            post(routes::messages::react),
        )
        // Live event fan-out. Not in the OpenAPI document — a WebSocket upgrade
        // isn't expressible in OpenAPI 3.1; the frame protocol is documented in
        // the `ws` module and ADR 0020.
        .route("/v1/ws", get(ws::ws_handler))
        .with_state(state)
}

/// Liveness probe. Always returns `200 OK` with `{"status":"ok"}` — it does not
/// touch the database, so a transient DB outage does not cause restarts.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
