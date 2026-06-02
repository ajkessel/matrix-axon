//! Postgres-backed event store, room state, and account data.
//!
//! `axon-store` owns all Postgres connections. Other crates (notably
//! `axon-api`) consume a cheaply-cloneable [`Store`] handle rather than talking
//! to the database directly. Migrations live under `migrations/` and are
//! embedded into the binary at compile time, so a deployed `axon` needs no
//! migration files on disk.

mod accounts;
mod error;
mod events;
mod migrations;

pub use accounts::Account;
pub use error::StoreError;
pub use events::{EventCiphertext, EventCrypto, NewEvent, PendingUtd, TimelineCursor, TimelineRow};

use sqlx_postgres::{PgPool, PgPoolOptions};

/// A handle to the Axon Postgres database.
///
/// Cheap to [`Clone`] (the underlying pool is reference-counted), so it can be
/// shared across axum handlers via router state.
#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Connect a connection pool and run any pending migrations.
    pub async fn connect(database_url: &str, max_connections: u32) -> Result<Store, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await?;

        tracing::info!("running database migrations");
        migrations::embedded_migrator()?.run(&pool).await?;

        Ok(Store { pool })
    }

    /// Access the underlying connection pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
