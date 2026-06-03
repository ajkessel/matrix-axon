//! Shared router state.
//!
//! Handlers extract the pieces they need via [`FromRef`] rather than the whole
//! [`AppState`], so adding a field (e.g. a future live-event broadcast sender)
//! is a one-line change here plus the new extractor — existing handlers that
//! pull `State<Store>` are untouched.

use axon_store::Store;
use axum::extract::FromRef;

/// Everything the HTTP/WebSocket handlers share. Cheap to [`Clone`] (its fields
/// are all handles).
#[derive(Clone)]
pub struct AppState {
    /// Database handle.
    pub store: Store,
}

impl AppState {
    /// Build the application state from a [`Store`] handle.
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

impl FromRef<AppState> for Store {
    fn from_ref(state: &AppState) -> Store {
        state.store.clone()
    }
}
