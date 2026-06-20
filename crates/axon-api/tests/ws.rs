//! Integration test for the `/v1/ws` live-event fan-out.
//!
//! Binds the real router on an ephemeral port, connects a WebSocket client, then
//! publishes a `LiveEvent` on the same broadcast sender the router holds and
//! asserts the client receives the JSON envelope. The WebSocket path never
//! touches the database, but `AppState` needs a `Store` handle to build — so,
//! like the HTTP test, this connects to Postgres and is `#[ignore]`d by default:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-api --test ws -- --ignored
//! ```

mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axon_api::AppState;
use axon_core::{LiveEvent, LiveFrame, VerificationFrame, VerificationFrameKind};
use axon_store::Store;
use common::{
    StubLifecycle, StubMediaProxy, StubSender, StubTokenVerifier, StubTrust, StubVerification,
    TEST_TOKEN,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

/// Build a WebSocket client request for `url` carrying the bearer token the
/// `/v1/ws` upgrade requires (M7b).
fn authed_request(url: &str) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = url.into_client_request().expect("ws request");
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {TEST_TOKEN}")
            .parse()
            .expect("header value"),
    );
    request
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn ws_streams_live_events() {
    let store = store().await;
    // We keep the sender so the test can publish; the router subscribes per
    // connection. Dropping the initial receiver means the connected handler is
    // the *only* subscriber, so `send` succeeding proves the socket is wired.
    let (live, _) = broadcast::channel::<LiveFrame>(16);
    let app = axon_api::router(AppState::new(
        store,
        live.clone(),
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
    ));

    // Serve on an ephemeral port in the background.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // Connect a client. The router calls `subscribe()` while handling the
    // upgrade — before the 101 response the handshake waits on — so by the time
    // `connect_async` returns, the handler's receiver is registered and any
    // subsequent `send` reaches it.
    let url = format!("ws://{addr}/v1/ws");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(authed_request(&url))
        .await
        .expect("ws connect");

    // Publish one live event. With the initial receiver dropped, a successful
    // send means exactly the connected handler received it.
    let account_id = Uuid::new_v4();
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    let receivers = live
        .send(LiveFrame::Timeline(LiveEvent {
            account_id,
            event_id: event_id.clone(),
            room_id: "!room:localhost".to_owned(),
            sender: "@jamie:localhost".to_owned(),
            state_key: Some("@alice:localhost".to_owned()),
            origin_ts: 1234,
            event_type: "m.room.member".to_owned(),
            content: Some(json!({ "membership": "join", "displayname": "Alice" })),
            body: None,
            relates_to: None,
            sender_trust: None,
        }))
        .expect("a connected subscriber");
    assert_eq!(receivers, 1, "exactly the connected socket subscribed");

    // The client should receive the envelope.
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("a frame within the timeout")
        .expect("stream still open")
        .expect("a websocket message");
    let text = match frame {
        Message::Text(text) => text,
        other => panic!("expected a text frame, got {other:?}"),
    };
    let envelope: Value = serde_json::from_str(text.as_str()).expect("json frame");

    assert_eq!(envelope["type"], "timeline.event");
    assert_eq!(envelope["account_id"], account_id.to_string());
    assert_eq!(envelope["payload"]["event_id"], event_id.as_str());
    assert_eq!(envelope["payload"]["room_id"], "!room:localhost");
    assert_eq!(envelope["payload"]["sender"], "@jamie:localhost");
    assert_eq!(envelope["payload"]["state_key"], "@alice:localhost");
    assert_eq!(envelope["payload"]["type"], "m.room.member");
    assert_eq!(envelope["payload"]["body"], Value::Null);
    assert_eq!(envelope["payload"]["redacted"], false);

    // A verification frame rides the same socket, tagged distinctly.
    let flow_id = "$flow:localhost";
    let receivers = live
        .send(LiveFrame::Verification(VerificationFrame {
            account_id,
            flow_id: flow_id.to_owned(),
            kind: VerificationFrameKind::Sas,
            target_device_id: "TRUSTEDDEV".to_owned(),
            emoji: Some(vec![("🐶".to_owned(), "Dog".to_owned())]),
            decimals: Some((1, 2, 3)),
            outcome: None,
        }))
        .expect("a connected subscriber");
    assert_eq!(receivers, 1);

    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("a frame within the timeout")
        .expect("stream still open")
        .expect("a websocket message");
    let text = match frame {
        Message::Text(text) => text,
        other => panic!("expected a text frame, got {other:?}"),
    };
    let envelope: Value = serde_json::from_str(text.as_str()).expect("json frame");
    assert_eq!(envelope["type"], "verification.sas");
    assert_eq!(envelope["account_id"], account_id.to_string());
    assert_eq!(envelope["payload"]["flow_id"], flow_id);
    assert_eq!(envelope["payload"]["device_id"], "TRUSTEDDEV");
    assert_eq!(envelope["payload"]["emoji"][0]["symbol"], "🐶");
    assert_eq!(envelope["payload"]["decimals"], json!([1, 2, 3]));

    ws.close(None).await.ok();
    server.abort();
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn ws_upgrade_rejected_without_a_token() {
    let store = store().await;
    let (live, _rx) = broadcast::channel::<LiveFrame>(16);
    let app = axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
    ));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    // No Authorization header: the upgrade is refused with 401 before it
    // completes, so the handshake fails — carrying the bare RFC 6750 `Bearer`
    // challenge, same as the HTTP gate.
    let url = format!("ws://{addr}/v1/ws");
    let err = tokio_tungstenite::connect_async(&url)
        .await
        .expect_err("unauthenticated upgrade must be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 401);
            assert_eq!(resp.headers()["www-authenticate"], "Bearer");
        }
        other => panic!("expected an HTTP 401 rejection, got {other:?}"),
    }

    // A present but wrong token: rejected with the `error="invalid_token"`
    // challenge (RFC 6750 §3.1).
    let mut bad = url.clone().into_client_request().expect("ws request");
    bad.headers_mut().insert(
        "authorization",
        "Bearer wrong-token".parse().expect("header"),
    );
    let err = tokio_tungstenite::connect_async(bad)
        .await
        .expect_err("a wrong token must be rejected");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status(), 401);
            assert_eq!(
                resp.headers()["www-authenticate"],
                "Bearer error=\"invalid_token\""
            );
        }
        other => panic!("expected an HTTP 401 rejection, got {other:?}"),
    }

    server.abort();
}

