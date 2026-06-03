//! The `/v1/ws` live-event WebSocket.
//!
//! A client opens one socket and receives every event the sync engine persists,
//! across all of this Axon's accounts, as it arrives. Each frame is a JSON
//! envelope `{ "type", "account_id", "payload" }` ([`WsEnvelope`]) — the same
//! `type`/`account_id`/`payload` shape used elsewhere on the wire, with the
//! payload for a timeline event being the read API's [`EventDto`].
//!
//! Delivery is **best-effort live tail**, not a replay: a client sees events
//! that arrive after it connects, and uses the HTTP read API for history. The
//! fan-out rides a [`tokio::sync::broadcast`] channel, so a client too slow to
//! keep up is told it lagged (and skips the backlog) rather than ever stalling
//! the sync engine. There is no auth yet — bearer-token validation on the
//! upgrade arrives with the rest of the API's auth (see ADR 0020).

use axon_core::LiveEvent;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use serde::Serialize;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::dto::EventDto;

/// The `type` tag for a live timeline event frame. Namespaced so later frame
/// kinds (e.g. interactive-verification events) extend the protocol without
/// colliding.
const TIMELINE_EVENT: &str = "timeline.event";

/// The wire envelope for every `/v1/ws` frame: a `type` discriminant, the
/// `account_id` the frame pertains to, and a type-specific `payload`.
#[derive(Debug, Serialize)]
struct WsEnvelope<T> {
    #[serde(rename = "type")]
    kind: &'static str,
    account_id: Uuid,
    payload: T,
}

/// `GET /v1/ws` — upgrade the connection and stream live events to the client.
pub async fn ws_handler(
    State(live): State<broadcast::Sender<LiveEvent>>,
    ws: WebSocketUpgrade,
) -> Response {
    // Subscribe before the upgrade completes so no event that arrives during the
    // handshake is missed.
    let rx = live.subscribe();
    ws.on_upgrade(move |socket| pump(socket, rx))
}

/// Forward live events to one connected client until either side hangs up.
async fn pump(mut socket: WebSocket, mut rx: broadcast::Receiver<LiveEvent>) {
    loop {
        tokio::select! {
            // A live event to push out.
            received = rx.recv() => match received {
                Ok(event) => {
                    let frame = WsEnvelope {
                        kind: TIMELINE_EVENT,
                        account_id: event.account_id,
                        payload: EventDto::from(event),
                    };
                    let text = match serde_json::to_string(&frame) {
                        Ok(text) => text,
                        Err(err) => {
                            // Serializing a JSON value can't realistically fail;
                            // log and skip rather than dropping the connection.
                            tracing::error!(error = %err, "failed to serialize live event frame");
                            continue;
                        }
                    };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        // The client is gone; stop pumping.
                        break;
                    }
                }
                // The client couldn't keep up and the channel overwrote unread
                // events. Skipping the backlog is the intended degradation for a
                // live tail — note it and keep the connection alive.
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "websocket client lagged; dropped live events");
                }
                // The sender was dropped (engine shutting down): no more events.
                Err(broadcast::error::RecvError::Closed) => break,
            },

            // Client -> server traffic. We accept no commands yet, but must read
            // the socket to observe a close and let axum answer control frames
            // (ping/pong) automatically.
            from_client = socket.recv() => match from_client {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Err(_)) => break,
                // Ignore any data frames a client sends; there's no protocol for
                // them yet.
                Some(Ok(_)) => {}
            },
        }
    }
}
