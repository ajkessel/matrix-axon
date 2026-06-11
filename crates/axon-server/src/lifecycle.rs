//! Composition-root adapter: binds `axon-sync`'s concrete [`AccountLifecycle`]
//! (the runtime login engine) to `axon-api`'s lifecycle port.
//!
//! Same shape as the [`GatewayAdapter`](crate::gateway::GatewayAdapter): this
//! binary is the one place that knows both crates, so the adapter and the error
//! translation live here. `axon-api` and `axon-sync` never depend on each other.

use async_trait::async_trait;
use axon_api::{AccountLifecycle, LoginError, LogoutError};
use axon_sync::{AccountLifecycle as SyncLifecycle, LifecycleError};
use uuid::Uuid;

/// Wraps the sync engine's lifecycle so it satisfies the API's `AccountLifecycle`
/// port. The orphan rule requires a local newtype to carry the impl.
pub struct LifecycleAdapter(pub SyncLifecycle);

/// Map a sync-layer lifecycle error onto the API-layer login error (and thus an
/// HTTP status): a bad MXID → invalid request, an account mid-teardown → conflict,
/// rejected credentials → auth failure, a homeserver failure → upstream, a store
/// failure → a logged internal error.
fn map_login_err(err: LifecycleError) -> LoginError {
    match err {
        LifecycleError::InvalidUserId(msg) => LoginError::InvalidRequest(msg),
        LifecycleError::BeingDeleted(id) => {
            LoginError::Conflict(format!("account is being deleted: {id}"))
        }
        LifecycleError::AuthFailed(msg) => LoginError::AuthFailed(msg),
        LifecycleError::Upstream(msg) => LoginError::Upstream(msg),
        // A previous task for this identity hasn't terminated yet; the store dir
        // it holds can't be restaged. Transient — a logout retry reaps it.
        LifecycleError::Draining(id) => LoginError::Conflict(format!(
            "a previous session for account {id} is still shutting down; retry shortly"
        )),
        LifecycleError::Store(msg) => {
            tracing::error!(error = %msg, "store error during account login");
            LoginError::Internal
        }
        // Login resolves by identity and mints a row for a new one, so it never
        // surfaces NotFound; treat it defensively as an internal error.
        LifecycleError::NotFound(id) => {
            tracing::error!(%id, "unexpected NotFound from account login");
            LoginError::Internal
        }
    }
}

/// Map a sync-layer lifecycle error onto the API-layer logout error: an unknown id
/// → not found, an account mid-teardown → conflict, a store failure → a logged
/// internal error.
fn map_logout_err(err: LifecycleError) -> LogoutError {
    match err {
        LifecycleError::NotFound(id) => LogoutError::NotFound(format!("account {id} not found")),
        LifecycleError::BeingDeleted(id) => {
            LogoutError::Conflict(format!("account is being deleted: {id}"))
        }
        // The task survived cancel + abort, so the logout could not complete
        // with its postcondition intact. Transient — retrying reaps it again.
        LifecycleError::Draining(id) => LogoutError::Conflict(format!(
            "the session for account {id} is still shutting down; retry shortly"
        )),
        LifecycleError::Store(msg) => {
            tracing::error!(error = %msg, "store error during account logout");
            LogoutError::Internal
        }
        // Logout takes only an account id and never fails over the upstream
        // homeserver (token invalidation is best-effort), so a bad MXID, rejected
        // credential, or upstream error can't arise; treat them defensively as an
        // internal error.
        other => {
            tracing::error!(error = %other, "unexpected error during account logout");
            LogoutError::Internal
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
            .map_err(map_login_err)
    }

    async fn logout(&self, account_id: Uuid) -> Result<(), LogoutError> {
        self.0.logout(account_id).await.map_err(map_logout_err)
    }
}
