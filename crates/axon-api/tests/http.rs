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

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use axon_api::{AccountLifecycle, AppState};
use axon_store::{AccountState, NewEvent, RoomStateUpsert, Store};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::{LoginCall, LoginOutcome, StubLifecycle, StubSender};
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`
use uuid::Uuid;

async fn store() -> Store {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests");
    Store::connect(&url, 5).await.expect("connect + migrate")
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .expect("request");
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
    // `verified` is `null` (unknown) until derivation lands, not a bare `false`.
    assert!(active_row["verified"].is_null());
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

/// Build a router whose login route is backed by `lifecycle`. The sender and live
/// bus are unused by the login path.
fn login_app(store: Store, lifecycle: Arc<dyn AccountLifecycle>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        lifecycle,
    ))
}

/// `POST /v1/accounts/login` with an optional peer address (installed as the
/// `ConnectInfo<SocketAddr>` extension the loopback guard reads, mirroring
/// `into_make_service_with_connect_info`). Returns `(status, parsed body)`.
async fn post_login(
    app: &axum::Router,
    peer: Option<SocketAddr>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/accounts/login")
        .header("content-type", "application/json");
    if let Some(addr) = peer {
        builder = builder.extension(ConnectInfo(addr));
    }
    let req = builder.body(Body::from(body.to_string())).unwrap();
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
async fn login_succeeds_routes_to_port_and_envelopes_account() {
    let store = store().await;
    let pool = store.pool().clone();

    // Seed the account the stub will "log in" — the handler reads it back by id to
    // build the response, so it must exist. Unique id keeps the shared DB clean.
    let hs = "https://hs.example.org";
    let user = format!("@login-{}:localhost", Uuid::new_v4());
    let account = store.upsert_account(&user, hs).await.expect("seed");

    let stub = Arc::new(StubLifecycle::ok(account.account_id));
    let app = login_app(store.clone(), stub.clone());

    let loopback = Some("127.0.0.1:54321".parse().unwrap());
    let (status, body) = post_login(
        &app,
        loopback,
        json!({ "homeserver_url": hs, "username": user, "password": "hunter2" }),
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
            homeserver_url: hs.to_owned(),
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
async fn login_requires_loopback_peer() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store, stub.clone());
    let body = json!({
        "homeserver_url": "https://hs.example.org",
        "username": "@a:localhost",
        "password": "pw",
    });

    // A non-loopback peer is rejected before the handler runs.
    let off_box = Some("203.0.113.7:443".parse().unwrap());
    let (status, err) = post_login(&app, off_box, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["error"]["code"], "forbidden");

    // No peer address at all also fails closed.
    let (status, _) = post_login(&app, None, body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The guard short-circuited both times: the lifecycle port was never invoked.
    assert!(stub.calls().is_empty());

    // A loopback peer passes the guard through to the (stubbed) port. The id is
    // nil and not seeded, so the read-back 404s into a 500 — but the guard let it
    // through, which is what this asserts.
    let loopback = Some("127.0.0.1:12000".parse().unwrap());
    let (status, _) = post_login(&app, loopback, body).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(stub.calls().len(), 1);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn login_malformed_body_is_enveloped_400_and_skips_port() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store, stub.clone());

    // Missing the required `password` field → JSON decode failure, but only after
    // the loopback guard has admitted the request.
    let loopback = Some("127.0.0.1:13000".parse().unwrap());
    let (status, err) = post_login(
        &app,
        loopback,
        json!({ "homeserver_url": "https://hs.example.org", "username": "@a:localhost" }),
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
    let loopback: Option<SocketAddr> = Some("127.0.0.1:14000".parse().unwrap());
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
        let app = login_app(store.clone(), Arc::new(StubLifecycle::failing(outcome)));
        let (status, err) = post_login(&app, loopback, body.clone()).await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}
