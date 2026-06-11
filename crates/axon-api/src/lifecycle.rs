//! The account-lifecycle port the lifecycle handlers depend on.
//!
//! Like [`MessageSender`](crate::sender::MessageSender), `axon-api` defines the
//! capability it *needs* — adding/reactivating a Matrix account at runtime —
//! rather than depending on whatever provides it. The real implementation lives
//! in `axon-sync`, adapted onto this port by `axon-server`, so this crate stays
//! free of `axon-sync` and `matrix-sdk`.
//!
//! Operations return the affected Axon `account_id` on success; failures are
//! [`LoginError`], whose variants map 1:1 to HTTP status in
//! [`response`](crate::response).

use async_trait::async_trait;
use uuid::Uuid;

/// What can go wrong logging an account in. Deliberately small and HTTP-shaped:
/// the adapter that implements [`AccountLifecycle`] collapses its richer backend
/// error into one of these so the handler layer maps a stable set of statuses.
#[derive(Debug)]
pub enum LoginError {
    /// The request was malformed — e.g. `username` is not a valid Matrix user ID.
    /// → `400`.
    InvalidRequest(String),
    /// The homeserver rejected the supplied credentials. → `401`.
    AuthFailed(String),
    /// An account for this identity is already active (or being deleted), so it
    /// must be logged out before logging in again. → `409`.
    Conflict(String),
    /// The upstream homeserver was unreachable or failed the login. → `502`.
    Upstream(String),
    /// An internal failure (e.g. the store). The detail is logged, not returned.
    /// → `500`.
    Internal,
}

/// What can go wrong logging an account out. Like [`LoginError`], small and
/// HTTP-shaped; the adapter collapses its backend error into one of these.
#[derive(Debug)]
pub enum LogoutError {
    /// No account exists for the given id. → `404`.
    NotFound(String),
    /// The account is mid-teardown (a delete is in flight), so it can't be logged
    /// out. → `409`.
    Conflict(String),
    /// An internal failure (e.g. the store). The detail is logged, not returned.
    /// → `500`.
    ///
    /// There is deliberately no upstream/502 variant: the backend invalidates the
    /// device token upstream best-effort and never fails logout over it, so such
    /// a variant would be unreachable API surface.
    Internal,
}

/// Adds, reactivates, or stops a Matrix account at runtime. Implemented outside
/// this crate; held in [`AppState`](crate::AppState) as `Arc<dyn AccountLifecycle>`.
#[async_trait]
pub trait AccountLifecycle: Send + Sync {
    /// Log in (or reactivate) the account identified by `(homeserver_url,
    /// username)`, where `username` is a full Matrix user ID. Returns the Axon
    /// `account_id` of the now-active account.
    async fn login(
        &self,
        homeserver_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Uuid, LoginError>;

    /// Log the account `account_id` out: stop syncing it, invalidate its device
    /// token upstream, and move it to a logged-out (`deactivated`) state, retaining
    /// all of its data so a later [`login`](Self::login) reactivates it. Idempotent
    /// — logging out an already-logged-out account succeeds.
    async fn logout(&self, account_id: Uuid) -> Result<(), LogoutError>;
}
