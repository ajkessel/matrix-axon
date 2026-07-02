//! DB-gated HTTP integration tests: drive the real router against Postgres.
//!
//! These seed a few rows through the `Store` API, then exercise the `/v1/`
//! handlers via `tower`'s `oneshot` and assert on status codes and the JSON
//! envelope. Like the store tests they need a database and are `#[ignore]`d by
//! default:
//!
//! ```sh
//! docker compose up -d postgres
//! # 5432 is the default; use your compose host port (e.g. 5433 if 5432 is taken).
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-api -- --ignored
//! ```
//!
//! Every `/v1/` route requires a bearer token (M7b); the router here is built
//! with a [`StubTokenVerifier`] that accepts [`TEST_TOKEN`], and the request
//! helpers attach it. The auth gate itself is exercised by the
//! `auth_gate_*` tests below.

mod common;

use std::sync::Arc;

use axon_api::{AccountLifecycle, AppState, MediaProxy};
use axon_store::{AccountState, NewEvent, RoomStateUpsert, Store};
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use common::{
    ConfiguredMediaProxy, DeleteOutcome, LoginCall, LoginOutcome, LogoutOutcome, MediaOutcome,
    RecoverOutcome, StubLifecycle, StubMediaProxy, StubSender, StubTokenVerifier, StubTrust,
    StubVerification, VerifyCall, VerifyOutcome, TEST_TOKEN,
};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

/// The default `Authorization` header value the helpers send.
fn bearer() -> String {
    format!("Bearer {TEST_TOKEN}")
}

/// Core request driver: optional JSON body, optional full `Authorization` header
/// value (the auth tests pass a wrong value or `None`). Returns `(status,
/// response headers, parsed body)`; the body is `Null` for empty responses
/// (e.g. a 204 or a pre-handler rejection).
async fn request_parts(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    auth: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(value) = auth {
        builder = builder.header("authorization", value);
    }
    let req = match &body {
        Some(value) => builder
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, headers, json)
}

/// As [`request_parts`], but returns the raw body bytes decoded as UTF-8 text.
/// Used for non-JSON responses such as the browser fallback page.
async fn request_text_parts(
    app: &axum::Router,
    method: &str,
    uri: &str,
    auth: Option<&str>,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(value) = auth {
        builder = builder.header("authorization", value);
    }
    let req = builder.body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = String::from_utf8(bytes.to_vec()).expect("utf-8 body");
    (status, headers, body)
}

/// As [`request_parts`], dropping the response headers — the common case.
async fn request(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let (status, _headers, json) = request_parts(app, method, uri, body, auth).await;
    (status, json)
}

/// Authenticated `GET`.
async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    request(app, "GET", uri, None, Some(&bearer())).await
}

async fn insert_message(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    ts: i64,
    body: &str,
) -> String {
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    let content = json!({ "msgtype": "m.text", "body": body });
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts: ts,
            event_type: "m.room.message",
            content: Some(content.clone()),
            raw_event: json!({ "type": "m.room.message", "content": content }),
            megolm_session_id: None,
            redacts: None,
            relates_to: None,
            decrypted_body_text: Some(body),
        })
        .await
        .expect("insert message");
    event_id
}

/// Insert an event carrying an explicit `relates_to` (and sender) — the shape the
/// M8 aggregation reads resolve over. The text body, if present, is lifted into
/// `decrypted_body_text` like the live ingestion path. Returns its event_id.
#[allow(clippy::too_many_arguments)]
async fn insert_relation(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    sender: &str,
    ts: i64,
    event_type: &str,
    content: Value,
    relates_to: Value,
) -> String {
    let event_id = format!("$rel-{}:localhost", Uuid::new_v4());
    let body = content
        .get("body")
        .and_then(|b| b.as_str())
        .map(str::to_owned);
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender,
            origin_ts: ts,
            event_type,
            content: Some(content.clone()),
            raw_event: json!({ "type": event_type, "content": content }),
            megolm_session_id: None,
            redacts: None,
            relates_to: Some(relates_to),
            decrypted_body_text: body.as_deref(),
        })
        .await
        .expect("insert relation");
    event_id
}

/// Redact `target` with an `m.room.redaction` event.
async fn insert_redaction(store: &Store, account_id: Uuid, room_id: &str, ts: i64, target: &str) {
    let event_id = format!("$red-{}:localhost", Uuid::new_v4());
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts: ts,
            event_type: "m.room.redaction",
            content: Some(json!({})),
            raw_event: json!({ "type": "m.room.redaction", "redacts": target }),
            megolm_session_id: None,
            redacts: Some(target),
            relates_to: None,
            decrypted_body_text: None,
        })
        .await
        .expect("insert redaction");
}

/// Build the read-API router over `store` with throwaway stubs for the ports the
/// read endpoints don't touch (sender, lifecycle, verify, trust, media).
fn read_app(store: Store) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
        None,
    ))
}

/// Build a router whose lifecycle routes are backed by `lifecycle`, gated by a
/// [`StubTokenVerifier`] that accepts [`TEST_TOKEN`]. The sender and live bus are
/// unused by these paths.
fn lifecycle_app(store: Store, lifecycle: Arc<dyn AccountLifecycle>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        lifecycle,
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
        None,
    ))
}

/// Build a router whose verify routes are backed by `verify` (other ports unused).
fn verify_app(store: Store, verify: Arc<dyn axon_api::VerificationService>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        verify,
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
        None,
    ))
}

/// Build a router whose verification-bundle route is backed by `trust` (other
/// ports unused).
fn trust_app(store: Store, trust: Arc<dyn axon_api::SenderTrustService>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        trust,
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
        None,
    ))
}

/// Build a router whose media route is backed by `media` (other ports unused).
fn media_app(store: Store, media: Arc<dyn MediaProxy>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        media,
        None,
    ))
}

