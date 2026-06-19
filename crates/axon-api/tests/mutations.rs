//! Handler tests for the M6 mutation routes.
//!
//! These drive the real router with a [`StubSender`] in place of the SDK
//! gateway, so they assert routing, request decoding, the success envelope, and
//! `SendError` → HTTP-status mapping **without** a homeserver. `AppState` needs a
//! `Store` to build (the read handlers share the router), so like the other API
//! tests they connect to Postgres and are `#[ignore]`d by default — the store is
//! never queried by these handlers:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-api --test mutations -- --ignored
//! ```

mod common;

use std::sync::Arc;

use axon_api::{AppState, MessageSender};
use axon_store::Store;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    Call, Outcome, StubLifecycle, StubSender, StubTokenVerifier, StubTrust, StubVerification,
    TEST_TOKEN,
};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

/// Build the router around a given sender. The live bus and lifecycle port are
/// unused here.
fn app(store: Store, sender: Arc<dyn MessageSender>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    let lifecycle = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let verify = Arc::new(StubVerification::ok("$unused-flow"));
    let trust = Arc::new(StubTrust::ok());
    let verifier = Arc::new(StubTokenVerifier::ok());
    axon_api::router(AppState::new(
        store, live, sender, lifecycle, verify, trust, verifier,
    ))
}

/// Issue a request (optional JSON body) and return `(status, parsed body)`. Every
/// request carries the bearer token the auth gate (M7b) requires.
async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TEST_TOKEN}"));
    let req = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => {
            builder = builder.header("content-length", "0");
            builder.body(Body::empty()).unwrap()
        }
    };
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, json)
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn mutations_route_to_sender_and_envelope_result() {
    let store = store().await;
    let stub = Arc::new(StubSender::ok("$created:localhost"));
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();
    let room_id = "!room:localhost";
    let event_id = "$target:localhost";

    // send
    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/send"),
        Some(json!({ "body": "hello" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["event_id"], "$created:localhost");

    // edit
    let (status, _) = send(
        &app,
        "PUT",
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}"),
        Some(json!({ "body": "edited" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // redact with a reason
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}?reason=spam"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // react
    let (status, _) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/events/{event_id}/reactions"),
        Some(json!({ "key": "👍" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Each handler routed to the matching MessageSender call with the path/body args.
    assert_eq!(
        stub.calls(),
        vec![
            Call::Send {
                account_id,
                room_id: room_id.to_owned(),
                body: "hello".to_owned(),
            },
            Call::Edit {
                account_id,
                room_id: room_id.to_owned(),
                event_id: event_id.to_owned(),
                body: "edited".to_owned(),
            },
            Call::Redact {
                account_id,
                room_id: room_id.to_owned(),
                event_id: event_id.to_owned(),
                reason: Some("spam".to_owned()),
            },
            Call::React {
                account_id,
                room_id: room_id.to_owned(),
                event_id: event_id.to_owned(),
                key: "👍".to_owned(),
            },
        ]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn send_error_maps_to_status() {
    let store = store().await;
    let account_id = Uuid::new_v4();
    let uri = format!("/v1/accounts/{account_id}/rooms/!r:localhost/send");

    let cases = [
        (
            Outcome::NotFound("x".into()),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            Outcome::Forbidden("x".into()),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            Outcome::Unavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        ),
        (
            Outcome::Invalid("x".into()),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            Outcome::Upstream("x".into()),
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = app(store.clone(), Arc::new(StubSender::failing(outcome)));
        let (status, body) = send(&app, "POST", &uri, Some(json!({ "body": "hi" }))).await;
        assert_eq!(status, want_status);
        assert_eq!(body["error"]["code"], want_code);
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn malformed_body_is_enveloped_400_and_skips_sender() {
    let store = store().await;
    let stub = Arc::new(StubSender::ok("$created:localhost"));
    let app = app(store, stub.clone());
    let account_id = Uuid::new_v4();

    // Missing the required `body` field → JSON decode failure.
    let (status, err) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms/!r:localhost/send"),
        Some(json!({ "wrong": "field" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");
    // The body never decoded, so the sender was not invoked.
    assert!(stub.calls().is_empty());
}
