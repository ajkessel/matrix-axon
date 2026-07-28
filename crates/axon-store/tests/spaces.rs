//! Integration tests for the space-hierarchy reads (`space_children`,
//! `space_parents`; issue #404, ADR 0084).
//!
//! Like the other store integration tests these need a running Postgres and
//! are `#[ignore]`d by default:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store -- --ignored
//! ```

mod common;

use axon_store::{RoomStateUpsert, Store};
use common::{cleanup_account, migrated_store, raw_pool, test_account};
use serde_json::{json, Value};
use uuid::Uuid;

async fn set_space_child(
    store: &Store,
    account_id: Uuid,
    space_room_id: &str,
    child_room_id: &str,
    origin_ts: i64,
    content: Value,
) {
    store
        .upsert_room_state(&RoomStateUpsert {
            account_id,
            room_id: space_room_id,
            event_type: "m.space.child",
            state_key: child_room_id,
            event_id: &format!("$child-{}:localhost", Uuid::new_v4()),
            sender: "@alice:localhost",
            origin_ts,
            content: Some(content),
        })
        .await
        .expect("m.space.child state");
}

async fn set_space_parent(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    parent_room_id: &str,
    content: Value,
) {
    store
        .upsert_room_state(&RoomStateUpsert {
            account_id,
            room_id,
            event_type: "m.space.parent",
            state_key: parent_room_id,
            event_id: &format!("$parent-{}:localhost", Uuid::new_v4()),
            sender: "@alice:localhost",
            origin_ts: 1,
            content: Some(content),
        })
        .await
        .expect("m.space.parent state");
}

/// Give `room_id` a name/avatar (and optionally an `m.room.create` `type`) so
/// the space reads' enrichment has something to find.
async fn set_room_display(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    name: &str,
    avatar_url: &str,
    room_type: Option<&str>,
) {
    store
        .upsert_room_state(&RoomStateUpsert {
            account_id,
            room_id,
            event_type: "m.room.name",
            state_key: "",
            event_id: &format!("$name-{}:localhost", Uuid::new_v4()),
            sender: "@alice:localhost",
            origin_ts: 1,
            content: Some(json!({ "name": name })),
        })
        .await
        .expect("name");
    store
        .upsert_room_state(&RoomStateUpsert {
            account_id,
            room_id,
            event_type: "m.room.avatar",
            state_key: "",
            event_id: &format!("$avatar-{}:localhost", Uuid::new_v4()),
            sender: "@alice:localhost",
            origin_ts: 1,
            content: Some(json!({ "url": avatar_url })),
        })
        .await
        .expect("avatar");
    if let Some(room_type) = room_type {
        store
            .upsert_room_state(&RoomStateUpsert {
                account_id,
                room_id,
                event_type: "m.room.create",
                state_key: "",
                event_id: &format!("$create-{}:localhost", Uuid::new_v4()),
                sender: "@alice:localhost",
                origin_ts: 1,
                content: Some(json!({ "type": room_type })),
            })
            .await
            .expect("create");
    }
}

/// MSC1772 ordering (`order` string ascending, absent sorts last) and the
/// per-child enrichment: a known plain room, a completely unknown room, and a
/// known nested space, each exercising a different combination of "known
/// room" x "has `order`" x "is itself a space."
#[tokio::test]
#[ignore = "requires Postgres"]
async fn space_children_orders_by_msc1772_and_enriches_known_children() {
    let store = migrated_store().await;
    let pool = raw_pool().await;
    let account_id = test_account(&store, "space-children").await;
    let space_room_id = format!("!space-{}:localhost", Uuid::new_v4());

    let child_a = format!("!child-a-{}:localhost", Uuid::new_v4());
    let child_b = format!("!child-b-{}:localhost", Uuid::new_v4());
    let child_space = format!("!child-space-{}:localhost", Uuid::new_v4());

    // order "b", never joined/known to Axon.
    set_space_child(
        &store,
        account_id,
        &space_room_id,
        &child_b,
        1_000,
        json!({ "via": [], "order": "b" }),
    )
    .await;
    // order "a" (sorts before "b"), known plain room.
    set_space_child(
        &store,
        account_id,
        &space_room_id,
        &child_a,
        2_000,
        json!({ "via": ["hs.example"], "order": "a", "suggested": true }),
    )
    .await;
    set_room_display(
        &store,
        account_id,
        &child_a,
        "Child A",
        "mxc://hs/child-a",
        None,
    )
    .await;
    // No `order` at all -> sorts after every child that has one, known nested space.
    set_space_child(
        &store,
        account_id,
        &space_room_id,
        &child_space,
        500,
        json!({ "via": ["hs.example"] }),
    )
    .await;
    set_room_display(
        &store,
        account_id,
        &child_space,
        "Nested Space",
        "mxc://hs/child-space",
        Some("m.space"),
    )
    .await;

    let children = store
        .space_children(account_id, &space_room_id)
        .await
        .expect("space_children");
    assert_eq!(children.len(), 3);

    assert_eq!(children[0].room_id, child_a);
    assert_eq!(children[0].order.as_deref(), Some("a"));
    assert!(children[0].suggested);
    assert_eq!(children[0].via, vec!["hs.example".to_owned()]);
    assert_eq!(children[0].name.as_deref(), Some("Child A"));
    assert_eq!(children[0].avatar_url.as_deref(), Some("mxc://hs/child-a"));
    assert_eq!(children[0].room_type, None);

    assert_eq!(children[1].room_id, child_b);
    assert_eq!(children[1].order.as_deref(), Some("b"));
    assert!(!children[1].suggested);
    assert!(children[1].via.is_empty());
    assert_eq!(children[1].name, None);
    assert_eq!(children[1].avatar_url, None);
    assert_eq!(children[1].room_type, None);

    assert_eq!(children[2].room_id, child_space);
    assert_eq!(children[2].order, None);
    assert_eq!(children[2].name.as_deref(), Some("Nested Space"));
    assert_eq!(
        children[2].avatar_url.as_deref(),
        Some("mxc://hs/child-space")
    );
    assert_eq!(children[2].room_type.as_deref(), Some("m.space"));

    cleanup_account(&pool, account_id).await;
}

