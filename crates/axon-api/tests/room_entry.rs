//! Handler tests for the room-entry routes (ADR 0068, M19c): `join`, `knock`,
//! `create_dm`, `create_room`.
//!
//! Mirrors `tests/membership.rs`: drives the real router with a
//! [`StubRoomEntry`] in place of the SDK gateway, asserting routing, request
//! decoding, the success envelope, and `SendError` → HTTP-status mapping
//! without a homeserver.
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-api --test room_entry -- --ignored
//! ```

mod common;

use std::sync::Arc;

use axon_api::{AppState, RoomEntrySender};
use axon_core::{CreateRoomRequest, RoomPreset};
use axon_store::Store;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{
    RoomEntryCall, RoomEntryOutcome, StubDeviceList, StubLifecycle, StubMediaProxy, StubRoomEntry,
    StubSender, StubTokenVerifier, StubTrust, StubVerification, TEST_TOKEN,
};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

/// Build the router around a given room-entry sender. The live bus, message
/// sender, and lifecycle port are unused here.
fn app(store: Store, room_entry: Arc<dyn RoomEntrySender>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    let sender = Arc::new(StubSender::ok("$unused:localhost"));
    let lifecycle = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let verify = Arc::new(StubVerification::ok("$unused-flow"));
    let trust = Arc::new(StubTrust::ok());
    let devices = Arc::new(StubDeviceList::ok());
    let verifier = Arc::new(StubTokenVerifier::ok());
    let media = Arc::new(StubMediaProxy);
    axon_api::router(
        AppState::new(
            store, live, sender, lifecycle, verify, trust, devices, verifier, media, None,
        )
        .with_room_entry(room_entry),
    )
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
async fn join_routes_to_sender_and_envelope_result() {
    let store = store().await;
    let stub = Arc::new(StubRoomEntry::ok());
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();

    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms/join"),
        Some(json!({ "room_id_or_alias": "!room:localhost" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room_id"], "!created:localhost");
    assert_eq!(
        stub.calls(),
        vec![RoomEntryCall::Join {
            account_id,
            room_id_or_alias: "!room:localhost".to_owned(),
            server_names: vec![],
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn join_with_server_names_routes_through() {
    let store = store().await;
    let stub = Arc::new(StubRoomEntry::ok());
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();

    let (status, _body) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms/join"),
        Some(json!({
            "room_id_or_alias": "#room:localhost",
            "server_names": ["example.org", "matrix.org"],
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stub.calls(),
        vec![RoomEntryCall::Join {
            account_id,
            room_id_or_alias: "#room:localhost".to_owned(),
            server_names: vec!["example.org".to_owned(), "matrix.org".to_owned()],
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn knock_routes_to_sender_with_optional_reason() {
    let store = store().await;
    let stub = Arc::new(StubRoomEntry::ok());
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();
    let uri = format!("/v1/accounts/{account_id}/rooms/knock");

    let (status, body) = send(
        &app,
        "POST",
        &uri,
        Some(json!({ "room_id_or_alias": "!room:localhost", "reason": "let me in" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room_id"], "!created:localhost");

    let (status, _) = send(
        &app,
        "POST",
        &uri,
        Some(json!({ "room_id_or_alias": "!room:localhost" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        stub.calls(),
        vec![
            RoomEntryCall::Knock {
                account_id,
                room_id_or_alias: "!room:localhost".to_owned(),
                reason: Some("let me in".to_owned()),
                server_names: vec![],
            },
            RoomEntryCall::Knock {
                account_id,
                room_id_or_alias: "!room:localhost".to_owned(),
                reason: None,
                server_names: vec![],
            },
        ]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn create_dm_routes_to_sender_and_envelope_result() {
    let store = store().await;
    let stub = Arc::new(StubRoomEntry::ok());
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();
    let user_id = "@bob:localhost";

    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms/dm"),
        Some(json!({ "user_id": user_id })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room_id"], "!created:localhost");
    assert_eq!(
        stub.calls(),
        vec![RoomEntryCall::CreateDm {
            account_id,
            user_id: user_id.to_owned(),
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn create_room_routes_to_sender_with_full_request() {
    let store = store().await;
    let stub = Arc::new(StubRoomEntry::ok());
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();

    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms"),
        Some(json!({
            "name": "Room",
            "topic": "Topic",
            "invite": ["@bob:localhost"],
            "is_direct": true,
            "public": true,
            "preset": "trusted_private_chat",
            "encrypted": true,
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room_id"], "!created:localhost");
    assert_eq!(
        stub.calls(),
        vec![RoomEntryCall::CreateRoom {
            account_id,
            request: CreateRoomRequest {
                name: Some("Room".to_owned()),
                topic: Some("Topic".to_owned()),
                invite: vec!["@bob:localhost".to_owned()],
                is_direct: true,
                public: true,
                preset: Some(RoomPreset::TrustedPrivate),
                encrypted: true,
            },
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn create_room_routes_to_sender_with_minimal_request() {
    let store = store().await;
    let stub = Arc::new(StubRoomEntry::ok());
    let app = app(store, stub.clone());

    let account_id = Uuid::new_v4();

    let (status, body) = send(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/rooms"),
        Some(json!({})),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["room_id"], "!created:localhost");
    assert_eq!(
        stub.calls(),
        vec![RoomEntryCall::CreateRoom {
            account_id,
            request: CreateRoomRequest::default(),
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn room_entry_error_maps_to_status() {
    let store = store().await;
    let account_id = Uuid::new_v4();
    let uri = format!("/v1/accounts/{account_id}/rooms/join");

    let cases = [
        (
            RoomEntryOutcome::NotFound("x".into()),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            RoomEntryOutcome::Forbidden("x".into()),
            StatusCode::FORBIDDEN,
            "forbidden",
        ),
        (
            RoomEntryOutcome::Unavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        ),
        (
            RoomEntryOutcome::Invalid("x".into()),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            RoomEntryOutcome::Upstream("x".into()),
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = app(store.clone(), Arc::new(StubRoomEntry::failing(outcome)));
        let (status, body) = send(
            &app,
            "POST",
            &uri,
            Some(json!({ "room_id_or_alias": "!r:localhost" })),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(body["error"]["code"], want_code);
    }
}
