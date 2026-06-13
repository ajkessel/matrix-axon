//! Composition-root adapter: binds `axon-sync`'s concrete [`AccountLifecycle`]
//! (the runtime login engine) to `axon-api`'s lifecycle port.
//!
//! Same shape as the [`GatewayAdapter`](crate::gateway::GatewayAdapter): this
//! binary is the one place that knows both crates, so the adapter and the error
//! translation live here. `axon-api` and `axon-sync` never depend on each other.

use async_trait::async_trait;
use axon_api::{AccountLifecycle, DeleteError, LoginError, LogoutError};
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
        // Login never surfaces these — `NotFound` (it mints a row for a new
        // identity) or `Provisioned` (a delete-only guard) — so treat them
        // defensively as an internal error.
        LifecycleError::NotFound(id) | LifecycleError::Provisioned(id) => {
            tracing::error!(%id, "unexpected lifecycle error from account login");
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

/// Map a sync-layer lifecycle error onto the API-layer delete error: an unknown id
/// → not found, a not-yet-terminated sync task → conflict, a store/dir-removal
/// failure → a logged internal error.
fn map_delete_err(err: LifecycleError) -> DeleteError {
    match err {
        LifecycleError::NotFound(id) => DeleteError::NotFound(format!("account {id} not found")),
        // The account's task survived cancel + abort, so its store dir can't be
        // treated as quiescent and the teardown stopped before removing it. The row
        // stays `deleting`; a retry (or the next boot's reconcile) finishes it.
        LifecycleError::Draining(id) => DeleteError::Conflict(format!(
            "the sync task for account {id} is still shutting down; retry shortly"
        )),
        // The account is still named by `sync.account`; deleting it would be undone
        // by boot provisioning. Surface as a conflict telling the operator to remove
        // it from config first.
        LifecycleError::Provisioned(id) => DeleteError::Conflict(format!(
            "account {id} is provisioned from config; remove it from sync.account before deleting"
        )),
        LifecycleError::Store(msg) => {
            tracing::error!(error = %msg, "store error during account delete");
            DeleteError::Internal
        }
        // Delete takes only an account id and resolves a `deleting` row by resuming
        // (never `BeingDeleted`); a bad MXID, rejected credential, or upstream error
        // can't arise. Treat them defensively as an internal error.
        other => {
            tracing::error!(error = %other, "unexpected error during account delete");
            DeleteError::Internal
        }
    }
}

#[async_trait]
impl AccountLifecycle for LifecycleAdapter {
    async fn login(
        &self,
        homeserver_url: Option<&str>,
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

    async fn delete(&self, account_id: Uuid) -> Result<(), DeleteError> {
        self.0.delete(account_id).await.map_err(map_delete_err)
    }
}