/// Children with no `order` fall back to `origin_ts` then `room_id` — the
/// second and third MSC1772 tiebreaks, not exercised by the enrichment test
/// above (which only has one order-less child).
#[tokio::test]
#[ignore = "requires Postgres"]
async fn space_children_without_order_tie_break_on_origin_ts_then_room_id() {
    let store = migrated_store().await;
    let pool = raw_pool().await;
    let account_id = test_account(&store, "space-children-tie").await;
    let space_room_id = format!("!space-tie-{}:localhost", Uuid::new_v4());

    let late = format!("!tie-late-{}:localhost", Uuid::new_v4());
    let early = format!("!tie-early-{}:localhost", Uuid::new_v4());
    // Fixed (non-random) suffixes so the lexical room_id comparison at equal
    // origin_ts is deterministic.
    let same_ts_a = "!tie-same-a:localhost".to_owned();
    let same_ts_b = "!tie-same-b:localhost".to_owned();

    set_space_child(&store, account_id, &space_room_id, &late, 2_000, json!({})).await;
    set_space_child(&store, account_id, &space_room_id, &early, 1_000, json!({})).await;
    set_space_child(
        &store,
        account_id,
        &space_room_id,
        &same_ts_b,
        1_500,
        json!({}),
    )
    .await;
    set_space_child(
        &store,
        account_id,
        &space_room_id,
        &same_ts_a,
        1_500,
        json!({}),
    )
    .await;

    let children = store
        .space_children(account_id, &space_room_id)
        .await
        .expect("space_children");
    let room_ids: Vec<&str> = children.iter().map(|c| c.room_id.as_str()).collect();
    assert_eq!(room_ids, vec![&early, &same_ts_a, &same_ts_b, &late]);

    cleanup_account(&pool, account_id).await;
}

/// The reverse lookup: a room's parent spaces, with the same enrichment as
/// children but `canonical` instead of `order`/`suggested`.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn space_parents_reads_canonical_flag_and_enriches_known_parents() {
    let store = migrated_store().await;
    let pool = raw_pool().await;
    let account_id = test_account(&store, "space-parents").await;
    let room_id = format!("!room-{}:localhost", Uuid::new_v4());

    let parent_known = format!("!parent-known-{}:localhost", Uuid::new_v4());
    let parent_unknown = format!("!parent-unknown-{}:localhost", Uuid::new_v4());

    set_space_parent(
        &store,
        account_id,
        &room_id,
        &parent_known,
        json!({ "via": ["hs.example"], "canonical": true }),
    )
    .await;
    set_room_display(
        &store,
        account_id,
        &parent_known,
        "Parent Space",
        "mxc://hs/parent",
        Some("m.space"),
    )
    .await;
    set_space_parent(
        &store,
        account_id,
        &room_id,
        &parent_unknown,
        json!({ "via": [] }),
    )
    .await;

    let parents = store
        .space_parents(account_id, &room_id)
        .await
        .expect("space_parents");
    assert_eq!(parents.len(), 2);

    let known = parents
        .iter()
        .find(|p| p.room_id == parent_known)
        .expect("known parent present");
    assert!(known.canonical);
    assert_eq!(known.via, vec!["hs.example".to_owned()]);
    assert_eq!(known.name.as_deref(), Some("Parent Space"));
    assert_eq!(known.avatar_url.as_deref(), Some("mxc://hs/parent"));
    assert_eq!(known.room_type.as_deref(), Some("m.space"));

    let unknown = parents
        .iter()
        .find(|p| p.room_id == parent_unknown)
        .expect("unknown parent present");
    assert!(!unknown.canonical);
    assert_eq!(unknown.name, None);
    assert_eq!(unknown.avatar_url, None);
    assert_eq!(unknown.room_type, None);

    cleanup_account(&pool, account_id).await;
}

