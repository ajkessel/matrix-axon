//! Integration tests for the search-index store support (M9): the batched,
//! projection-aware `events_for_index` read and the transactional `search_outbox`
//! fan-out (ADR 0039).
//!
//! DB-gated like the other store tests; run with:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store -- --ignored
//! ```

mod common;

use axon_store::{NewEvent, Store};
use common::insert_message;
use serde_json::{json, Value};
use uuid::Uuid;

const SENDER: &str = "@alice:localhost";

/// Insert an event carrying an explicit `relates_to`, lifting any text body into
/// `decrypted_body_text` like live ingestion does.
async fn insert_relation(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    origin_ts: i64,
    event_type: &str,
    content: Value,
    relates_to: Value,
) -> String {
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    let body = content.get("body").and_then(|b| b.as_str());
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: SENDER,
            origin_ts,
            event_type,
            content: Some(content.clone()),
            raw_event: json!({ "type": event_type, "content": content }),
            megolm_session_id: None,
            redacts: None,
            relates_to: Some(relates_to),
            decrypted_body_text: body,
        })
        .await
        .expect("insert relation");
    event_id
}

async fn insert_edit(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    origin_ts: i64,
    target: &str,
    new_body: &str,
) -> String {
    insert_relation(
        store,
        account_id,
        room_id,
        origin_ts,
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

async fn insert_reaction(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    origin_ts: i64,
    target: &str,
    key: &str,
) -> String {
    insert_relation(
        store,
        account_id,
        room_id,
        origin_ts,
        "m.reaction",
        json!({}),
        json!({ "rel_type": "m.annotation", "event_id": target, "key": key }),
    )
    .await
}

async fn insert_redaction(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    origin_ts: i64,
    target: &str,
) {
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: SENDER,
            origin_ts,
            event_type: "m.room.redaction",
            content: None,
            raw_event: json!({ "type": "m.room.redaction", "redacts": target }),
            megolm_session_id: None,
            redacts: Some(target),
            relates_to: None,
            decrypted_body_text: None,
        })
        .await
        .expect("insert redaction");
}

/// `events_for_index` returns exactly the indexable message events with their
/// **resolved** bodies: a plain message; an edited message by its *new* body (not
/// the original); and *not* the standalone edit/reaction events, the redacted
/// message, or non-text events.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn events_for_index_returns_resolved_indexable_set() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = common::test_account(&store, "idx").await;
    let room = format!("!room-{}:localhost", Uuid::new_v4());

    // A plain message.
    let plain = insert_message(&store, account_id, &room, 100, "hello plain world").await;
    // A message that gets edited — its resolved body should be the new text.
    let edited = insert_message(&store, account_id, &room, 200, "original body text").await;
    insert_edit(&store, account_id, &room, 250, &edited, "edited body text").await;
    // A message that gets redacted — excluded from the index entirely.
    let redacted = insert_message(&store, account_id, &room, 300, "secret to be removed").await;
    insert_redaction(&store, account_id, &room, 350, &redacted).await;
    // A reaction (standalone relation, no body) — excluded.
    insert_reaction(&store, account_id, &room, 400, &plain, "👍").await;

    // `events_for_index` is cross-account (one combined index), so drain every
    // batch and scope to this test's account — the DB may hold other tests' rows.
    let mut all = Vec::new();
    let mut after = 0i64;
    loop {
        let batch = store
            .events_for_index(after, 500)
            .await
            .expect("events_for_index");
        let Some(last) = batch.last() else { break };
        after = last.id;
        let n = batch.len();
        all.extend(batch);
        if n < 500 {
            break;
        }
    }
    let mine: Vec<_> = all.iter().filter(|r| r.account_id == account_id).collect();
    let by_id: std::collections::HashMap<&str, &str> = mine
        .iter()
        .map(|r| (r.event_id.as_str(), r.body.as_str()))
        .collect();

    // Plain message present with its body.
    assert_eq!(by_id.get(plain.as_str()), Some(&"hello plain world"));
    // Edited message present with the NEW body, not the original.
    assert_eq!(by_id.get(edited.as_str()), Some(&"edited body text"));
    // Redacted message absent.
    assert!(
        !by_id.contains_key(redacted.as_str()),
        "redacted event must not be indexed"
    );
    // No standalone edit/reaction rows (only the plain + edited messages remain).
    assert_eq!(
        mine.len(),
        2,
        "only the plain + edited messages are indexable"
    );
    // Every returned row carries a non-empty body.
    assert!(mine.iter().all(|r| !r.body.is_empty()));

    common::cleanup_account(&pool, account_id).await;
}

/// An empty resolved body is excluded — the bulk build must not index a bodyless
/// document the live path would skip (rebuild/incremental convergence).
#[tokio::test]
#[ignore = "requires Postgres"]
async fn events_for_index_excludes_empty_body() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = common::test_account(&store, "idxempty").await;
    let room = format!("!room-{}:localhost", Uuid::new_v4());

    let empty = insert_message(&store, account_id, &room, 100, "").await;
    let nonempty = insert_message(&store, account_id, &room, 200, "has content").await;

    let mut all = Vec::new();
    let mut after = 0i64;
    loop {
        let batch = store
            .events_for_index(after, 500)
            .await
            .expect("events_for_index");
        let Some(last) = batch.last() else { break };
        after = last.id;
        let n = batch.len();
        all.extend(batch);
        if n < 500 {
            break;
        }
    }
    let mine: std::collections::HashSet<&str> = all
        .iter()
        .filter(|r| r.account_id == account_id)
        .map(|r| r.event_id.as_str())
        .collect();

    assert!(
        !mine.contains(empty.as_str()),
        "empty-body event must not be indexed"
    );
    assert!(mine.contains(nonempty.as_str()), "non-empty event indexed");

    common::cleanup_account(&pool, account_id).await;
}

