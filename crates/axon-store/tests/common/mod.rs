//! Shared helpers for the DB-gated store integration tests.
//!
//! Each integration test binary pulls this in with `mod common;`. Not every
//! binary uses every helper, so dead-code is allowed here.
#![allow(dead_code)]

use axon_store::{NewEvent, Store};
use sqlx_postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

fn db_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests")
}

/// Provision a throwaway account with a unique random user id (so concurrent or
/// repeated runs never collide), returning its `account_id`. `prefix` just makes
/// the generated user id readable in the DB during debugging.
pub async fn test_account(store: &Store, prefix: &str) -> Uuid {
    let user = format!("@{prefix}-{}:localhost", Uuid::new_v4());
    store
        .upsert_account(&user, "https://hs.example.org")
        .await
        .expect("account")
        .account_id
}

/// Insert a decrypted `m.room.message` with a text body, returning its event_id.
pub async fn insert_message(
    store: &Store,
    account_id: Uuid,
    room_id: &str,
    origin_ts: i64,
    body: &str,
) -> String {
    let event_id = format!("$evt-{}:localhost", Uuid::new_v4());
    let content = serde_json::json!({ "msgtype": "m.text", "body": body });
    store
        .upsert_event(&NewEvent {
            event_id: &event_id,
            room_id,
            account_id,
            sender: "@alice:localhost",
            origin_ts,
            event_type: "m.room.message",
            content: Some(content.clone()),
            raw_event: serde_json::json!({ "type": "m.room.message", "content": content }),
            megolm_session_id: None,
            redacts: None,
            relates_to: None,
            decrypted_body_text: Some(body),
        })
        .await
        .expect("insert message");
    event_id
}

/// A migrated `Store` connected to the test database. Named for the surprising
/// part — it runs migrations, not just opens a connection.
pub async fn migrated_store() -> Store {
    Store::connect(&db_url(), 5)
        .await
        .expect("connect + migrate")
}

/// A raw pool for reading columns the store API doesn't expose and for cleanup.
pub async fn raw_pool() -> PgPool {
    PgPoolOptions::new()
        .connect(&db_url())
        .await
        .expect("raw pool connect")
}

/// Delete an account and (via `ON DELETE CASCADE`) all its events, so repeated
/// test runs don't accumulate rows. Best-effort; call at the end of a test.
/// (A panic mid-test still leaves its row, but each test uses a unique random
/// user id, so the steady state stays clean.)
pub async fn cleanup_account(pool: &PgPool, account_id: Uuid) {
    let _ = sqlx_core::query::query("DELETE FROM accounts WHERE account_id = $1")
        .bind(account_id)
        .execute(pool)
        .await;
}
