//! Shared router state.
//!
//! Handlers extract the pieces they need via [`FromRef`] rather than the whole
//! [`AppState`], so adding a field is a one-line change here plus the new
//! extractor — existing handlers that pull `State<Store>` are untouched. The
//! live-event sender is exactly that: the `/v1/ws` handler pulls
//! `State<broadcast::Sender<LiveFrame>>`, the read handlers are unchanged.

use std::sync::Arc;

use axon_core::LiveFrame;
use axon_store::Store;
use axum::extract::FromRef;
use tokio::sync::broadcast;

use crate::lifecycle::AccountLifecycle;
use crate::sender::MessageSender;
use crate::verification::VerificationService;

/// Everything the HTTP/WebSocket handlers share. Cheap to [`Clone`] (its fields
/// are all handles).
#[derive(Clone)]
pub struct AppState {
    /// Database handle.
    pub store: Store,
    /// Producer end of the live-event bus, owned by the sync engine. The
    /// `/v1/ws` handler calls [`broadcast::Sender::subscribe`] on a clone of
    /// this once per connection.
    pub live: broadcast::Sender<LiveFrame>,
    /// Outbound-message port for the mutation handlers. The concrete
    /// implementation (the sync engine's SDK gateway) is injected by the binary
    /// via an adapter, so this crate stays free of `axon-sync`/`matrix-sdk`.
    pub sender: Arc<dyn MessageSender>,
    /// Account-lifecycle port for the login handler (and later logout/delete).
    /// Injected by the binary via an adapter over the sync engine, same as
    /// `sender`.
    pub lifecycle: Arc<dyn AccountLifecycle>,
    /// Device-verification port for the `/v1/accounts/{id}/verify` handlers.
    /// Injected by the binary via an adapter over the sync engine, same as
    /// `lifecycle`.
    pub verify: Arc<dyn VerificationService>,
}

impl AppState {
    /// Build the application state from a [`Store`] handle, the sync engine's
    /// live-event sender (see `axon_sync::SyncEngine::live_events`), an
    /// outbound-message [`MessageSender`], an [`AccountLifecycle`], and a
    /// [`VerificationService`].
    pub fn new(
        store: Store,
        live: broadcast::Sender<LiveFrame>,
        sender: Arc<dyn MessageSender>,
        lifecycle: Arc<dyn AccountLifecycle>,
        verify: Arc<dyn VerificationService>,
    ) -> Self {
        Self {
            store,
            live,
            sender,
            lifecycle,
            verify,
        }
    }
}

impl FromRef<AppState> for Store {
    fn from_ref(state: &AppState) -> Store {
        state.store.clone()
    }
}

impl FromRef<AppState> for broadcast::Sender<LiveFrame> {
    fn from_ref(state: &AppState) -> broadcast::Sender<LiveFrame> {
        state.live.clone()
    }
}

impl FromRef<AppState> for Arc<dyn MessageSender> {
    fn from_ref(state: &AppState) -> Arc<dyn MessageSender> {
        state.sender.clone()
    }
}

impl FromRef<AppState> for Arc<dyn AccountLifecycle> {
    fn from_ref(state: &AppState) -> Arc<dyn AccountLifecycle> {
        state.lifecycle.clone()
    }
}

impl FromRef<AppState> for Arc<dyn VerificationService> {
    fn from_ref(state: &AppState) -> Arc<dyn VerificationService> {
        state.verify.clone()
    }
}