async fn insert_media_event(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    mxc_url: &str,
) -> String {
    let event_id = format!("$media-{}:localhost", Uuid::new_v4());
    let content = json!({
        "msgtype": "m.image",
        "body": "image.png",
        "url": mxc_url
    });
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts: 1_700_000_000_000,
            event_type: "m.room.message",
            content: Some(content.clone()),
            raw_event: json!({ "type": "m.room.message", "content": content }),
            megolm_session_id: None,
            redacts: None,
            relates_to: None,
            decrypted_body_text: None,
        })
        .await
        .expect("insert media event");
    event_id
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn media_route_fails_closed_for_missing_and_redacted_metadata() {
    let store = store().await;
    let pool = store.pool().clone();
    let account_id = store
        .upsert_account(
            &format!("@media-closed-{}:localhost", Uuid::new_v4()),
            "https://hs.example.org",
        )
        .await
        .expect("account")
        .account_id;
    let room_id = format!("!media-closed-{}:localhost", Uuid::new_v4());
    let media = Arc::new(ConfiguredMediaProxy::ok(b"must not be returned"));
    let app = media_app(store.clone(), media.clone());

    let missing_mxc = format!("mxc://example.org/{}", Uuid::new_v4().simple());
    let missing_path = missing_mxc.trim_start_matches("mxc://");
    let (status, body) = get(&app, &format!("/v1/media/{account_id}/{missing_path}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    assert!(media.calls().is_empty(), "store miss must not reach proxy");

    let redacted_mxc = format!("mxc://example.org/{}", Uuid::new_v4().simple());
    let target = insert_media_event(&store, account_id, &room_id, &redacted_mxc).await;
    let redaction_id = format!("$redact-{}:localhost", Uuid::new_v4());
    store
        .upsert_event(&NewEvent {
            event_id: &redaction_id,
            room_id: &room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts: 1_700_000_000_001,
            event_type: "m.room.redaction",
            content: Some(json!({})),
            raw_event: json!({ "type": "m.room.redaction", "redacts": target }),
            megolm_session_id: None,
            redacts: Some(&target),
            relates_to: None,
            decrypted_body_text: None,
        })
        .await
        .expect("insert redaction");

    let redacted_path = redacted_mxc.trim_start_matches("mxc://");
    let (status, body) = get(&app, &format!("/v1/media/{account_id}/{redacted_path}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    assert!(
        media.calls().is_empty(),
        "redacted media must not reach proxy"
    );

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn media_route_preserves_forbidden_and_not_connected_statuses() {
    let store = store().await;
    let pool = store.pool().clone();
    let account_id = store
        .upsert_account(
            &format!("@media-errors-{}:localhost", Uuid::new_v4()),
            "https://hs.example.org",
        )
        .await
        .expect("account")
        .account_id;
    let room_id = format!("!media-errors-{}:localhost", Uuid::new_v4());
    let mxc_url = format!("mxc://example.org/{}", Uuid::new_v4().simple());
    insert_media_event(&store, account_id, &room_id, &mxc_url).await;
    let media_path = mxc_url.trim_start_matches("mxc://");
    let uri = format!("/v1/media/{account_id}/{media_path}");

    let forbidden = Arc::new(ConfiguredMediaProxy::failing(MediaOutcome::Forbidden(
        "media forbidden".to_owned(),
    )));
    let (status, body) = get(&media_app(store.clone(), forbidden), &uri).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "forbidden");

    let not_connected = Arc::new(ConfiguredMediaProxy::failing(MediaOutcome::NotConnected(
        "account unavailable".to_owned(),
    )));
    let (status, body) = get(&media_app(store.clone(), not_connected), &uri).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "service_unavailable");

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn read_api_end_to_end() {
    let store = store().await;
    let pool = store.pool().clone();
    let account_user_id = format!("@http-{}:localhost", Uuid::new_v4());
    let account_id = store
        .upsert_account(&account_user_id, "https://hs.example.org")
        .await
        .expect("account")
        .account_id;
    let room_id = format!("!http-{}:localhost", Uuid::new_v4());

    // Two messages; a name so the summary is populated.
    let e1 = insert_message(&store, account_id, &room_id, 1_000, "first").await;
    let e2 = insert_message(&store, account_id, &room_id, 2_000, "second").await;
    // A state event, used below to assert `state_key` is exposed on reads. Its
    // ts sits *below* both messages so it doesn't interleave with the message
    // pagination assertions — `room_timeline` includes state events, so a member
    // event between e1 and e2 would land in the limit-1 page 2 instead of e1.
    let member_event_id = format!("$member-{}:localhost", Uuid::new_v4());
    let member_content = json!({ "membership": "join", "displayname": "Alice" });
    store
        .upsert_event(&NewEvent {
            event_id: &member_event_id,
            room_id: &room_id,
            account_id,
            sender: "@jamie:localhost",
            origin_ts: 500,
            event_type: "m.room.member",
            content: Some(member_content.clone()),
            raw_event: json!({
                "type": "m.room.member",
                "state_key": "@alice:localhost",
                "content": member_content
            }),
            megolm_session_id: None,
            redacts: None,
            relates_to: None,
            decrypted_body_text: None,
        })
        .await
        .expect("insert membership event");
    store
        .upsert_room_state(&RoomStateUpsert {
            account_id,
            room_id: &room_id,
            event_type: "m.room.name",
            state_key: "",
            event_id: "$name:localhost",
            sender: "@alice:localhost",
            origin_ts: 1_500,
            content: Some(json!({ "name": "HTTP Room" })),
        })
        .await
        .expect("name");

    // The read endpoints don't touch the live-event bus or the message sender;
    // throwaway instances satisfy `AppState`.
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    let app = axon_api::router(AppState::new(
        store.clone(),
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
        None,
    ));

    // GET /v1/rooms?account_id= — our room is present with its name + latest event.
    let (status, body) = get(&app, &format!("/v1/rooms?account_id={account_id}")).await;
    assert_eq!(status, StatusCode::OK);
    let rooms = body["data"].as_array().expect("data array");
    let room = rooms
        .iter()
        .find(|r| r["room_id"] == room_id.as_str())
        .expect("our room present");
    assert_eq!(room["name"], "HTTP Room");
    assert_eq!(room["last_activity_ts"], 2_000);
    assert_eq!(room["last_event_id"], e2.as_str());
    assert_eq!(room["account_id"], account_id.to_string());
    assert_eq!(room["account_user_id"], account_user_id);

    // Timeline, page 1 (newest): limit 1 -> [e2] with a next_cursor.
    let base = format!("/v1/accounts/{account_id}/rooms/{room_id}/timeline");
    let (status, page1) = get(&app, &format!("{base}?limit=1")).await;
    assert_eq!(status, StatusCode::OK);
    let evs = page1["data"]["events"].as_array().expect("events");
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0]["event_id"], e2.as_str());
    assert_eq!(evs[0]["type"], "m.room.message");
    assert_eq!(evs[0]["body"], "second");
    let cursor = page1["data"]["next_cursor"].as_str().expect("next_cursor");

    // Page 2 via the cursor: [e1], and no overlap with page 1.
    let (status, page2) = get(&app, &format!("{base}?limit=1&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK);
    let evs2 = page2["data"]["events"].as_array().expect("events");
    assert_eq!(evs2.len(), 1);
    assert_eq!(evs2[0]["event_id"], e1.as_str());

    // A malformed cursor is a 400 with a bad_request code.
    let (status, err) = get(&app, &format!("{base}?cursor=not-a-cursor")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    // cursor and at_ts are mutually exclusive: supplying both is a 400, not a
    // silent preference for one over the other.
    let (status, err) = get(&app, &format!("{base}?cursor={cursor}&at_ts=0")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    // A non-UUID account_id is a 400 in the *envelope* (not axum's plain-text
    // rejection): once in a query filter, once in a path segment.
    let (status, err) = get(&app, "/v1/rooms?account_id=12345").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    let (status, err) = get(&app, "/v1/accounts/12345/events/$x:localhost").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    // GET single event -> 200 with content.
    let (status, ev) = get(&app, &format!("/v1/accounts/{account_id}/events/{e1}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ev["data"]["event_id"], e1.as_str());
    assert_eq!(ev["data"]["body"], "first");
    assert_eq!(ev["data"]["state_key"], Value::Null);
    assert_eq!(ev["data"]["redacted"], false);

    let (status, member) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/{member_event_id}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(member["data"]["state_key"], "@alice:localhost");
    assert_eq!(member["data"]["sender"], "@jamie:localhost");

    // Unknown event -> 404 with not_found code.
    let (status, err) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/$nope:localhost"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(err["error"]["code"], "not_found");

    // Clean up: delete the account, cascading to its events + room state, so the
    // seeded test rows don't leak into a real `/v1/rooms` against the same DB.
    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn accounts_read_api() {
    let store = store().await;
    let pool = store.pool().clone();

    // Three accounts in distinct lifecycle states. Unique user ids per run so the
    // assertions hold regardless of whatever else is in the shared test DB.
    let hs = "https://hs.example.org";
    let active_user = format!("@active-{}:localhost", Uuid::new_v4());
    let deactivated_user = format!("@deactivated-{}:localhost", Uuid::new_v4());
    let deleting_user = format!("@deleting-{}:localhost", Uuid::new_v4());

    let active = store
        .upsert_account(&active_user, hs)
        .await
        .expect("active");
    let deactivated = store
        .upsert_account(&deactivated_user, hs)
        .await
        .expect("deactivated");
    let deleting = store
        .upsert_account(&deleting_user, hs)
        .await
        .expect("deleting");
    store
        .set_account_state(deactivated.account_id, AccountState::Deactivated)
        .await
        .expect("deactivate");
    store
        .set_account_state(deleting.account_id, AccountState::Deleting)
        .await
        .expect("set deleting");

    let (live, _rx) = tokio::sync::broadcast::channel(16);
    let app = axon_api::router(AppState::new(
        store.clone(),
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        Arc::new(StubVerification::ok("$unused-flow")),
        Arc::new(StubTrust::ok()),
        Arc::new(StubTokenVerifier::ok()),
        Arc::new(StubMediaProxy),
        None,
    ));

    // GET /v1/accounts — the client-visible set: `active` and `deactivated` are
    // both listed (so a logged-out account is discoverable for re-login), but the
    // transient `deleting` teardown state is excluded.
    let (status, body) = get(&app, "/v1/accounts").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body["data"].as_array().expect("data array");
    let find = |id: Uuid| rows.iter().find(|a| a["account_id"] == id.to_string());

    let active_row = find(active.account_id).expect("active listed");
    assert_eq!(active_row["state"], "active");
    // `verified` now surfaces the persisted column (ADR 0026): a freshly upserted
    // account is unverified until a recover/verify derives otherwise, so it reads
    // a concrete `false` rather than the old always-`null` stub.
    assert_eq!(active_row["verified"], false);
    assert_eq!(active_row["user_id"], active_user);
    assert_eq!(active_row["homeserver_url"], hs);
    // The token is never exposed, under any key.
    assert!(active_row.get("access_token").is_none());
    assert!(active_row.get("access_token_encrypted").is_none());

    let deactivated_row = find(deactivated.account_id).expect("deactivated listed");
    assert_eq!(deactivated_row["state"], "deactivated");

    assert!(
        find(deleting.account_id).is_none(),
        "deleting account must not appear in the list"
    );

    // GET /v1/accounts/{id} — 200 for a known account, in whatever state. The
    // list omits `deleting`, but a by-id read is not filtered at all.
    let (status, one) = get(&app, &format!("/v1/accounts/{}", active.account_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["data"]["account_id"], active.account_id.to_string());
    assert_eq!(one["data"]["state"], "active");

    let (status, deactivated_by_id) =
        get(&app, &format!("/v1/accounts/{}", deactivated.account_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deactivated_by_id["data"]["state"], "deactivated");

    // The `deleting` account is absent from the list but still readable by id —
    // the by-id read is unfiltered.
    let (status, deleting_by_id) =
        get(&app, &format!("/v1/accounts/{}", deleting.account_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleting_by_id["data"]["state"], "deleting");

    // Unknown account id -> 404 with not_found code.
    let (status, err) = get(&app, &format!("/v1/accounts/{}", Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(err["error"]["code"], "not_found");

    // A non-UUID id -> 400 in the envelope (not axum's plain-text rejection).
    let (status, err) = get(&app, "/v1/accounts/not-a-uuid").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    for id in [
        active.account_id,
        deactivated.account_id,
        deleting.account_id,
    ] {
        sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("cleanup");
    }
}

// ---- Bearer-token auth gate (M7b) ----
//
// The gate is a single middleware layer over every `/v1/` route, so one route
// (login, backed by a stub that records calls) is representative: a missing or
// wrong token is rejected *before* the handler runs, and a read route is gated
// the same way.

#[tokio::test]
#[ignore = "requires Postgres"]
async fn auth_gate_rejects_missing_and_invalid_tokens_before_the_handler() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = lifecycle_app(store, stub.clone());
    let body = json!({
        "homeserver_url": "https://hs.example.org",
        "username": "@a:localhost",
        "password": "pw",
    });

    // No Authorization header → 401, and the lifecycle port is never invoked. A
    // missing credential gets the bare RFC 6750 `Bearer` challenge.
    let (status, headers, err) =
        request_parts(&app, "POST", "/v1/accounts/login", Some(body.clone()), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(err["error"]["code"], "unauthorized");
    assert_eq!(headers["www-authenticate"], "Bearer");

    // A wrong token → 401, still short-circuited before the handler. A present
    // but rejected token gets the `error="invalid_token"` challenge (§3.1).
    let (status, headers, err) = request_parts(
        &app,
        "POST",
        "/v1/accounts/login",
        Some(body),
        Some("Bearer not-the-test-token"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(err["error"]["code"], "unauthorized");
    assert_eq!(
        headers["www-authenticate"],
        "Bearer error=\"invalid_token\""
    );

    assert!(
        stub.calls().is_empty(),
        "the auth gate must short-circuit before the lifecycle port"
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn auth_gate_covers_read_routes_but_healthz_is_open() {
    let store = store().await;
    let app = lifecycle_app(store, Arc::new(StubLifecycle::ok(Uuid::nil())));

    // A plain read route is gated too: no token → 401 carrying the bearer challenge.
    let (status, headers, err) = request_parts(&app, "GET", "/v1/accounts", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(err["error"]["code"], "unauthorized");
    assert_eq!(headers["www-authenticate"], "Bearer");

    // The unversioned liveness probe carries no auth, so a monitor can reach it.
    let (status, _) = request(&app, "GET", "/healthz", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn browser_fallback_serves_html_for_root_and_non_api_misses() {
    let store = store().await;
    let app = read_app(store);

    for uri in ["/", "/not-a-route"] {
        let (status, headers, body) = request_text_parts(&app, "GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "uri: {uri}");
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert!(body.contains("Axon"), "uri: {uri}");
        assert!(
            body.contains("self-hosted Matrix agent and API server"),
            "uri: {uri}"
        );
        assert!(
            body.contains("No web interface is served at this address"),
            "uri: {uri}"
        );
        assert!(body.contains("axon-tui"), "uri: {uri}");
        assert!(body.contains("/healthz"), "uri: {uri}");
        assert!(body.contains("/v1/"), "uri: {uri}");
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn unknown_v1_paths_stay_in_the_api_boundary() {
    let store = store().await;
    let app = read_app(store);

    let (status, headers, err) = request_parts(&app, "GET", "/v1/not-a-route", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(err["error"]["code"], "unauthorized");
    assert_eq!(headers["www-authenticate"], "Bearer");

    let (status, body) = request(&app, "GET", "/v1/not-a-route", None, Some(&bearer())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(body["error"]["message"], "route not found");
}

// ---- Lifecycle: login / logout / delete / recover ----

#[tokio::test]
#[ignore = "requires Postgres"]
async fn login_succeeds_routes_to_port_and_envelopes_account() {
    let store = store().await;
    let pool = store.pool().clone();

    // Seed the account the stub will "log in" — the handler reads it back by id to
    // build the response, so it must exist. Unique id keeps the shared DB clean.
    let hs = "https://hs.example.org";
    let user = format!("@login-{}:localhost", Uuid::new_v4());
    let account = store.upsert_account(&user, hs).await.expect("seed");

    let stub = Arc::new(StubLifecycle::ok(account.account_id));
    let app = lifecycle_app(store.clone(), stub.clone());

    let (status, body) = request(
        &app,
        "POST",
        "/v1/accounts/login",
        Some(json!({ "homeserver_url": hs, "username": user, "password": "hunter2" })),
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["account_id"], account.account_id.to_string());
    assert_eq!(body["data"]["user_id"], user);
    // The password is never echoed back in the account view.
    assert!(body["data"].get("password").is_none());

    // The handler passed the decoded request straight through to the port.
    assert_eq!(
        stub.calls(),
        vec![LoginCall {
            homeserver_url: Some(hs.to_owned()),
            username: user.clone(),
            password: "hunter2".to_owned(),
        }]
    );

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account.account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn login_without_homeserver_url_forwards_none_for_discovery() {
    let store = store().await;
    let pool = store.pool().clone();

    let user = format!("@discover-{}:localhost", Uuid::new_v4());
    let account = store
        .upsert_account(&user, "https://hs.example.org")
        .await
        .expect("seed");

    let stub = Arc::new(StubLifecycle::ok(account.account_id));
    let app = lifecycle_app(store.clone(), stub.clone());

    // No `homeserver_url` in the body: the handler must accept the request and
    // forward `None` so the lifecycle backend performs discovery.
    let (status, body) = request(
        &app,
        "POST",
        "/v1/accounts/login",
        Some(json!({ "username": user, "password": "hunter2" })),
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["account_id"], account.account_id.to_string());
    assert_eq!(
        stub.calls(),
        vec![LoginCall {
            homeserver_url: None,
            username: user.clone(),
            password: "hunter2".to_owned(),
        }]
    );

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account.account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn login_malformed_body_is_enveloped_400_and_skips_port() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = lifecycle_app(store, stub.clone());

    // Missing the required `password` field → JSON decode failure, but only after
    // the auth gate has admitted the request.
    let (status, err) = request(
        &app,
        "POST",
        "/v1/accounts/login",
        Some(json!({ "homeserver_url": "https://hs.example.org", "username": "@a:localhost" })),
        Some(&bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");
    assert!(stub.calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn login_error_maps_to_status() {
    let store = store().await;
    let body = json!({
        "homeserver_url": "https://hs.example.org",
        "username": "@a:localhost",
        "password": "pw",
    });

    let cases = [
        (
            LoginOutcome::InvalidRequest("bad".into()),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            LoginOutcome::AuthFailed("nope".into()),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (
            LoginOutcome::Conflict("already".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            LoginOutcome::Upstream("hs down".into()),
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
        ),
        (
            LoginOutcome::Internal,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = lifecycle_app(store.clone(), Arc::new(StubLifecycle::failing(outcome)));
        let (status, err) = request(
            &app,
            "POST",
            "/v1/accounts/login",
            Some(body.clone()),
            Some(&bearer()),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn logout_succeeds_and_envelopes_deactivated_account() {
    let store = store().await;
    let pool = store.pool().clone();

    // Seed the account already `deactivated` to mirror the post-logout row the
    // handler reads back (the stubbed port doesn't touch the DB — the real
    // transition is covered by the axon-sync lifecycle tests).
    let hs = "https://hs.example.org";
    let user = format!("@logout-{}:localhost", Uuid::new_v4());
    let account = store.upsert_account(&user, hs).await.expect("seed");
    store
        .set_account_state(account.account_id, AccountState::Deactivated)
        .await
        .expect("deactivate");

    let stub = Arc::new(StubLifecycle::ok(account.account_id));
    let app = lifecycle_app(store.clone(), stub.clone());

    let (status, body) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{}/logout", account.account_id),
        None,
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["account_id"], account.account_id.to_string());
    assert_eq!(body["data"]["state"], "deactivated");
    // The handler routed the id straight through to the port.
    assert_eq!(stub.logout_calls(), vec![account.account_id]);

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account.account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn logout_error_maps_to_status() {
    let store = store().await;
    let id = Uuid::new_v4();

    let cases = [
        (
            LogoutOutcome::NotFound("nope".into()),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            LogoutOutcome::Conflict("deleting".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            LogoutOutcome::Internal,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = lifecycle_app(
            store.clone(),
            Arc::new(StubLifecycle::logout_failing(outcome)),
        );
        let (status, err) = request(
            &app,
            "POST",
            &format!("/v1/accounts/{id}/logout"),
            None,
            Some(&bearer()),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_succeeds_with_204_and_routes_to_port() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = lifecycle_app(store, stub.clone());

    let id = Uuid::new_v4();
    let (status, body) = request(
        &app,
        "DELETE",
        &format!("/v1/accounts/{id}"),
        None,
        Some(&bearer()),
    )
    .await;

    // 204 No Content — the resource is gone, nothing to return.
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    // The handler routed the id straight through to the port.
    assert_eq!(stub.delete_calls(), vec![id]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_error_maps_to_status() {
    let store = store().await;
    let id = Uuid::new_v4();

    let cases = [
        (
            DeleteOutcome::NotFound("nope".into()),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            DeleteOutcome::Conflict("draining".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            DeleteOutcome::Internal,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = lifecycle_app(
            store.clone(),
            Arc::new(StubLifecycle::delete_failing(outcome)),
        );
        let (status, err) = request(
            &app,
            "DELETE",
            &format!("/v1/accounts/{id}"),
            None,
            Some(&bearer()),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn recover_succeeds_and_envelopes_account_with_verified() {
    let store = store().await;
    let pool = store.pool().clone();

    // Seed the account `active` (recover requires it) and mark it verified to
    // mirror the post-recover row the handler reads back — the stubbed port
    // doesn't touch the DB, so the real cross-signing + flag write are covered by
    // the axon-sync lifecycle tests; here we assert the read-back is enveloped.
    let hs = "https://hs.example.org";
    let user = format!("@recover-{}:localhost", Uuid::new_v4());
    let account = store.upsert_account(&user, hs).await.expect("seed");
    store
        .set_account_verified(account.account_id, true)
        .await
        .expect("verify");

    let stub = Arc::new(StubLifecycle::ok(account.account_id));
    let app = lifecycle_app(store.clone(), stub.clone());

    let (status, resp) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{}/recover", account.account_id),
        Some(json!({ "recovery_key": "EsTc SomeRecoveryKey" })),
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(resp["data"]["account_id"], account.account_id.to_string());
    assert_eq!(resp["data"]["verified"], true);
    // The handler forwarded the id + recovery key straight to the port.
    assert_eq!(
        stub.recover_calls(),
        vec![(account.account_id, "EsTc SomeRecoveryKey".to_string())]
    );

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account.account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn recover_malformed_body_is_enveloped_400_and_skips_port() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = lifecycle_app(store, stub.clone());

    // A valid token so the request reaches body decoding (missing `recovery_key`),
    // proving the 400 is decode-side, not the auth gate.
    let (status, _) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{}/recover", Uuid::new_v4()),
        Some(json!({})),
        Some(&bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(stub.recover_calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn recover_error_maps_to_status() {
    let store = store().await;
    let id = Uuid::new_v4();
    let body = json!({ "recovery_key": "k" });

    let cases = [
        (
            RecoverOutcome::NotFound("nope".into()),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            RecoverOutcome::Conflict("not active".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            RecoverOutcome::BadRequest("bad key".into()),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            RecoverOutcome::Internal,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = lifecycle_app(
            store.clone(),
            Arc::new(StubLifecycle::recover_failing(outcome)),
        );
        let (status, err) = request(
            &app,
            "POST",
            &format!("/v1/accounts/{id}/recover"),
            Some(body.clone()),
            Some(&bearer()),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

// ---- Interactive SAS verification (M7a PR6) ----

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_start_succeeds_and_returns_flow_id() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-abc"));
    let app = verify_app(store, verify.clone());

    let account_id = Uuid::new_v4();
    let (status, body) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/verify"),
        Some(json!({ "device_id": "TRUSTEDDEV" })),
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["flow_id"], "$flow-abc");
    // The handler forwarded the id + device straight to the port.
    assert_eq!(
        verify.calls(),
        vec![VerifyCall::Start {
            account_id,
            user_id: None,
            device_id: Some("TRUSTEDDEV".to_owned()),
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_start_rejects_ambiguous_target_before_service() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-unused"));
    let app = verify_app(store, verify.clone());

    let account_id = Uuid::new_v4();
    let (status, body) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/verify"),
        Some(json!({ "user_id": "@peer:localhost", "device_id": "TRUSTEDDEV" })),
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("exactly one verification target"));
    assert!(verify.calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_start_rejects_missing_target_before_service() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-unused"));
    let app = verify_app(store, verify.clone());

    let account_id = Uuid::new_v4();
    let (status, body) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/verify"),
        Some(json!({})),
        Some(&bearer()),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("neither a device_id nor a user_id"));
    assert!(verify.calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_get_returns_flow_state_dto() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-xyz"));
    let app = verify_app(store, verify.clone());
    let account_id = Uuid::new_v4();

    let (status, body) = get(&app, &format!("/v1/accounts/{account_id}/verify/$flow-xyz")).await;

    assert_eq!(status, StatusCode::OK);
    let flow = &body["data"];
    assert_eq!(flow["flow_id"], "$flow-xyz");
    assert_eq!(flow["device_id"], "TRUSTEDDEV");
    assert_eq!(flow["stage"], "keys_exchanged");
    assert_eq!(flow["emoji"][0]["symbol"], "🐶");
    assert_eq!(flow["emoji"][0]["description"], "Dog");
    assert_eq!(flow["decimals"][0], 1234);
    assert_eq!(flow["decimals"][2], 9012);
    assert!(flow["cancel_reason"].is_null());

    assert_eq!(
        verify.calls(),
        vec![VerifyCall::Get {
            account_id,
            flow_id: "$flow-xyz".to_owned(),
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_list_returns_flows() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-1"));
    let app = verify_app(store, verify.clone());
    let account_id = Uuid::new_v4();

    let (status, body) = get(&app, &format!("/v1/accounts/{account_id}/verify")).await;

    assert_eq!(status, StatusCode::OK);
    let flows = body["data"].as_array().expect("data array");
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0]["flow_id"], "$flow-1");
    assert_eq!(verify.calls(), vec![VerifyCall::List { account_id }]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_confirm_and_cancel_return_204_and_route() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-2"));
    let app = verify_app(store, verify.clone());
    let account_id = Uuid::new_v4();

    let (status, _) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/verify/$flow-2/confirm"),
        None,
        Some(&bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = request(
        &app,
        "POST",
        &format!("/v1/accounts/{account_id}/verify/$flow-2/cancel"),
        None,
        Some(&bearer()),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(
        verify.calls(),
        vec![
            VerifyCall::Confirm {
                account_id,
                flow_id: "$flow-2".to_owned(),
            },
            VerifyCall::Cancel {
                account_id,
                flow_id: "$flow-2".to_owned(),
            },
        ]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_error_maps_to_status() {
    let store = store().await;
    let account_id = Uuid::new_v4();
    let body = json!({ "device_id": "D" });

    let cases = [
        (
            VerifyOutcome::NotFound("nope".into()),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            VerifyOutcome::NotActive("logged out".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            VerifyOutcome::Conflict("wrong stage".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            VerifyOutcome::BadRequest("unknown device".into()),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            VerifyOutcome::Upstream("hs down".into()),
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
        ),
        (
            VerifyOutcome::Internal,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];

    for (outcome, want_status, want_code) in cases {
        let app = verify_app(store.clone(), Arc::new(StubVerification::failing(outcome)));
        let (status, err) = request(
            &app,
            "POST",
            &format!("/v1/accounts/{account_id}/verify"),
            Some(body.clone()),
            Some(&bearer()),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

// ---- Per-event verification bundle (M7c) ----

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verification_bundle_returns_snapshot_and_current() {
    let store = store().await;
    let app = trust_app(store, Arc::new(StubTrust::ok()));
    let account_id = Uuid::new_v4();

    let (status, body) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/$evt:localhost/verification"),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["event_id"], "$evt:localhost");
    assert_eq!(body["data"]["sender"], "@bob:localhost");
    // The at-decrypt snapshot half.
    assert_eq!(body["data"]["snapshot"]["sender_trust"], "verified");
    assert_eq!(body["data"]["snapshot"]["device_id"], "BOBDEVICE");
    // Megolm session provenance rides the snapshot half.
    assert_eq!(body["data"]["snapshot"]["session_id"], "session-1");
    assert_eq!(body["data"]["snapshot"]["forwarded"], false);
    // The live-evidence half — separate from the snapshot.
    assert_eq!(body["data"]["current"]["device_cross_signed"], true);
    assert_eq!(body["data"]["current"]["identity_verified"], true);
    assert_eq!(body["data"]["current"]["verification_violation"], false);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verification_bundle_error_maps_to_status() {
    let store = store().await;
    let account_id = Uuid::new_v4();

    let cases = [
        (
            (|| axon_api::TrustError::NotFound("no event".into())) as fn() -> axon_api::TrustError,
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            || axon_api::TrustError::NotActive("logged out".into()),
            StatusCode::CONFLICT,
            "conflict",
        ),
        (
            || axon_api::TrustError::Upstream("hs down".into()),
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
        ),
        (
            || axon_api::TrustError::Internal,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        ),
    ];

    for (make_err, want_status, want_code) in cases {
        let app = trust_app(store.clone(), Arc::new(StubTrust::failing(make_err)));
        let (status, err) = get(
            &app,
            &format!("/v1/accounts/{account_id}/events/$evt:localhost/verification"),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

/// M8b: the relation-aggregation read endpoints resolve over relations that land
/// far outside the default timeline window, and the timeline read serves the
/// collapsed/edited view. Exercises reactions (tally, dedup, `me`, redaction),
/// the collapsed timeline (edited body in place, no stray edit rows), the edits
/// trail, replies, and threads + a thread-scoped paginated timeline.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn aggregation_api_end_to_end() {
    let store = store().await;
    let pool = store.pool().clone();
    let me_user = format!("@agg-{}:localhost", Uuid::new_v4());
    let account_id = store
        .upsert_account(&me_user, "https://hs.example.org")
        .await
        .expect("account")
        .account_id;
    let room_id = format!("!agg-{}:localhost", Uuid::new_v4());

    // The base message at ts=1000; every relation below lands *far later* (ts 2k–9k)
    // so a naive client window over the newest page would miss them.
    let msg = insert_message(&store, account_id, &room_id, 1_000, "hello").await;

    // --- Reactions: 👍 from alice (twice → dedup) + from me; ❤️ from bob; a 👎
    // from carol that gets redacted (drops from the tally, leaving the rest). ---
    let react = |sender: &'static str, ts: i64, target: String, key: &'static str| {
        let store = store.clone();
        let room_id = room_id.clone();
        async move {
            insert_relation(
                &store,
                account_id,
                &room_id,
                sender,
                ts,
                "m.reaction",
                json!({}),
                json!({ "rel_type": "m.annotation", "event_id": target, "key": key }),
            )
            .await
        }
    };
    react("@alice:localhost", 9_000, msg.clone(), "👍").await;
    react("@alice:localhost", 9_001, msg.clone(), "👍").await; // duplicate (sender,key)
    let my_thumb = insert_relation(
        &store,
        account_id,
        &room_id,
        &me_user,
        9_002,
        "m.reaction",
        json!({}),
        json!({ "rel_type": "m.annotation", "event_id": msg.clone(), "key": "👍" }),
    )
    .await;
    react("@bob:localhost", 9_003, msg.clone(), "❤️").await;
    let downvote = react("@carol:localhost", 9_004, msg.clone(), "👎").await;
    insert_redaction(&store, account_id, &room_id, 9_005, &downvote).await;

    let app = read_app(store.clone());

    let (status, body) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/{msg}/reactions"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tally = &body["data"];
    assert_eq!(tally["👍"]["count"], 2, "alice + me, the dup counts once");
    assert_eq!(tally["👍"]["me"], true);
    let senders = tally["👍"]["senders"].as_array().expect("senders");
    assert!(senders.iter().any(|s| s == "@alice:localhost"));
    assert!(senders.iter().any(|s| s == me_user.as_str()));
    assert_eq!(tally["❤️"]["count"], 1);
    assert_eq!(tally["❤️"]["me"], false);
    assert!(tally.get("👎").is_none(), "redacted reaction drops out");
    // my_event_ids carries the account user's own reaction event(s) for the key —
    // the ids a client redacts to unreact — and is empty for keys we didn't send.
    let mine = tally["👍"]["my_event_ids"]
        .as_array()
        .expect("my_event_ids");
    assert_eq!(mine.len(), 1, "only my own 👍 reaction event");
    assert_eq!(mine[0], my_thumb.as_str());
    assert_eq!(
        tally["❤️"]["my_event_ids"],
        json!([]),
        "no own reaction event for a key we didn't send"
    );

    // An event with no reactions → empty object, not a 404.
    let (status, body) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/$nobody:localhost/reactions"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!({}));

    // --- Edits: two valid edits by the original sender (latest wins) + one from a
    // non-sender (ignored by the collapse, but present in the forensic trail). ---
    let edit = |sender: &'static str, ts: i64, new_body: &'static str| {
        let store = store.clone();
        let room_id = room_id.clone();
        let target = msg.clone();
        async move {
            insert_relation(
                &store,
                account_id,
                &room_id,
                sender,
                ts,
                "m.room.message",
                json!({
                    "msgtype": "m.text",
                    "body": format!("* {new_body}"),
                    "m.new_content": { "msgtype": "m.text", "body": new_body },
                }),
                json!({ "rel_type": "m.replace", "event_id": target }),
            )
            .await
        }
    };
    edit("@alice:localhost", 2_000, "hello (v2)").await;
    edit("@alice:localhost", 3_000, "hello (v3)").await; // latest valid edit
    edit("@mallory:localhost", 4_000, "PWNED").await; // non-sender → not honored

    // Timeline serves the collapsed/edited view: msg's body is the latest edit,
    // `edited`/`edit_count` are set, and no standalone m.replace rows appear.
    let (status, page) = get(
        &app,
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/timeline"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events = page["data"]["events"].as_array().expect("events");
    let m = events
        .iter()
        .find(|e| e["event_id"] == msg.as_str())
        .expect("base message present in timeline");
    assert_eq!(m["body"], "hello (v3)", "non-sender edit must not win");
    assert_eq!(m["edited"], true);
    assert_eq!(m["edit_count"], 2, "two valid edits; mallory's is excluded");
    assert!(
        events
            .iter()
            .all(|e| e["relates_to"]["rel_type"] != "m.replace"),
        "standalone edit rows must be collapsed out of the timeline"
    );

    // The forensic edits trail is unfiltered (includes the non-sender edit), oldest
    // first.
    let (status, body) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/{msg}/edits"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let edits = body["data"].as_array().expect("edits");
    assert_eq!(edits.len(), 3, "two valid + one non-sender, all preserved");
    assert_eq!(edits[0]["body"], "* hello (v2)", "oldest first");

    // --- Replies: a plain reply (no rel_type) is found via nested m.in_reply_to;
    // thread members (below) must not bleed into it. ---
    let reply = insert_relation(
        &store,
        account_id,
        &room_id,
        "@bob:localhost",
        5_000,
        "m.room.message",
        json!({ "msgtype": "m.text", "body": "a reply" }),
        json!({ "m.in_reply_to": { "event_id": msg.clone() } }),
    )
    .await;
    let (status, body) = get(
        &app,
        &format!("/v1/accounts/{account_id}/events/{msg}/replies"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let replies = body["data"].as_array().expect("replies");
    assert_eq!(replies.len(), 1, "only the plain reply, not thread members");
    assert_eq!(replies[0]["event_id"], reply.as_str());
    assert_eq!(replies[0]["body"], "a reply");

    // --- Threads: two thread replies rooted at msg → one thread, reply_count 2,
    // latest is the newest; the thread timeline pages reverse-chronologically. ---
    let thread = |ts: i64, body: &'static str| {
        let store = store.clone();
        let room_id = room_id.clone();
        let root = msg.clone();
        async move {
            insert_relation(
                &store,
                account_id,
                &room_id,
                "@bob:localhost",
                ts,
                "m.room.message",
                json!({ "msgtype": "m.text", "body": body }),
                json!({
                    "rel_type": "m.thread",
                    "event_id": root,
                    "m.in_reply_to": { "event_id": root, "is_falling_back": true },
                }),
            )
            .await
        }
    };
    let t1 = thread(6_000, "thread 1").await;
    let t2 = thread(6_001, "thread 2").await;

    let (status, body) = get(
        &app,
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/threads"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let threads = body["data"].as_array().expect("threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["root_event_id"], msg.as_str());
    assert_eq!(threads[0]["reply_count"], 2);
    assert_eq!(threads[0]["latest_reply_event_id"], t2.as_str());
    assert_eq!(threads[0]["latest_reply_ts"], 6_001);

    // Thread-scoped timeline, paginated: page 1 (limit 1) → [t2] + cursor;
    // page 2 → [t1]; only thread members appear, newest first.
    let base = format!("/v1/accounts/{account_id}/rooms/{room_id}/threads/{msg}/timeline");
    let (status, page1) = get(&app, &format!("{base}?limit=1")).await;
    assert_eq!(status, StatusCode::OK);
    let evs = page1["data"]["events"].as_array().expect("events");
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0]["event_id"], t2.as_str());
    let cursor = page1["data"]["next_cursor"].as_str().expect("next_cursor");

    let (status, page2) = get(&app, &format!("{base}?limit=1&cursor={cursor}")).await;
    assert_eq!(status, StatusCode::OK);
    let evs2 = page2["data"]["events"].as_array().expect("events");
    assert_eq!(evs2.len(), 1);
    assert_eq!(evs2[0]["event_id"], t1.as_str());

    // An unknown thread root → an empty page, not a 404.
    let (status, empty) = get(
        &app,
        &format!("/v1/accounts/{account_id}/rooms/{room_id}/threads/$nope:localhost/timeline"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["data"]["events"].as_array().expect("events").len(), 0);

    // A malformed cursor on the thread timeline is a 400, like the room timeline.
    let (status, err) = get(&app, &format!("{base}?cursor=not-a-cursor")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(err["error"]["code"], "bad_request");

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}

/// Regression for the unreact round-trip after relation aggregation (Adam's M8b
/// review, finding 1): the collapsed timeline strips raw `m.reaction` rows, so a
/// client can no longer recover its own reaction event ids by scanning events. It
/// must instead read them from the aggregated tally's `my_event_ids`. This walks
/// react → reload the timeline → withdraw (redact the id from `my_event_ids`) →
/// reload again and confirm the reaction is gone.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn unreact_uses_aggregated_reaction_ids_from_timeline() {
    let store = store().await;
    let pool = store.pool().clone();
    let me_user = format!("@unreact-{}:localhost", Uuid::new_v4());
    let account_id = store
        .upsert_account(&me_user, "https://hs.example.org")
        .await
        .expect("account")
        .account_id;
    let room_id = format!("!unreact-{}:localhost", Uuid::new_v4());

    let msg = insert_message(&store, account_id, &room_id, 1_000, "hello").await;
    // I react with 👍; the raw reaction lands far later than the message.
    let my_reaction = insert_relation(
        &store,
        account_id,
        &room_id,
        &me_user,
        9_000,
        "m.reaction",
        json!({}),
        json!({ "rel_type": "m.annotation", "event_id": msg.clone(), "key": "👍" }),
    )
    .await;

    let app = read_app(store.clone());
    let timeline = format!("/v1/accounts/{account_id}/rooms/{room_id}/timeline");

    // Reload the timeline: the raw reaction row is gone, but the aggregated tally
    // on the message carries my_event_ids — the id a client redacts to unreact.
    let (status, page) = get(&app, &timeline).await;
    assert_eq!(status, StatusCode::OK);
    let events = page["data"]["events"].as_array().expect("events");
    assert!(
        !events.iter().any(|e| e["event_id"] == my_reaction.as_str()),
        "raw m.reaction must not appear in the collapsed timeline"
    );
    let m = events
        .iter()
        .find(|e| e["event_id"] == msg.as_str())
        .expect("base message present");
    assert_eq!(m["reactions"]["👍"]["count"], 1);
    assert_eq!(m["reactions"]["👍"]["me"], true);
    let mine = m["reactions"]["👍"]["my_event_ids"]
        .as_array()
        .expect("my_event_ids");
    assert_eq!(mine, &[json!(my_reaction)], "the id to redact to unreact");

    // Withdraw: redact exactly the id the client read from my_event_ids.
    insert_redaction(&store, account_id, &room_id, 9_001, &my_reaction).await;

    // Reload again: the reaction is gone from the message's tally entirely.
    let (status, page) = get(&app, &timeline).await;
    assert_eq!(status, StatusCode::OK);
    let m = page["data"]["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["event_id"] == msg.as_str())
        .expect("base message present")
        .clone();
    assert!(
        m["reactions"].is_null() || m["reactions"].get("👍").is_none(),
        "withdrawn reaction drops from the tally"
    );

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account_id)
        .execute(&pool)
        .await
        .expect("cleanup");
}
