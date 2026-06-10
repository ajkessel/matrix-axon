//! Composition-root adapter: binds `axon-sync`'s concrete [`AccountLifecycle`]
//! (the runtime login engine) to `axon-api`'s lifecycle port.
//!
//! Same shape as the [`GatewayAdapter`](crate::gateway::GatewayAdapter): this
//! binary is the one place that knows both crates, so the adapter and the error
//! translation live here. `axon-api` and `axon-sync` never depend on each other.

use async_trait::async_trait;
use axon_api::{AccountLifecycle, LoginError};
use axon_sync::{AccountLifecycle as SyncLifecycle, LifecycleError};
use uuid::Uuid;

/// Wraps the sync engine's lifecycle so it satisfies the API's `AccountLifecycle`
/// port. The orphan rule requires a local newtype to carry the impl.
pub struct LifecycleAdapter(pub SyncLifecycle);

/// Map a sync-layer lifecycle error onto the API-layer login error (and thus an
/// HTTP status): a bad MXID → invalid request, an account mid-teardown → conflict,
/// rejected credentials → auth failure, a homeserver failure → upstream, a store
/// failure → a logged internal error.
fn map_err(err: LifecycleError) -> LoginError {
    match err {
        LifecycleError::InvalidUserId(msg) => LoginError::InvalidRequest(msg),
        LifecycleError::BeingDeleted(id) => {
            LoginError::Conflict(format!("account is being deleted: {id}"))
        }
        LifecycleError::AuthFailed(msg) => LoginError::AuthFailed(msg),
        LifecycleError::Upstream(msg) => LoginError::Upstream(msg),
        LifecycleError::Store(msg) => {
            tracing::error!(error = %msg, "store error during account login");
            LoginError::Internal
        }
    }
}

#[async_trait]
impl AccountLifecycle for LifecycleAdapter {
    async fn login(
        &self,
        homeserver_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Uuid, LoginError> {
        self.0
            .login(homeserver_url, username, password)
            .await
            .map_err(map_err)
    }
}
