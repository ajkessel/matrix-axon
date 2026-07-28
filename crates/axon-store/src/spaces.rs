//! Space-hierarchy reads: `m.space.child` / `m.space.parent` resolved and
//! enriched with the referenced room's own cached display fields (issue #404,
//! ADR 0084).
//!
//! Axon already stores these state tuples like any other `room_state` row
//! (ADR 0016); nothing resolved them into a hierarchy before this. Unlike the
//! generic [`Store::room_state_of_type`](crate::Store::room_state_of_type),
//! space children need MSC1772 ordering (`order` string, then `origin_ts`,
//! then `room_id`) rather than the plain `state_key` order, so they get their
//! own queries here instead of reusing the generic read.

use serde_json::Value;
use sqlx_core::row::Row;
use sqlx_postgres::{PgRow, Postgres};
use uuid::Uuid;

use crate::{Store, StoreError};

/// Hard cap on the number of rows [`Store::space_children`] and
/// [`Store::space_parents`] return. These are unpaginated single-shot reads,
/// but a remote room can hold an arbitrary number of distinct
/// `m.space.child`/`m.space.parent` state keys (bounded only by upstream
/// federation, not by anything Axon controls) — without a cap, one request
/// would drive unbounded database work, allocation, and response size (three
/// correlated enrichment lookups per row). Mirrors the same
/// hostile/pathological-room guard as `RELATION_READ_CAP` in `events.rs`;
/// children are truncated *after* the MSC1772 sort, so the cap always keeps
/// the leading (highest-priority) entries rather than an arbitrary subset.
const SPACE_HIERARCHY_CAP: i64 = 1000;

