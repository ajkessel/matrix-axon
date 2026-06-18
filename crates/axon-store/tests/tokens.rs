//! DB-gated tests for the bearer-token store (M7b).
//!
//! Like the other store tests these need Postgres and are `#[ignore]`d by
//! default:
//!
//! ```sh
//! docker compose up -d postgres
//! DATABASE_URL=postgres://axon:axon@127.0.0.1:5432/axon cargo test -p axon-store --test tokens -- --ignored
//! ```

mod common;

use common::migrated_store;

#[tokio::test]
#[ignore = "requires Postgres"]
async fn issue_then_verify_round_trips_and_touches_last_used() {
    let store = migrated_store().await;

    let issued = store.issue_token("laptop").await.expect("issue");
    assert!(issued.token.starts_with("axon_"), "raw token is prefixed");
    assert_eq!(issued.label, "laptop");

    // The raw token verifies and resolves to its row id.
    let id = store
        .verify_token(&issued.token)
        .await
        .expect("verify")
        .expect("token is accepted");
    assert_eq!(id, issued.id);

    // Verification stamped last_used_at.
    let listed = store.list_tokens().await.expect("list");
    let row = listed
        .iter()
        .find(|t| t.id == issued.id)
        .expect("issued token listed");
    assert_eq!(row.label, "laptop");
    assert!(row.last_used_at.is_some(), "verify touched last_used_at");
    assert!(!row.is_revoked());

    store.revoke_token(issued.id).await.expect("cleanup revoke");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn unknown_token_is_rejected() {
    let store = migrated_store().await;
    let got = store
        .verify_token("axon_definitely-not-a-real-token")
        .await
        .expect("verify");
    assert!(got.is_none(), "an unknown token must not verify");
}

#[tokio::test]
#[ignore = "requires Postgres"]
async fn revoked_token_no_longer_verifies() {
    let store = migrated_store().await;
    let issued = store.issue_token("revoke-me").await.expect("issue");

    // First revoke reports it acted; verification then fails.
    assert!(store.revoke_token(issued.id).await.expect("revoke"));
    assert!(
        store
            .verify_token(&issued.token)
            .await
            .expect("verify")
            .is_none(),
        "a revoked token must not verify"
    );

    // The row survives as a tombstone (revocation is not a delete).
    let listed = store.list_tokens().await.expect("list");
    let row = listed
        .iter()
        .find(|t| t.id == issued.id)
        .expect("revoked token still listed");
    assert!(row.is_revoked());

    // Re-revoking an already-revoked token is a no-op.
    assert!(
        !store.revoke_token(issued.id).await.expect("re-revoke"),
        "re-revoking reports no change"
    );
}
