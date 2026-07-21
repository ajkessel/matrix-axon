//! Integration tests for `room_unread_counts` reads/deletes (issue #313).
//!
//! Like the other store integration tests these need a running Postgres and
//! are `#[ignore]`d by default. Run them with:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store -- --ignored
//! ```

mod common;

use common::test_account;
use uuid::Uuid;

/// `room_unread_counts` reads back exactly what was upserted, keyed by room
/// id, and is scoped to the requesting account.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn room_unread_counts_reads_back_upserted_rows() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = test_account(&store, "unread-read").await;
    let other_account_id = test_account(&store, "unread-read-other").await;
    let room_a = format!("!a-{}:localhost", Uuid::new_v4());
    let room_b = format!("!b-{}:localhost", Uuid::new_v4());

    store
        .upsert_room_unread_counts(account_id, &room_a, 3, 1)
        .await
        .expect("seed room a");
    store
        .upsert_room_unread_counts(account_id, &room_b, 0, 0)
        .await
        .expect("seed room b");
    store
        .upsert_room_unread_counts(other_account_id, &room_a, 99, 99)
        .await
        .expect("seed other account, same room id");

    let counts = store
        .room_unread_counts(account_id)
        .await
        .expect("read counts");
    assert_eq!(counts.len(), 2);
    assert_eq!(counts.get(&room_a), Some(&(3, 1)));
    assert_eq!(counts.get(&room_b), Some(&(0, 0)));

    common::cleanup_account(&pool, account_id).await;
    common::cleanup_account(&pool, other_account_id).await;
}

/// An account with no persisted rows reads back an empty map, not an error.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn room_unread_counts_is_empty_for_a_fresh_account() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = test_account(&store, "unread-fresh").await;

    let counts = store
        .room_unread_counts(account_id)
        .await
        .expect("read counts");
    assert!(counts.is_empty());

    common::cleanup_account(&pool, account_id).await;
}

/// `delete_stale_room_unread_counts` removes only the rows whose room id is
/// absent from the given joined set, leaving the rest (and other accounts'
/// rows) untouched.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_stale_room_unread_counts_prunes_only_non_joined_rooms() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = test_account(&store, "unread-prune").await;
    let other_account_id = test_account(&store, "unread-prune-other").await;
    let joined = format!("!joined-{}:localhost", Uuid::new_v4());
    let left = format!("!left-{}:localhost", Uuid::new_v4());

    store
        .upsert_room_unread_counts(account_id, &joined, 5, 2)
        .await
        .expect("seed joined room");
    store
        .upsert_room_unread_counts(account_id, &left, 7, 3)
        .await
        .expect("seed left room");
    store
        .upsert_room_unread_counts(other_account_id, &left, 11, 4)
        .await
        .expect("seed other account's row for the same room id");

    store
        .delete_stale_room_unread_counts(account_id, std::slice::from_ref(&joined))
        .await
        .expect("prune");

    let counts = store
        .room_unread_counts(account_id)
        .await
        .expect("read counts");
    assert_eq!(counts.len(), 1, "only the joined room's row survives");
    assert_eq!(counts.get(&joined), Some(&(5, 2)));
    assert!(!counts.contains_key(&left));

    let other_counts = store
        .room_unread_counts(other_account_id)
        .await
        .expect("read other account's counts");
    assert_eq!(
        other_counts.get(&left),
        Some(&(11, 4)),
        "pruning one account's stale rows must not touch another account's row \
         for the same room id"
    );

    common::cleanup_account(&pool, account_id).await;
    common::cleanup_account(&pool, other_account_id).await;
}

/// An empty joined set deletes every row for the account — correct when the
/// account currently has no joined rooms at all.
#[tokio::test]
#[ignore = "requires Postgres"]
async fn delete_stale_room_unread_counts_with_empty_joined_set_deletes_everything() {
    let store = common::migrated_store().await;
    let pool = common::raw_pool().await;
    let account_id = test_account(&store, "unread-prune-all").await;
    let room_id = format!("!r-{}:localhost", Uuid::new_v4());

    store
        .upsert_room_unread_counts(account_id, &room_id, 1, 0)
        .await
        .expect("seed row");

    store
        .delete_stale_room_unread_counts(account_id, &[])
        .await
        .expect("prune with empty joined set");

    let counts = store
        .room_unread_counts(account_id)
        .await
        .expect("read counts");
    assert!(counts.is_empty());

    common::cleanup_account(&pool, account_id).await;
}
