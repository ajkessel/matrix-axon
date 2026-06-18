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

mod auth;
mod cursor;
mod dto;
mod extract;
mod lifecycle;
mod openapi;
mod response;
mod routes;
mod sender;
mod state;
mod verification;
mod ws;

pub use auth::{StoreTokenVerifier, TokenVerifier};
pub use lifecycle::{AccountLifecycle, DeleteError, LoginError, LogoutError, RecoverError};
pub use openapi::ApiDoc;
pub use response::{ApiError, ApiResponse, ErrorBody, ErrorResponse};
pub use sender::{MessageSender, SendError};
pub use state::AppState;
pub use verification::{FlowStage, FlowSummary, VerificationService, VerifyError};

use axum::{
    middleware::from_fn_with_state,
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
    // Every `/v1/…` HTTP route requires a valid bearer token (M7b). The guard is
    // a single layer over this sub-router rather than a per-route attachment, so
    // there is no route that can be added without it — including the lifecycle
    // verbs that were loopback-restricted before auth existed. `/healthz` and the
    // WebSocket are assembled outside it (below).
    let verifier = state.verifier.clone();
    let authed = Router::new()
        // Account read API: the cross-account list and a single account.
        .route("/v1/accounts", get(routes::accounts::list_accounts))
        // Runtime login / logout / recover — the secret-bearing lifecycle verbs.
        .route("/v1/accounts/login", post(routes::accounts::login))
        .route(
            "/v1/accounts/{account_id}/logout",
            post(routes::accounts::logout),
        )
        .route(
            "/v1/accounts/{account_id}/recover",
            post(routes::accounts::recover),
        )
        // Read one account and delete one account (same path, two methods).
        .route(
            "/v1/accounts/{account_id}",
            get(routes::accounts::get_account).delete(routes::accounts::delete_account),
        )
        // Interactive SAS verification: start/list on one path, per-flow read +
        // confirm/cancel below.
        .route(
            "/v1/accounts/{account_id}/verify",
            get(routes::verify::list_flows).post(routes::verify::start_verification),
        )
        .route(
            "/v1/accounts/{account_id}/verify/{flow_id}",
            get(routes::verify::get_flow),
        )
        .route(
            "/v1/accounts/{account_id}/verify/{flow_id}/confirm",
            post(routes::verify::confirm),
        )
        .route(
            "/v1/accounts/{account_id}/verify/{flow_id}/cancel",
            post(routes::verify::cancel),
        )
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
        .route_layer(from_fn_with_state(verifier, auth::require_bearer));

    Router::new()
        // Unversioned operational liveness probe — no auth (a monitor must reach
        // it without a token).
        .route("/healthz", get(healthz))
        .merge(authed)
        // Live event fan-out. Not in the OpenAPI document — a WebSocket upgrade
        // isn't expressible in OpenAPI 3.1; the frame protocol is documented in
        // the `ws` module and ADR 0020. A browser can't set an `Authorization`
        // header on a socket, so the handler authenticates the token itself at
        // upgrade time rather than riding the `require_bearer` layer.
        .route("/v1/ws", get(ws::ws_handler))
        .with_state(state)
}

/// Liveness probe. Always returns `200 OK` with `{"status":"ok"}` — it does not
/// touch the database, so a transient DB outage does not cause restarts.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
