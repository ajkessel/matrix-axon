//! Shared router state.
//!
//! Handlers extract the pieces they need via [`FromRef`] rather than the whole
//! [`AppState`], so adding a field is a one-line change here plus the new
//! extractor — existing handlers that pull `State<Store>` are untouched. The
//! live-event sender is exactly that: the `/v1/ws` handler pulls
//! `State<broadcast::Sender<LiveEvent>>`, the read handlers are unchanged.

use axon_core::LiveEvent;
use axon_store::Store;
use axum::extract::FromRef;
use tokio::sync::broadcast;

/// Everything the HTTP/WebSocket handlers share. Cheap to [`Clone`] (its fields
/// are all handles).
#[derive(Clone)]
pub struct AppState {
    /// Database handle.
    pub store: Store,
    /// Producer end of the live-event bus, owned by the sync engine. The
    /// `/v1/ws` handler calls [`broadcast::Sender::subscribe`] on a clone of
    /// this once per connection.
    pub live: broadcast::Sender<LiveEvent>,
}

impl AppState {
    /// Build the application state from a [`Store`] handle and the sync engine's
    /// live-event sender (see [`axon_sync::SyncEngine::live_events`]).
    pub fn new(store: Store, live: broadcast::Sender<LiveEvent>) -> Self {
        Self { store, live }
    }
}

impl FromRef<AppState> for Store {
    fn from_ref(state: &AppState) -> Store {
        state.store.clone()
    }
}

impl FromRef<AppState> for broadcast::Sender<LiveEvent> {
    fn from_ref(state: &AppState) -> broadcast::Sender<LiveEvent> {
        state.live.clone()
    }
}
