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
use common::{
    DeleteOutcome, LoginCall, LoginOutcome, LogoutOutcome, RecoverOutcome, StubLifecycle,
    StubSender, StubVerification, VerifyCall, VerifyOutcome,
};
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
        Arc::new(StubVerification::ok("$unused-flow")),
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
        Arc::new(StubVerification::ok("$unused-flow")),
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

/// Build a router whose login route is backed by `lifecycle`. The sender and live
/// bus are unused by the login path.
fn login_app(store: Store, lifecycle: Arc<dyn AccountLifecycle>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        lifecycle,
        Arc::new(StubVerification::ok("$unused-flow")),
    ))
}

/// Build a router whose verify routes are backed by `verify`. The other ports are
/// unused by the verification path (the handlers delegate straight to the port and
/// never touch the store).
fn verify_app(store: Store, verify: Arc<dyn axon_api::VerificationService>) -> axum::Router {
    let (live, _rx) = tokio::sync::broadcast::channel(16);
    axon_api::router(AppState::new(
        store,
        live,
        Arc::new(StubSender::ok("$unused:localhost")),
        Arc::new(StubLifecycle::ok(Uuid::nil())),
        verify,
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
    let app = login_app(store.clone(), stub.clone());

    // No `homeserver_url` in the body: the handler must accept the request and
    // forward `None` so the lifecycle backend performs discovery.
    let loopback = Some("127.0.0.1:54322".parse().unwrap());
    let (status, body) = post_login(
        &app,
        loopback,
        json!({ "username": user, "password": "hunter2" }),
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

/// `POST /v1/accounts/{account_id}/logout` with an optional peer address (the
/// `ConnectInfo<SocketAddr>` the loopback guard reads). Returns `(status, body)`.
async fn post_logout(
    app: &axum::Router,
    peer: Option<SocketAddr>,
    account_id: Uuid,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/accounts/{account_id}/logout"));
    if let Some(addr) = peer {
        builder = builder.extension(ConnectInfo(addr));
    }
    let req = builder.body(Body::empty()).unwrap();
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
    let app = login_app(store.clone(), stub.clone());

    let loopback = Some("127.0.0.1:54300".parse().unwrap());
    let (status, body) = post_logout(&app, loopback, account.account_id).await;

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
async fn logout_requires_loopback_peer() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store, stub.clone());

    // Non-loopback and missing-peer both fail closed before the handler runs.
    let off_box = Some("203.0.113.7:443".parse().unwrap());
    let (status, err) = post_logout(&app, off_box, Uuid::new_v4()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["error"]["code"], "forbidden");

    let (status, _) = post_logout(&app, None, Uuid::new_v4()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The guard short-circuited: the lifecycle port was never invoked.
    assert!(stub.logout_calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn logout_error_maps_to_status() {
    let store = store().await;
    let loopback: Option<SocketAddr> = Some("127.0.0.1:14100".parse().unwrap());
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
        let app = login_app(
            store.clone(),
            Arc::new(StubLifecycle::logout_failing(outcome)),
        );
        let (status, err) = post_logout(&app, loopback, id).await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

/// `DELETE /v1/accounts/{account_id}` with an optional peer address. Returns
/// `(status, parsed body)` — the body is `Null` for the 204 success path.
async fn delete_account(
    app: &axum::Router,
    peer: Option<SocketAddr>,
    account_id: Uuid,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/accounts/{account_id}"));
    if let Some(addr) = peer {
        builder = builder.extension(ConnectInfo(addr));
    }
    let req = builder.body(Body::empty()).unwrap();
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
async fn delete_succeeds_with_204_and_routes_to_port() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store, stub.clone());

    let id = Uuid::new_v4();
    let loopback = Some("127.0.0.1:54400".parse().unwrap());
    let (status, body) = delete_account(&app, loopback, id).await;

    // 204 No Content — the resource is gone, nothing to return.
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    // The handler routed the id straight through to the port.
    assert_eq!(stub.delete_calls(), vec![id]);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_requires_loopback_but_get_on_same_path_stays_open() {
    let store = store().await;
    // Seed a real account so the open GET on the same path can read it back.
    let user = format!("@del-http-{}:localhost", Uuid::new_v4());
    let account = store
        .upsert_account(&user, "https://hs.example.org")
        .await
        .expect("seed");
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store.clone(), stub.clone());

    // DELETE from a non-loopback peer is rejected before the handler runs.
    let off_box = Some("203.0.113.7:443".parse().unwrap());
    let (status, err) = delete_account(&app, off_box, account.account_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["error"]["code"], "forbidden");
    // Missing peer fails closed too.
    let (status, _) = delete_account(&app, None, account.account_id).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The guard short-circuited: the lifecycle port was never invoked.
    assert!(stub.delete_calls().is_empty());

    // The sibling GET on the *same* path carries no loopback layer — a plain read
    // (no peer extension) still succeeds, proving the guard is DELETE-only.
    let (status, body) = get(&app, &format!("/v1/accounts/{}", account.account_id)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["account_id"], account.account_id.to_string());

    sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account.account_id)
        .execute(store.pool())
        .await
        .expect("cleanup");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_error_maps_to_status() {
    let store = store().await;
    let loopback: Option<SocketAddr> = Some("127.0.0.1:14200".parse().unwrap());
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
        let app = login_app(
            store.clone(),
            Arc::new(StubLifecycle::delete_failing(outcome)),
        );
        let (status, err) = delete_account(&app, loopback, id).await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

/// `POST /v1/accounts/{account_id}/recover` with an optional peer address and a
/// JSON body. Returns `(status, body)`.
async fn post_recover(
    app: &axum::Router,
    peer: Option<SocketAddr>,
    account_id: Uuid,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/accounts/{account_id}/recover"))
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
    let app = login_app(store.clone(), stub.clone());

    let loopback = Some("127.0.0.1:54500".parse().unwrap());
    let body = json!({ "recovery_key": "EsTc SomeRecoveryKey" });
    let (status, resp) = post_recover(&app, loopback, account.account_id, body).await;

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
async fn recover_requires_loopback_peer() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store, stub.clone());
    let body = json!({ "recovery_key": "k" });

    // Non-loopback and missing-peer both fail closed before the handler runs.
    let off_box = Some("203.0.113.7:443".parse().unwrap());
    let (status, err) = post_recover(&app, off_box, Uuid::new_v4(), body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["error"]["code"], "forbidden");

    let (status, _) = post_recover(&app, None, Uuid::new_v4(), body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The guard short-circuited: the lifecycle port was never invoked.
    assert!(stub.recover_calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn recover_malformed_body_is_enveloped_400_and_skips_port() {
    let store = store().await;
    let stub = Arc::new(StubLifecycle::ok(Uuid::nil()));
    let app = login_app(store, stub.clone());

    // A loopback peer so the request reaches body decoding (missing
    // `recovery_key`), proving the 400 is decode-side, not the guard.
    let loopback = Some("127.0.0.1:13500".parse().unwrap());
    let (status, _) = post_recover(&app, loopback, Uuid::new_v4(), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(stub.recover_calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn recover_error_maps_to_status() {
    let store = store().await;
    let loopback: Option<SocketAddr> = Some("127.0.0.1:14300".parse().unwrap());
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
        let app = login_app(
            store.clone(),
            Arc::new(StubLifecycle::recover_failing(outcome)),
        );
        let (status, err) = post_recover(&app, loopback, id, body.clone()).await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}

// ---- Interactive SAS verification (M7a PR6) ----

/// `POST` to a verify route with an optional peer address (the
/// `ConnectInfo<SocketAddr>` the loopback guard reads) and an optional JSON body
/// (start carries one; confirm/cancel don't). Returns `(status, parsed body)`.
async fn post_verify(
    app: &axum::Router,
    peer: Option<SocketAddr>,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("POST").uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(addr) = peer {
        builder = builder.extension(ConnectInfo(addr));
    }
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let req = builder.body(Body::from(payload)).unwrap();
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
async fn verify_start_succeeds_and_returns_flow_id() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-abc"));
    let app = verify_app(store, verify.clone());

    let account_id = Uuid::new_v4();
    let loopback = Some("127.0.0.1:55000".parse().unwrap());
    let (status, body) = post_verify(
        &app,
        loopback,
        &format!("/v1/accounts/{account_id}/verify"),
        Some(json!({ "device_id": "TRUSTEDDEV" })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["flow_id"], "$flow-abc");
    // The handler forwarded the id + device straight to the port.
    assert_eq!(
        verify.calls(),
        vec![VerifyCall::Start {
            account_id,
            device_id: "TRUSTEDDEV".to_owned(),
        }]
    );
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_start_requires_loopback_peer() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-abc"));
    let app = verify_app(store, verify.clone());
    let account_id = Uuid::new_v4();
    let uri = format!("/v1/accounts/{account_id}/verify");
    let body = json!({ "device_id": "D" });

    // Non-loopback and missing-peer both fail closed before the handler runs.
    let off_box = Some("203.0.113.7:443".parse().unwrap());
    let (status, err) = post_verify(&app, off_box, &uri, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["error"]["code"], "forbidden");

    let (status, _) = post_verify(&app, None, &uri, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The guard short-circuited: the verification port was never invoked.
    assert!(verify.calls().is_empty());

    // A loopback peer passes through to the port.
    let loopback = Some("127.0.0.1:55001".parse().unwrap());
    let (status, _) = post_verify(&app, loopback, &uri, Some(body)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(verify.calls().len(), 1);
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_get_returns_flow_state_dto_and_stays_open() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-xyz"));
    let app = verify_app(store, verify.clone());
    let account_id = Uuid::new_v4();

    // The GET read carries no loopback layer — a plain request (no peer) is admitted.
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
async fn verify_list_returns_flows_and_stays_open() {
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
    let loopback = Some("127.0.0.1:55002".parse().unwrap());

    let (status, _) = post_verify(
        &app,
        loopback,
        &format!("/v1/accounts/{account_id}/verify/$flow-2/confirm"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = post_verify(
        &app,
        loopback,
        &format!("/v1/accounts/{account_id}/verify/$flow-2/cancel"),
        None,
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
async fn verify_confirm_requires_loopback_peer() {
    let store = store().await;
    let verify = Arc::new(StubVerification::ok("$flow-3"));
    let app = verify_app(store, verify.clone());
    let account_id = Uuid::new_v4();
    let uri = format!("/v1/accounts/{account_id}/verify/$flow-3/confirm");

    let off_box = Some("203.0.113.7:443".parse().unwrap());
    let (status, err) = post_verify(&app, off_box, &uri, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(err["error"]["code"], "forbidden");

    assert!(verify.calls().is_empty());
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn verify_error_maps_to_status() {
    let store = store().await;
    let loopback: Option<SocketAddr> = Some("127.0.0.1:55003".parse().unwrap());
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
        let (status, err) = post_verify(
            &app,
            loopback,
            &format!("/v1/accounts/{account_id}/verify"),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, want_status);
        assert_eq!(err["error"]["code"], want_code);
    }
}
