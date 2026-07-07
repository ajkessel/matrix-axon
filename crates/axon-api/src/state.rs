//! Shared router state.
//!
//! Handlers extract the pieces they need via [`FromRef`] rather than the whole
//! [`AppState`], so adding a field is a one-line change here plus the new
//! extractor — existing handlers that pull `State<Store>` are untouched. The
//! live-event sender is exactly that: the `/v1/ws` handler pulls
//! `State<broadcast::Sender<LiveFrame>>`, the read handlers are unchanged.

use std::sync::Arc;
use std::time::Duration;

use axon_core::LiveFrame;
use axon_store::Store;
use axum::extract::FromRef;
use tokio::sync::broadcast;

use crate::auth::TokenVerifier;
use crate::backfill::{BackfillStatusProvider, NoBackfillStatus};
use crate::lifecycle::AccountLifecycle;
use crate::media::MediaProxy;
use crate::oauth::OAuthRuntime;
use crate::search::SearchQuery;
use crate::sender::MessageSender;
use crate::trust::SenderTrustService;
use crate::verification::VerificationService;

/// How often an established `/v1/ws` socket re-checks its bearer token. Token
/// revocation happens out-of-process (the `axon token revoke` CLI writes the DB,
/// the running server never gets an in-process signal), so a live socket has to
/// poll to notice it; this bounds how long a revoked client keeps receiving
/// frames. Tests shorten it via [`AppState::with_ws_revalidation_interval`].
const DEFAULT_WS_REVALIDATION_INTERVAL: Duration = Duration::from_secs(30);

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
    /// Sender-trust port for the per-event verification-bundle handler (M7c).
    /// Injected by the binary via an adapter over the sync engine, same as
    /// `verify`.
    pub trust: Arc<dyn SenderTrustService>,
    /// Bearer-token verifier (M7b): the seam the `/v1/` auth gate (the
    /// `require_bearer` middleware and the WebSocket upgrade) checks every
    /// request against. The shipped implementation is
    /// [`StoreTokenVerifier`](crate::auth::StoreTokenVerifier); a future OAuth
    /// issuer slots in here without touching any route.
    pub verifier: Arc<dyn TokenVerifier>,
    /// How often a live `/v1/ws` socket revalidates its token (see
    /// [`DEFAULT_WS_REVALIDATION_INTERVAL`]).
    ws_revalidation_interval: Duration,
    /// Media-proxy port for the `GET /v1/media/{account_id}/…` handler. The
    /// concrete implementation fetches via the SDK client's authenticated
    /// connection and is injected by the binary via an adapter.
    pub media: Arc<dyn MediaProxy>,
    /// Full-text-search port for `GET /v1/search` (M9b). `None` when search is
    /// disabled (`search.enabled = false`), in which case the handler returns
    /// `503`. The concrete implementation is an adapter over the `axon-search`
    /// Tantivy index, injected by the binary like the other ports.
    pub search: Option<Arc<dyn SearchQuery>>,
    /// Backfill status port for `GET /v1/status` (M10). Defaults to a no-op
    /// (always-healthy) provider; the binary injects an adapter over the sync
    /// engine's `BackfillHealth` via [`with_backfill_status`](Self::with_backfill_status).
    pub backfill_status: Arc<dyn BackfillStatusProvider>,
    /// OAuth 2.0 authorization-server runtime for `/v1/oauth/*` (M14, ADR
    /// 0054). `None` when `oauth.enabled = false`, in which case every oauth
    /// handler (and the rate-limiting layer in front of them) returns `404`
    /// — the same "disabled surface" pattern as `search`.
    pub oauth: Option<Arc<OAuthRuntime>>,
}

impl AppState {
    /// Build the application state from a [`Store`] handle, the sync engine's
    /// live-event sender (see `axon_sync::SyncEngine::live_events`), an
    /// outbound-message [`MessageSender`], an [`AccountLifecycle`], and a
    /// [`VerificationService`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        live: broadcast::Sender<LiveFrame>,
        sender: Arc<dyn MessageSender>,
        lifecycle: Arc<dyn AccountLifecycle>,
        verify: Arc<dyn VerificationService>,
        trust: Arc<dyn SenderTrustService>,
        verifier: Arc<dyn TokenVerifier>,
        media: Arc<dyn MediaProxy>,
        search: Option<Arc<dyn SearchQuery>>,
    ) -> Self {
        Self {
            store,
            live,
            sender,
            lifecycle,
            verify,
            trust,
            verifier,
            ws_revalidation_interval: DEFAULT_WS_REVALIDATION_INTERVAL,
            media,
            search,
            backfill_status: Arc::new(NoBackfillStatus),
            oauth: None,
        }
    }

    /// Override the `/v1/ws` token-revalidation cadence. Production uses the
    /// default; tests set a short interval to exercise revocation of a live
    /// socket without waiting the full default.
    pub fn with_ws_revalidation_interval(mut self, interval: Duration) -> Self {
        self.ws_revalidation_interval = interval;
        self
    }

    /// Inject the backfill status provider (`GET /v1/status`). The binary calls
    /// this with an adapter over the sync engine's `BackfillHealth`; tests that
    /// don't care keep the default no-op provider.
    pub fn with_backfill_status(mut self, provider: Arc<dyn BackfillStatusProvider>) -> Self {
        self.backfill_status = provider;
        self
    }

    /// Enable `/v1/oauth/*` with the given runtime. The binary calls this
    /// only when `oauth.enabled = true`; tests that don't care leave the
    /// default `None` (every oauth route 404s).
    pub fn with_oauth(mut self, oauth: Arc<OAuthRuntime>) -> Self {
        self.oauth = Some(oauth);
        self
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

impl FromRef<AppState> for Arc<dyn SenderTrustService> {
    fn from_ref(state: &AppState) -> Arc<dyn SenderTrustService> {
        state.trust.clone()
    }
}

impl FromRef<AppState> for Arc<dyn TokenVerifier> {
    fn from_ref(state: &AppState) -> Arc<dyn TokenVerifier> {
        state.verifier.clone()
    }
}

impl FromRef<AppState> for Arc<dyn MediaProxy> {
    fn from_ref(state: &AppState) -> Arc<dyn MediaProxy> {
        state.media.clone()
    }
}

impl FromRef<AppState> for Option<Arc<dyn SearchQuery>> {
    fn from_ref(state: &AppState) -> Option<Arc<dyn SearchQuery>> {
        state.search.clone()
    }
}

impl FromRef<AppState> for Arc<dyn BackfillStatusProvider> {
    fn from_ref(state: &AppState) -> Arc<dyn BackfillStatusProvider> {
        state.backfill_status.clone()
    }
}

impl FromRef<AppState> for Option<Arc<OAuthRuntime>> {
    fn from_ref(state: &AppState) -> Option<Arc<OAuthRuntime>> {
        state.oauth.clone()
    }
}

/// The `/v1/ws` token-revalidation cadence, extracted as router state by the
/// WebSocket handler.
#[derive(Clone, Copy)]
pub struct WsRevalidationInterval(pub Duration);

impl FromRef<AppState> for WsRevalidationInterval {
    fn from_ref(state: &AppState) -> WsRevalidationInterval {
        WsRevalidationInterval(state.ws_revalidation_interval)
    }
}
