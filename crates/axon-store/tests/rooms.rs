//! Integration tests for the room list (`list_rooms`) and single-event read
//! (`get_event`).
//!
//! Like the other store integration tests these need a running Postgres and are
//! `#[ignore]`d by default. Run them with:
//!
//! ```sh
//! docker compose up -d postgres
//! # 5432 is the default; use your compose host port (e.g. 5433 if 5432 is taken).
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store -- --ignored
//! ```

mod common;

use axon_store::{NewEvent, RoomStateUpsert, Store};
use common::{insert_message, test_account};
use serde_json::{json, Value};
use uuid::Uuid;

/// Redact `target` with an `m.room.redaction` event, returning the redaction's
/// event_id.
async fn insert_redaction(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    origin_ts: i64,
    target: &str,
) -> String {
    let event_id = format!("$red-{}:localhost", Uuid::new_v4());
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts,
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
    event_id
}

async fn set_state(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    event_type: &str,
    content: Value,
) {
    store
        .upsert_room_state(&RoomStateUpsert {
            account_id,
            room_id,
            event_type,
            state_key: "",
            event_id: &format!("$state-{}:localhost", Uuid::new_v4()),
            sender: "@alice:localhost",
            origin_ts: 1_000,
            content: Some(content),
        })
        .await
        .expect("set state");
}

/// `list_rooms` orders by most-recent activity, carries the latest event id, and
/// resolves the four display fields from current room state.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn list_rooms_orders_by_activity_with_summary_fields() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = test_account(&store, "rooms").await;
    let room_a = format!("!a-{}:localhost", Uuid::new_v4());
    let room_b = format!("!b-{}:localhost", Uuid::new_v4());

    // Room A: two events; the newest (ts 2000) is the activity marker.
    insert_message(&store, account_id, &room_a, 1_000, "first").await;
    let a_latest = insert_message(&store, account_id, &room_a, 2_000, "second").await;
    set_state(
        &store,
        account_id,
        &room_a,
        "m.room.name",
        json!({ "name": "Alpha" }),
    )
    .await;
    set_state(
        &store,
        account_id,
        &room_a,
        "m.room.topic",
        json!({ "topic": "about a" }),
    )
    .await;
    set_state(
        &store,
        account_id,
        &room_a,
        "m.room.avatar",
        json!({ "url": "mxc://hs/a" }),
    )
    .await;
    set_state(
        &store,
        account_id,
        &room_a,
        "m.room.canonical_alias",
        json!({ "alias": "#alpha:localhost" }),
    )
    .await;

    // Room B: a single, newer event (ts 3000) and no state — sorts first.
    let b_latest = insert_message(&store, account_id, &room_b, 3_000, "newest").await;

    let rooms = store.list_rooms(Some(account_id)).await.expect("list");
    assert_eq!(rooms.len(), 2, "exactly the two rooms for this account");

    // Newest-activity first: Room B (3000) before Room A (2000).
    assert_eq!(rooms[0].room_id, room_b);
    assert!(rooms[0].account_user_id.starts_with("@rooms-"));
    assert_eq!(rooms[0].last_activity_ts, 3_000);
    assert_eq!(rooms[0].last_event_id.as_deref(), Some(b_latest.as_str()));
    assert!(rooms[0].name.is_none(), "room B has no name state");

    assert_eq!(rooms[1].room_id, room_a);
    assert!(rooms[1].account_user_id.starts_with("@rooms-"));
    assert_eq!(rooms[1].last_activity_ts, 2_000);
    assert_eq!(rooms[1].last_event_id.as_deref(), Some(a_latest.as_str()));
    assert_eq!(rooms[1].name.as_deref(), Some("Alpha"));
    assert_eq!(rooms[1].topic.as_deref(), Some("about a"));
    assert_eq!(rooms[1].avatar_url.as_deref(), Some("mxc://hs/a"));
    assert_eq!(
        rooms[1].canonical_alias.as_deref(),
        Some("#alpha:localhost")
    );

    common::cleanup_account(&pool, account_id).await;
}

/// `list_rooms` filters by account: a room under one account never leaks into
/// another's list, and `None` returns both accounts' rooms.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn list_rooms_filters_by_account() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let acc1 = test_account(&store, "f1").await;
    let acc2 = test_account(&store, "f2").await;
    let room1 = format!("!r1-{}:localhost", Uuid::new_v4());
    let room2 = format!("!r2-{}:localhost", Uuid::new_v4());
    insert_message(&store, acc1, &room1, 1_000, "one").await;
    insert_message(&store, acc2, &room2, 1_000, "two").await;

    let only1 = store.list_rooms(Some(acc1)).await.expect("list acc1");
    assert_eq!(only1.len(), 1);
    assert_eq!(only1[0].room_id, room1);
    assert_eq!(only1[0].account_id, acc1);
    assert!(only1[0].account_user_id.starts_with("@f1-"));

    // None spans accounts; both of ours appear (alongside any others in the DB).
    let all = store.list_rooms(None).await.expect("list all");
    assert!(all
        .iter()
        .any(|r| r.room_id == room1 && r.account_id == acc1));
    assert!(all
        .iter()
        .any(|r| r.room_id == room2 && r.account_id == acc2));

    common::cleanup_account(&pool, acc1).await;
    common::cleanup_account(&pool, acc2).await;
}

/// `get_event` returns a present event, `None` for a miss, and masks a redacted
/// event's content while reporting the redaction id.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn get_event_hit_miss_and_redaction_masking() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = test_account(&store, "ge").await;
    let room_id = format!("!ge-{}:localhost", Uuid::new_v4());

    // Hit: a plain message comes back with content and no redaction.
    let msg = insert_message(&store, account_id, &room_id, 1_000, "hello").await;
    let got = store
        .get_event(account_id, &msg)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(got.event_id, msg);
    assert_eq!(got.decrypted_body_text.as_deref(), Some("hello"));
    assert!(got.content.is_some());
    assert!(got.redaction_event_id.is_none());

    // Miss: an unknown id is None, not an error.
    assert!(store
        .get_event(account_id, "$nope:localhost")
        .await
        .expect("get miss")
        .is_none());

    // Redaction masking: the redacted message reads back masked.
    let redaction = insert_redaction(&store, account_id, &room_id, 2_000, &msg).await;
    let masked = store
        .get_event(account_id, &msg)
        .await
        .expect("get redacted")
        .expect("present");
    assert!(masked.content.is_none(), "content masked at read time");
    assert!(masked.decrypted_body_text.is_none(), "body masked");
    assert_eq!(
        masked.redaction_event_id.as_deref(),
        Some(redaction.as_str())
    );

    common::cleanup_account(&pool, account_id).await;
}
