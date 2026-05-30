//! axum HTTP and WebSocket handlers; OpenAPI spec via utoipa.
//!
//! `axon-api` owns the axum [`Router`] and all HTTP/WebSocket handlers. It
//! consumes a [`Store`](axon_store::Store) handle via router state rather than
//! opening its own database connections.
//!
//! Versioned application routes live under `/v1/` (added in later milestones).
//! `/healthz` is an unversioned operational liveness probe.

use axon_store::Store;
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

/// Build the top-level application router.
///
/// The [`Store`] is held as router state so handlers can reach the database;
/// it is unused by the current liveness probe but wired in for the `/v1/`
/// routes that follow.
pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(store)
}

/// Liveness probe. Always returns `200 OK` with `{"status":"ok"}` — it does not
/// touch the database, so a transient DB outage does not cause restarts.
async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
