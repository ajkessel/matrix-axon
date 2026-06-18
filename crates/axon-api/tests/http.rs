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

use axon_api::{AccountLifecycle, AppState};
use axon_store::{AccountState, NewEvent, RoomStateUpsert, Store};
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use common::{
    DeleteOutcome, LoginCall, LoginOutcome, LogoutOutcome, RecoverOutcome, StubLifecycle,
    StubSender, StubTokenVerifier, StubVerification, VerifyCall, VerifyOutcome, TEST_TOKEN,
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
        Arc::new(StubTokenVerifier::ok()),
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
        Arc::new(StubTokenVerifier::ok()),
    ))
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
        Arc::new(StubTokenVerifier::ok()),
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
        Arc::new(StubTokenVerifier::ok()),
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
            device_id: "TRUSTEDDEV".to_owned(),
        }]
    );
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