/// `events_for_index` keyset-paginates by ascending `id`.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn events_for_index_keyset_paginates() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = common::test_account(&store, "idxpage").await;
    let room = format!("!room-{}:localhost", Uuid::new_v4());

    for i in 0..5 {
        insert_message(
            &store,
            account_id,
            &room,
            100 + i,
            &format!("message number {i}"),
        )
        .await;
    }

    // First page of 2.
    let page1 = store.events_for_index(0, 2).await.expect("page1");
    assert_eq!(page1.len(), 2);
    let after = page1.last().unwrap().id;
    assert!(page1.iter().all(|r| r.id <= after));

    // Next page continues strictly after the cursor and is ordered.
    let page2 = store.events_for_index(after, 2).await.expect("page2");
    assert_eq!(page2.len(), 2);
    assert!(page2.iter().all(|r| r.id > after));

    common::cleanup_account(&pool, account_id).await;
}

/// Each index-affecting write appends its outbox obligation in the *same*
/// transaction as the mutation: a message enqueues its own id; an edit and a
/// redaction each *also* enqueue their target's id (the document that actually
/// changes); a reply enqueues only its own id (no top-level target).
///
/// Reads the outbox directly (filtered to this account, from a pre-insert
/// high-water baseline) without advancing the cursor or pruning, so it is safe to
/// run alongside the other store tests on a shared database.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn outbox_fans_out_to_relation_targets() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = common::test_account(&store, "outbox").await;
    let room = format!("!room-{}:localhost", Uuid::new_v4());

    let baseline = store.search_outbox_high_water().await.expect("high water");

    let target = insert_message(&store, account_id, &room, 100, "original body").await;
    let edit = insert_edit(&store, account_id, &room, 150, &target, "edited body").await;
    insert_redaction(&store, account_id, &room, 200, &target).await;
    // A reply: `m.relates_to` carries only `m.in_reply_to`, no top-level event_id.
    let reply = insert_relation(
        &store,
        account_id,
        &room,
        250,
        "m.room.message",
        json!({ "msgtype": "m.text", "body": "a reply" }),
        json!({ "m.in_reply_to": { "event_id": target } }),
    )
    .await;

    let rows = store
        .drain_search_outbox(baseline, 10_000)
        .await
        .expect("drain");
    let ids: Vec<String> = rows
        .into_iter()
        .filter(|r| r.account_id == account_id)
        .map(|r| r.event_id)
        .collect();

    let count = |id: &str| ids.iter().filter(|x| x.as_str() == id).count();
    // Every insert enqueues its own id.
    assert_eq!(count(&edit), 1, "edit's own id enqueued once");
    assert_eq!(count(&reply), 1, "reply's own id enqueued once");
    // The target is enqueued three times: its own insert, the edit's fan-out, and
    // the redaction's fan-out — but NOT a fourth time for the reply (a reply
    // changes no other document).
    assert_eq!(
        count(&target),
        3,
        "own insert + edit fan-out + redaction fan-out, not the reply"
    );
    // Ordinary writes never append the purge sentinel.
    assert!(!ids.iter().any(|id| id.is_empty()));

    common::cleanup_account(&pool, account_id).await;
}

/// Deleting an account row appends the purge sentinel (`event_id = ""`) in the
/// same statement, and — because `search_outbox` has no FK to `accounts` — the
/// obligation outlives the cascaded row.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_account_row_appends_purge_sentinel() {
    let store = common::migrated_store().await;
    let account_id = common::test_account(&store, "purge").await;
    let room = format!("!room-{}:localhost", Uuid::new_v4());

    let baseline = store.search_outbox_high_water().await.expect("high water");
    insert_message(&store, account_id, &room, 100, "doomed body").await;

    store
        .delete_account_row(account_id)
        .await
        .expect("delete account row");

    let rows = store
        .drain_search_outbox(baseline, 10_000)
        .await
        .expect("drain");
    let mine: Vec<_> = rows
        .into_iter()
        .filter(|r| r.account_id == account_id)
        .collect();
    assert!(
        mine.iter().any(|r| r.is_purge()),
        "a purge sentinel is appended for the deleted account"
    );
    // The account row (and its events) are already gone via the cascade; nothing
    // to clean up. The outbox rows linger globally and are pruned by the indexer.
}

/// The durable cursor round-trips and never reads back less than it was set to.
/// The cursor is a global singleton (one indexer in production), so this is the
/// sole store test that mutates it.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn outbox_cursor_round_trips() {
    let store = common::migrated_store().await;

    let hw = store.search_outbox_high_water().await.expect("high water");
    // Advancing to the current high-water mark is always valid.
    store
        .set_search_outbox_cursor(hw)
        .await
        .expect("set cursor");
    assert_eq!(store.search_outbox_cursor().await.expect("read"), hw);
}
