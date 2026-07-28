//! Path B (native identity token) single-use replay defense (M14b, ADR 0054).
//!
//! Path B hands axon a provider-signed identity token directly; the
//! consumption insert itself *is* the concurrency control (`ON CONFLICT DO
//! NOTHING`, then check rows-affected) — not a read-then-write check — so two
//! concurrent redemptions of the same stolen token can't both win the race.

use crate::{Store, StoreError};

impl Store {
    /// Attempt to consume an identity token's replay key. Returns `true` the
    /// first time a given `(provider, replay_key)` is seen (the caller may
    /// proceed), `false` if it was already consumed (the caller must reject
    /// the token as a replay).
    pub async fn consume_identity_token(
        &self,
        provider: &str,
        replay_key: &str,
    ) -> Result<bool, StoreError> {
        let result = sqlx_core::query::query(
            "INSERT INTO oauth_consumed_identity_tokens (provider, replay_key) \
             VALUES ($1, $2) ON CONFLICT (provider, replay_key) DO NOTHING",
        )
        .bind(provider)
        .bind(replay_key)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
