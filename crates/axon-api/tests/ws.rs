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

use std::sync::Arc;
use std::time::Duration;

use axon_api::AppState;
use axon_core::LiveEvent;
use axon_store::Store;
use common::StubSender;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn ws_streams_live_events() {
    let store = store().await;
    // We keep the sender so the test can publish; the router subscribes per
    // connection. Dropping the initial receiver means the connected handler is
    // the *only* subscriber, so `send` succeeding proves the socket is wired.
    let (live, _) = broadcast::channel::<LiveEvent>(16);
    let app = axon_api::router(AppState::new(
        store,
        live.clone(),
        Arc::new(StubSender::ok("$unused:localhost")),
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
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("ws connect");

    // Publish one live event. With the initial receiver dropped, a successful
    // send means exactly the connected handler received it.
    let account_id = Uuid::new_v4();
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    let receivers = live
        .send(LiveEvent {
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
        })
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

    ws.close(None).await.ok();
    server.abort();
}
