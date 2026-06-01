//! Shared helpers for the DB-gated store integration tests.
//!
//! Each integration test binary pulls this in with `mod common;`. Not every
//! binary uses every helper, so dead-code is allowed here.
#![allow(dead_code)]

use axon_store::Store;
use sqlx_postgres::{PgPool, PgPoolOptions};
use uuid::Uuid;

fn db_url() -> String {
    std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for integration tests")
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
