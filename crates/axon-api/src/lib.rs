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
mod media;
mod openapi;
mod response;
mod routes;
mod sender;
mod state;
mod trust;
mod verification;
mod ws;

pub use auth::{StoreTokenVerifier, TokenVerifier};
pub use axon_core::Formatted;
pub use lifecycle::{AccountLifecycle, DeleteError, LoginError, LogoutError, RecoverError};
pub use media::{MediaContent, MediaError, MediaProxy};
pub use openapi::ApiDoc;
pub use response::{ApiError, ApiResponse, ErrorBody, ErrorResponse};
pub use sender::{MessageSender, SendError};
pub use state::AppState;
pub use trust::{CurrentTrust, SenderTrustService, TrustBundle, TrustError, TrustSnapshot};
pub use verification::{FlowStage, FlowSummary, VerificationService, VerifyError};

use axum::{
    middleware::from_fn_with_state,
    response::Html,
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
            "/v1/accounts/{account_id}/rooms/{room_id}/members",
            get(routes::rooms::room_members),
        )
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/timeline",
            get(routes::rooms::room_timeline),
        )
        // Relation aggregation reads (M8b): the room's threads and a thread-scoped
        // timeline, reusing the room timeline's cursor pagination.
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/threads",
            get(routes::rooms::room_threads),
        )
        .route(
            "/v1/accounts/{account_id}/rooms/{room_id}/threads/{root_id}/timeline",
            get(routes::rooms::thread_timeline),
        )
        .route(
            "/v1/accounts/{account_id}/events/{event_id}",
            get(routes::events::get_event),
        )
        // Per-event relation aggregations (M8b): reaction tallies, direct replies,
        // and the forensic edit trail — resolved regardless of pagination.
        .route(
            "/v1/accounts/{account_id}/events/{event_id}/reactions",
            get(routes::events::get_reactions),
        )
        .route(
            "/v1/accounts/{account_id}/events/{event_id}/replies",
            get(routes::events::get_replies),
        )
        .route(
            "/v1/accounts/{account_id}/events/{event_id}/edits",
            get(routes::events::get_edits),
        )
        // Per-event verification bundle (M7c): at-decrypt snapshot + live evidence.
        .route(
            "/v1/accounts/{account_id}/events/{event_id}/verification",
            get(routes::events::get_verification_bundle),
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
        // Media proxy. Authenticated download of an `mxc://` resource through
        // the account's live homeserver connection. Returns raw bytes, not the
        // JSON envelope, so it is not expressible cleanly in the same response
        // schema as the rest of the read API.
        .route(
            "/v1/media/{account_id}/{server_name}/{media_id}",
            get(routes::media::get_media),
        )
        // Keep unmatched `/v1/...` paths inside the authenticated API boundary:
        // they must not fall through to the browser-facing HTML fallback below.
        .route("/v1", get(v1_not_found))
        .route("/v1/{*path}", get(v1_not_found))
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
        // Human-facing browser fallback for the server root / other non-API
        // unmatched paths. This is intentionally outside the `/v1` subtree so
        // unknown API routes keep auth + JSON semantics above.
        .fallback(browser_fallback)
        .with_state(state)
}

/// Liveness probe. Always returns `200 OK` with `{"status":"ok"}` — it does not
/// touch the database, so a transient DB outage does not cause restarts.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// JSON `404` for unknown `/v1/...` paths. These stay in the authenticated API
/// boundary instead of falling through to the browser-facing fallback page.
async fn v1_not_found() -> ApiError {
    ApiError::not_found("route not found")
}

/// Browser-facing informational page for unmatched non-API paths.
async fn browser_fallback() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Axon</title>
  </head>
  <body>
    <h1>Axon</h1>
    <p>Axon is a self-hosted Matrix agent and API server.</p>
    <p>No web interface is served at this address. Use a compatible client such as <code>axon-tui</code>.</p>
    <p>Operational checks live at <code>/healthz</code>. The API is served under <code>/v1/</code>.</p>
  </body>
</html>
"#,
    )
}