/// Build a minimal timeline `LiveFrame` for publishing in tests.
fn sample_frame(account_id: Uuid) -> LiveFrame {
    LiveFrame::Timeline(LiveEvent {
        account_id,
        event_id: format!("$evt-{}:localhost", Uuid::new_v4()),
        room_id: "!room:localhost".to_owned(),
        sender: "@a:localhost".to_owned(),
        state_key: None,
        origin_ts: 1,
        event_type: "m.room.message".to_owned(),
        content: None,
        body: Some("hi".to_owned()),
        relates_to: None,
        sender_trust: None,
    })
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn ws_socket_closes_when_token_is_revoked() {
    let store = store().await;
    let (live, _rx) = broadcast::channel::<LiveFrame>(16);
    let stub = StubTokenVerifier::ok();
    // Grab the revocation handle before the stub is moved into the router.
    let revoke = stub.revocation_handle();
    let app = axon_api::router(
        AppState::new(
            store,
            live.clone(),
            Arc::new(StubSender::ok("$unused:localhost")),
            Arc::new(StubLifecycle::ok(Uuid::nil())),
            Arc::new(StubVerification::ok("$unused-flow")),
            Arc::new(StubTrust::ok()),
            Arc::new(stub),
            Arc::new(StubMediaProxy),
        )
        // Short cadence so the revocation is observed within the test, not 30s.
        .with_ws_revalidation_interval(Duration::from_millis(150)),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let url = format!("ws://{addr}/v1/ws");
    let (mut ws, _resp) = tokio_tungstenite::connect_async(authed_request(&url))
        .await
        .expect("ws connect");

    // The socket works initially: publish a frame and receive it.
    let account_id = Uuid::new_v4();
    live.send(sample_frame(account_id))
        .expect("a connected subscriber");
    let first = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("a frame within the timeout")
        .expect("stream still open")
        .expect("a websocket message");
    assert!(matches!(first, Message::Text(_)));

    // Revoke the token out from under the live socket, then publish another frame.
    // The client must observe the socket closing (revalidation tick fires within
    // ~150ms), not keep streaming frames.
    revoke.store(false, Ordering::SeqCst);
    let _ = live.send(sample_frame(account_id));

    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                // Closed by the server (Close frame), or the stream ended.
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break true,
                // A frame that raced ahead of the close — ignore and keep reading.
                Some(Ok(_)) => continue,
            }
        }
    })
    .await
    .expect("the revoked socket closes within the timeout");
    assert!(closed, "a revoked token must terminate the live socket");

    server.abort();
}
