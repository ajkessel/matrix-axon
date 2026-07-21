//! Server-derived per-room unread counts (issue #313, ADR 0070).
//!
//! Axon does not compute these values — they are matrix-sdk's already-derived
//! read of the upstream homeserver's own sync room-summary, captured by a
//! watcher in `axon-sync` and cached here so a fresh client load can read a
//! number without first observing a live event. See
//! `room_unread_counts.sql` and ADR 0070 for the encrypted-room caveat this
//! inherits.

use std::collections::HashMap;

use sqlx_postgres::Postgres;
use uuid::Uuid;

use crate::{Store, StoreError};

impl Store {
    /// Upsert one room's cached unread counts. A single `INSERT ... ON
    /// CONFLICT DO UPDATE` — one round trip, no read-modify-write — since the
    /// caller (the `axon-sync` watcher) always supplies the full current
    /// value, never a delta.
    pub async fn upsert_room_unread_counts(
        &self,
        account_id: Uuid,
        room_id: &str,
        notification_count: i64,
        highlight_count: i64,
    ) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "INSERT INTO room_unread_counts (account_id, room_id, notification_count, highlight_count) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (account_id, room_id) DO UPDATE SET \
               notification_count = EXCLUDED.notification_count, \
               highlight_count = EXCLUDED.highlight_count",
        )
        .bind(account_id)
        .bind(room_id)
        .bind(notification_count)
        .bind(highlight_count)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Read this account's currently-persisted unread counts, keyed by room
    /// id. Used to seed the `axon-sync` watcher's in-memory dedup cache on
    /// startup, so a restart doesn't treat every joined room as "changed"
    /// and unconditionally re-upsert + re-broadcast it (PR review, issue
    /// #313).
    pub async fn room_unread_counts(
        &self,
        account_id: Uuid,
    ) -> Result<HashMap<String, (i64, i64)>, StoreError> {
        let rows: Vec<(String, i64, i64)> =
            sqlx_core::query_as::query_as::<Postgres, (String, i64, i64)>(
                "SELECT room_id, notification_count, highlight_count FROM room_unread_counts \
             WHERE account_id = $1",
            )
            .bind(account_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(room_id, notification_count, highlight_count)| {
                (room_id, (notification_count, highlight_count))
            })
            .collect())
    }

    /// Delete this account's `room_unread_counts` rows for any room *not* in
    /// `joined_room_ids` — the durable counterpart to the `axon-sync`
    /// watcher's in-memory cache pruning, so a room the account has left
    /// doesn't leave a row behind forever (previously only `ON DELETE
    /// CASCADE` on account deletion cleaned these up; PR review, issue
    /// #313). An empty `joined_room_ids` deletes every row for the account,
    /// which is correct when the account currently has no joined rooms at
    /// all.
    pub async fn delete_stale_room_unread_counts(
        &self,
        account_id: Uuid,
        joined_room_ids: &[String],
    ) -> Result<(), StoreError> {
        sqlx_core::query::query(
            "DELETE FROM room_unread_counts WHERE account_id = $1 AND room_id <> ALL($2)",
        )
        .bind(account_id)
        .bind(joined_room_ids)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