/// An unknown room, or one with no space state, yields an empty list — not an
/// error — for both directions.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn space_reads_are_empty_for_a_room_with_no_space_state() {
    let store = migrated_store().await;
    let pool = raw_pool().await;
    let account_id = test_account(&store, "space-empty").await;
    let room_id = format!("!room-{}:localhost", Uuid::new_v4());

    assert!(store
        .space_children(account_id, &room_id)
        .await
        .expect("space_children")
        .is_empty());
    assert!(store
        .space_parents(account_id, &room_id)
        .await
        .expect("space_parents")
        .is_empty());

    cleanup_account(&pool, account_id).await;
}

/// A room can hold an arbitrary number of `m.space.child`/`m.space.parent`
/// state keys (bounded only by upstream federation, not by Axon), so both
/// reads must cap rather than return everything unbounded. `SPACE_HIERARCHY_CAP`
/// itself is a private store constant, so this pins the current cap value
/// (1000) rather than importing it — if the store's cap ever changes, update
/// `over_cap`/the assertions here to match.
const SPACE_HIERARCHY_CAP: i64 = 1000;

/// Bulk-insert `count` distinct `m.space.child` rows for `space_room_id`, each
/// with a unique `origin_ts` (`1..=count`) and no `order` — one round trip via
/// `generate_series` rather than `count` sequential awaited upserts, so a
/// >1000-row test stays fast.
async fn bulk_insert_space_children(
    pool: &sqlx_postgres::PgPool,
    account_id: Uuid,
    space_room_id: &str,
    count: i64,
) {
    sqlx_core::query::query(
        "INSERT INTO room_state \
             (account_id, room_id, event_type, state_key, event_id, sender, origin_ts, content) \
         SELECT $1, $2, 'm.space.child', '!child-' || g || ':localhost', \
                '$child-' || g || ':localhost', '@alice:localhost', g, '{}'::jsonb \
         FROM generate_series(1, $3) AS g",
    )
    .bind(account_id)
    .bind(space_room_id)
    .bind(count)
    .execute(pool)
    .await
    .expect("bulk insert space children");
}

/// Same as [`bulk_insert_space_children`] but for `m.space.parent`, state-keyed
/// on the parent room id rather than a child's.
async fn bulk_insert_space_parents(
    pool: &sqlx_postgres::PgPool,
    account_id: Uuid,
    room_id: &str,
    count: i64,
) {
    sqlx_core::query::query(
        "INSERT INTO room_state \
             (account_id, room_id, event_type, state_key, event_id, sender, origin_ts, content) \
         SELECT $1, $2, 'm.space.parent', '!parent-' || g || ':localhost', \
                '$parent-' || g || ':localhost', '@alice:localhost', g, '{}'::jsonb \
         FROM generate_series(1, $3) AS g",
    )
    .bind(account_id)
    .bind(room_id)
    .bind(count)
    .execute(pool)
    .await
    .expect("bulk insert space parents");
}

/// `space_children` truncates to `SPACE_HIERARCHY_CAP` rows, keeping the
/// leading (lowest `origin_ts`, since none of these have an `order`) entries
/// per the MSC1772 sort — not an arbitrary subset.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn space_children_are_capped_at_space_hierarchy_cap() {
    let store = migrated_store().await;
    let pool = raw_pool().await;
    let account_id = test_account(&store, "space-children-cap").await;
    let space_room_id = format!("!space-cap-{}:localhost", Uuid::new_v4());

    let over_cap = SPACE_HIERARCHY_CAP + 5;
    bulk_insert_space_children(&pool, account_id, &space_room_id, over_cap).await;

    let children = store
        .space_children(account_id, &space_room_id)
        .await
        .expect("space_children");
    assert_eq!(children.len() as i64, SPACE_HIERARCHY_CAP);
    // origin_ts 1..=SPACE_HIERARCHY_CAP survive; the last 5 (over the cap) don't.
    assert_eq!(children[0].room_id, "!child-1:localhost");
    assert_eq!(
        children[(SPACE_HIERARCHY_CAP - 1) as usize].room_id,
        format!("!child-{SPACE_HIERARCHY_CAP}:localhost")
    );
    assert!(!children
        .iter()
        .any(|c| c.room_id == format!("!child-{over_cap}:localhost")));

    cleanup_account(&pool, account_id).await;
}

/// `space_parents` is capped the same way as `space_children`.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn space_parents_are_capped_at_space_hierarchy_cap() {
    let store = migrated_store().await;
    let pool = raw_pool().await;
    let account_id = test_account(&store, "space-parents-cap").await;
    let room_id = format!("!room-cap-{}:localhost", Uuid::new_v4());

    bulk_insert_space_parents(&pool, account_id, &room_id, SPACE_HIERARCHY_CAP + 5).await;

    let parents = store
        .space_parents(account_id, &room_id)
        .await
        .expect("space_parents");
    assert_eq!(parents.len() as i64, SPACE_HIERARCHY_CAP);

    cleanup_account(&pool, account_id).await;
}