/// Pull `content.via` (a list of server names) out of a state event's content,
/// defaulting to empty when absent or malformed. Shared by both row types
/// below since `m.space.child` and `m.space.parent` carry the same `via`
/// field.
fn via_from_content(content: &Option<Value>) -> Vec<String> {
    content
        .as_ref()
        .and_then(|c| c.get("via"))
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// One `m.space.child` tuple (a room `room_id` is spaced into), enriched with
/// the child room's own cached display fields — `None` when Axon doesn't
/// know that room (e.g. the account was never joined to it).
#[derive(Debug, Clone)]
pub struct SpaceChildRow {
    /// The child room's id (the `m.space.child` state key).
    pub room_id: String,
    /// Federation-resolution hints (`content.via`) for a child Axon has no
    /// direct path to.
    pub via: Vec<String>,
    /// MSC1772 sort key, if set. Absent sorts last (see [`Store::space_children`]).
    pub order: Option<String>,
    /// Whether the child is flagged as suggested (`content.suggested`,
    /// defaults to `false` when absent).
    pub suggested: bool,
    /// The child room's cached `m.room.name`, if known.
    pub name: Option<String>,
    /// The child room's cached `m.room.avatar` `url`, if known.
    pub avatar_url: Option<String>,
    /// The child room's `m.room.create` `type` (e.g. `m.space`), if known.
    pub room_type: Option<String>,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for SpaceChildRow {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        let content: Option<Value> = row.try_get("content")?;
        let order = content
            .as_ref()
            .and_then(|c| c.get("order"))
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let suggested = content
            .as_ref()
            .and_then(|c| c.get("suggested"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(SpaceChildRow {
            room_id: row.try_get("room_id")?,
            via: via_from_content(&content),
            order,
            suggested,
            name: row.try_get("name")?,
            avatar_url: row.try_get("avatar_url")?,
            room_type: row.try_get("room_type")?,
        })
    }
}

/// One `m.space.parent` tuple (a space `room_id` claims to belong to),
/// enriched the same way as [`SpaceChildRow`].
#[derive(Debug, Clone)]
pub struct SpaceParentRow {
    /// The parent space's room id (the `m.space.parent` state key).
    pub room_id: String,
    /// Federation-resolution hints (`content.via`).
    pub via: Vec<String>,
    /// Whether this parent is the canonical one (`content.canonical`,
    /// defaults to `false` when absent).
    pub canonical: bool,
    /// The parent's cached `m.room.name`, if known.
    pub name: Option<String>,
    /// The parent's cached `m.room.avatar` `url`, if known.
    pub avatar_url: Option<String>,
    /// The parent's `m.room.create` `type` (normally `m.space`), if known.
    pub room_type: Option<String>,
}

impl sqlx_core::from_row::FromRow<'_, PgRow> for SpaceParentRow {
    fn from_row(row: &PgRow) -> Result<Self, sqlx_core::Error> {
        let content: Option<Value> = row.try_get("content")?;
        let canonical = content
            .as_ref()
            .and_then(|c| c.get("canonical"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(SpaceParentRow {
            room_id: row.try_get("room_id")?,
            via: via_from_content(&content),
            canonical,
            name: row.try_get("name")?,
            avatar_url: row.try_get("avatar_url")?,
            room_type: row.try_get("room_type")?,
        })
    }
}

impl Store {
    /// List `room_id`'s space children (`m.space.child`), MSC1772-ordered:
    /// `order` string ascending (a child with no `order` sorts after every
    /// child that has one), then `origin_ts`, then child `room_id` as a
    /// final tiebreak. Each child is enriched with its own cached
    /// name/avatar/room_type via a correlated point lookup into `room_state`
    /// keyed on the child's `room_id` — `None` when Axon doesn't know that
    /// room. An unknown or childless `room_id` yields an empty list. Capped at
    /// [`SPACE_HIERARCHY_CAP`] rows (applied after the MSC1772 sort, so a
    /// truncated result still keeps the leading/highest-priority children).
    pub async fn space_children(
        &self,
        account_id: Uuid,
        room_id: &str,
    ) -> Result<Vec<SpaceChildRow>, StoreError> {
        let sql = format!(
            "SELECT sc.state_key AS room_id, sc.content, \
                    (SELECT rs.content->>'name' FROM room_state rs \
                       WHERE rs.account_id = sc.account_id AND rs.room_id = sc.state_key \
                         AND rs.event_type = 'm.room.name' AND rs.state_key = '') AS name, \
                    (SELECT rs.content->>'url' FROM room_state rs \
                       WHERE rs.account_id = sc.account_id AND rs.room_id = sc.state_key \
                         AND rs.event_type = 'm.room.avatar' AND rs.state_key = '') AS avatar_url, \
                    (SELECT rs.content->>'type' FROM room_state rs \
                       WHERE rs.account_id = sc.account_id AND rs.room_id = sc.state_key \
                         AND rs.event_type = 'm.room.create' AND rs.state_key = '') AS room_type \
             FROM room_state sc \
             WHERE sc.account_id = $1 AND sc.room_id = $2 AND sc.event_type = 'm.space.child' \
             ORDER BY (sc.content->>'order') ASC NULLS LAST, sc.origin_ts ASC, sc.state_key ASC \
             LIMIT {SPACE_HIERARCHY_CAP}"
        );
        let rows = sqlx_core::query_as::query_as::<Postgres, SpaceChildRow>(&sql)
            .bind(account_id)
            .bind(room_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }

    /// List the spaces `room_id` claims as a parent (`m.space.parent`) — the
    /// reverse lookup of [`space_children`](Self::space_children). No
    /// ordering is spec-defined here (unlike children's `order` field), so
    /// results are ordered by parent `room_id` for determinism. Same
    /// per-parent enrichment as children, and the same [`SPACE_HIERARCHY_CAP`]
    /// row cap.
    pub async fn space_parents(
        &self,
        account_id: Uuid,
        room_id: &str,
    ) -> Result<Vec<SpaceParentRow>, StoreError> {
        let sql = format!(
            "SELECT sp.state_key AS room_id, sp.content, \
                    (SELECT rs.content->>'name' FROM room_state rs \
                       WHERE rs.account_id = sp.account_id AND rs.room_id = sp.state_key \
                         AND rs.event_type = 'm.room.name' AND rs.state_key = '') AS name, \
                    (SELECT rs.content->>'url' FROM room_state rs \
                       WHERE rs.account_id = sp.account_id AND rs.room_id = sp.state_key \
                         AND rs.event_type = 'm.room.avatar' AND rs.state_key = '') AS avatar_url, \
                    (SELECT rs.content->>'type' FROM room_state rs \
                       WHERE rs.account_id = sp.account_id AND rs.room_id = sp.state_key \
                         AND rs.event_type = 'm.room.create' AND rs.state_key = '') AS room_type \
             FROM room_state sp \
             WHERE sp.account_id = $1 AND sp.room_id = $2 AND sp.event_type = 'm.space.parent' \
             ORDER BY sp.state_key ASC \
             LIMIT {SPACE_HIERARCHY_CAP}"
        );
        let rows = sqlx_core::query_as::query_as::<Postgres, SpaceParentRow>(&sql)
            .bind(account_id)
            .bind(room_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows)
    }
}
