//! Integration tests for the account store methods.
//!
//! These require a running Postgres and are `#[ignore]`d by default so the
//! normal `cargo test` stays database-free. Run them with:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store -- --ignored
//! ```

mod common;

use uuid::Uuid;

#[tokio::test]
#[ignore = "requires Postgres"]
async fn upsert_is_idempotent_and_listable() {
    let store = common::migrated_store().await;
    // Unique user per run so repeated runs don't collide.
    let user = format!("@upsert-{}:localhost", Uuid::new_v4());
    let hs = "https://hs.example.org";

    let first = store.upsert_account(&user, hs).await.expect("first upsert");
    let second = store
        .upsert_account(&user, hs)
        .await
        .expect("second upsert");
    // Same logical account: stable primary key across calls.
    assert_eq!(first.account_id, second.account_id);

    let listed = store.list_accounts().await.expect("list");
    assert!(listed.iter().any(|a| a.account_id == first.account_id));

    common::cleanup_account(&common::raw_pool().await, first.account_id).await;
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn session_token_encrypts_and_round_trips() {
    let store = common::migrated_store().await;
    let user = format!("@session-{}:localhost", Uuid::new_v4());
    let account = store
        .upsert_account(&user, "https://hs.example.org")
        .await
        .expect("upsert");

    // No session yet.
    assert!(store
        .account_token(account.account_id, "key-A")
        .await
        .expect("token query")
        .is_none());

    store
        .set_account_session(account.account_id, "DEVICE42", "syt_secret", "key-A")
        .await
        .expect("set session");

    // Correct key round-trips.
    assert_eq!(
        store
            .account_token(account.account_id, "key-A")
            .await
            .expect("token query"),
        Some("syt_secret".to_owned())
    );

    // Device id was persisted.
    let listed = store.list_accounts().await.expect("list");
    let row = listed
        .iter()
        .find(|a| a.account_id == account.account_id)
        .expect("row");
    assert_eq!(row.device_id.as_deref(), Some("DEVICE42"));

    // Wrong key fails to decrypt (pgcrypto raises an error).
    assert!(store
        .account_token(account.account_id, "wrong-key")
        .await
        .is_err());

    common::cleanup_account(&common::raw_pool().await, account.account_id).await;
}
