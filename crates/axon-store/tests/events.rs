//! Integration tests for the event store + re-decryption queue plumbing.
//!
//! These require a running Postgres and are `#[ignore]`d by default so the
//! normal `cargo test` stays database-free. Run them with:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store -- --ignored
//! ```

mod common;

use axon_store::NewEvent;
use serde_json::{json, Value};
use sqlx_core::row::Row;
use sqlx_postgres::PgPool;
use uuid::Uuid;

async fn read_event(pool: &PgPool, account_id: Uuid, event_id: &str) -> (Option<Value>, String) {
    let row = sqlx_core::query::query(
        "SELECT content, event_type FROM events WHERE account_id = $1 AND event_id = $2",
    )
    .bind(account_id)
    .bind(event_id)
    .fetch_one(pool)
    .await
    .expect("read event");
    (
        row.try_get::<Option<Value>, _>("content").expect("content"),
        row.try_get::<String, _>("event_type").expect("event_type"),
    )
}

/// The full life cycle: a UTD lands with `content = NULL` and a session id, the
/// queue finds it, back-fills it, and the `content IS NULL` guard then makes the
/// update idempotent (no clobbering an already-decrypted row).
#[tokio::test]
#[ignore = "requires Postgres"]
async fn pending_utd_is_found_then_back_filled_idempotently() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;

    // Unique per run so repeated runs don't collide.
    let user = format!("@utd-{}:localhost", Uuid::new_v4());
    let account = store
        .upsert_account(&user, "https://hs.example.org")
        .await
        .expect("upsert account");
    let account_id = account.account_id;

    let room_id = format!("!room-{}:localhost", Uuid::new_v4());
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    let session_id = format!("session-{}", Uuid::new_v4());

    // Insert a UTD by first constructing the raw_event with the session_id
    // (which is needed for the queue to find it) and then upserting with
    // `content = NULL` (which is needed for the back-fill guard).
    let raw_event = json!({
        "type": "m.room.encrypted",
        "event_id": event_id,
        "sender": "@alice:localhost",
        "content": { "algorithm": "m.megolm.v1.aes-sha2", "session_id": session_id }
    });
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id: &room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts: 1_700_000_000_000,
            event_type: "m.room.encrypted",
            content: None, // content is NULL for pending UTD
            raw_event: raw_event.clone(),
            megolm_session_id: Some(&session_id),
        })
        .await
        .expect("insert UTD");

    // The queue finds it by session...
    let by_session = store
        .pending_utds_for_session(account_id, &room_id, &session_id)
        .await
        .expect("pending by session");
    assert_eq!(by_session.len(), 1);
    assert_eq!(by_session[0].event_id, event_id);
    assert_eq!(by_session[0].room_id, room_id);
    assert_eq!(by_session[0].raw_event, raw_event);

    // ...and the startup sweep finds it among the account's backlog.
    let by_account = store
        .pending_utds_for_account(account_id)
        .await
        .expect("pending by account");
    assert!(by_account.iter().any(|p| p.event_id == event_id));

    // Back-fill the decrypted payload.
    let content = json!({ "msgtype": "m.text", "body": "decrypted!" });
    store
        .update_decrypted_event(account_id, &event_id, &content, "m.room.message")
        .await
        .expect("back-fill");

    // Row is no longer pending, and content/type were written.
    assert!(store
        .pending_utds_for_session(account_id, &room_id, &session_id)
        .await
        .expect("pending after flip")
        .is_empty());
    let (stored_content, stored_type) = read_event(&pool, account_id, &event_id).await;
    assert_eq!(stored_content, Some(content.clone()));
    assert_eq!(stored_type, "m.room.message");

    // Guard/idempotency: a second update can't clobber the decrypted row
    // (the `content IS NULL` guard no longer matches).
    store
        .update_decrypted_event(
            account_id,
            &event_id,
            &json!({ "body": "SHOULD NOT OVERWRITE" }),
            "m.room.redaction",
        )
        .await
        .expect("guarded update");
    let (after_content, after_type) = read_event(&pool, account_id, &event_id).await;
    assert_eq!(after_content, Some(content));
    assert_eq!(after_type, "m.room.message");

    // And re-delivering the original UTD envelope must not reset content to NULL
    // (upsert is ON CONFLICT DO NOTHING).
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id: &room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts: 1_700_000_000_000,
            event_type: "m.room.encrypted",
            content: None,
            raw_event,
            megolm_session_id: Some(&session_id),
        })
        .await
        .expect("re-upsert UTD");
    assert!(store
        .pending_utds_for_session(account_id, &room_id, &session_id)
        .await
        .expect("pending after re-upsert")
        .is_empty());

    common::cleanup_account(&pool, account_id).await;
}
