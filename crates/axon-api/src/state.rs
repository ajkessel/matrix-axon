//! Shared router state.
//!
//! Handlers extract the pieces they need via [`FromRef`] rather than the whole
//! [`AppState`], so adding a field is a one-line change here plus the new
//! extractor — existing handlers that pull `State<Store>` are untouched. The
//! live-event sender is exactly that: the `/v1/ws` handler pulls
//! `State<broadcast::Sender<LiveFrame>>`, the read handlers are unchanged.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axon_core::LiveFrame;
use axon_store::Store;
use axum::extract::FromRef;
use tokio::sync::broadcast;

use crate::auth::TokenVerifier;
use crate::backfill::{BackfillStatusProvider, NoBackfillStatus};
use crate::devices::DeviceListService;
use crate::lifecycle::AccountLifecycle;
use crate::media::MediaProxy;
use crate::member_profiles::{MemberProfileService, NoopMemberProfileService};
use crate::oauth::OAuthRuntime;
use crate::search::SearchQuery;
use crate::sender::{EphemeralSender, MessageSender, SendError};
use crate::trust::SenderTrustService;
use crate::uploads::{
    StageUploadError, StageUploadRequest, StagedUpload, StagedUploadService, UploadStream,
};
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
    /// Outbound-ephemeral port for the read-receipt and typing-notice routes
    /// (ADR 0067 / ADR 0068 M19a). Defaults to a no-op so tests and
    /// non-sync configurations that don't inject one keep compiling; the
    /// binary overrides it via [`with_ephemeral`](Self::with_ephemeral).
    pub ephemeral: Arc<dyn EphemeralSender>,
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
    /// Device-list port for the `GET /v1/accounts/{id}/devices` handler (M16,
    /// ADR 0060). Injected by the binary via an adapter over the sync engine,
    /// same as `trust`.
    pub devices: Arc<dyn DeviceListService>,
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
    /// Cached room-member profile port for best-effort avatar enrichment on
    /// `GET /v1/accounts/{id}/rooms/{room_id}/members`. Defaults to a no-op so
    /// tests and non-sync configurations keep returning store-backed rows.
    pub member_profiles: Arc<dyn MemberProfileService>,
    /// Staged-upload port for `POST`/`DELETE /v1/accounts/{id}/media/uploads`.
    /// The concrete implementation streams bytes to durable local storage and
    /// records metadata in Postgres.
    pub uploads: Arc<dyn StagedUploadService>,
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
    /// One-time first-credential web bootstrap. Present only when the server
    /// was started interactively, the operator explicitly armed it, and the DB
    /// had no accounts or credentials at boot.
    pub bootstrap: Option<BootstrapConfig>,
}

#[derive(Clone)]
pub struct BootstrapConfig {
    pub allow_remote: bool,
    access_code: String,
    failed_url_attempts: Arc<AtomicUsize>,
    pub web_client_url: Option<String>,
}

pub const BOOTSTRAP_MAX_WRONG_URLS: usize = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootstrapAccessCodeError {
    Invalid,
    Locked,
}

/// The bootstrap surface is locked (too many wrong URL attempts). Distinct
/// from [`BootstrapAccessCodeError`] because [`BootstrapConfig::ensure_unlocked`]
/// only ever checks the attempt counter — it never compares a submitted code,
/// so it can never produce an `Invalid` result. Giving it its own
/// single-variant type makes that structural rather than an `unreachable!()`
/// a caller's `match` has to carry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootstrapLocked;

impl From<BootstrapLocked> for BootstrapAccessCodeError {
    fn from(_: BootstrapLocked) -> Self {
        BootstrapAccessCodeError::Locked
    }
}

impl BootstrapConfig {
    pub fn new(allow_remote: bool, access_code: String, web_client_url: Option<String>) -> Self {
        Self {
            allow_remote,
            access_code,
            failed_url_attempts: Arc::new(AtomicUsize::new(0)),
            web_client_url,
        }
    }

    pub fn access_code(&self) -> &str {
        &self.access_code
    }

    pub fn url_path(&self) -> String {
        format!("/bootstrap/{}", self.access_code)
    }

    pub fn ensure_unlocked(&self) -> Result<(), BootstrapLocked> {
        if self.failed_url_attempts.load(Ordering::Relaxed) >= BOOTSTRAP_MAX_WRONG_URLS {
            Err(BootstrapLocked)
        } else {
            Ok(())
        }
    }

    pub fn failed_url_attempts(&self) -> usize {
        self.failed_url_attempts.load(Ordering::Relaxed)
    }

    pub fn validate_access_code(&self, submitted: &str) -> Result<(), BootstrapAccessCodeError> {
        self.ensure_unlocked()?;
        // Constant-time compare: the 6-attempt lockout already bounds brute
        // force, but the access code is otherwise unhashed and held in
        // memory for the process lifetime, so there's no reason to leak
        // per-byte timing on top of that.
        if constant_time_eq(submitted.as_bytes(), self.access_code.as_bytes()) {
            return Ok(());
        }

        let attempts = self.failed_url_attempts.fetch_add(1, Ordering::Relaxed) + 1;
        if attempts >= BOOTSTRAP_MAX_WRONG_URLS {
            Err(BootstrapAccessCodeError::Locked)
        } else {
            Err(BootstrapAccessCodeError::Invalid)
        }
    }
}

/// Constant-time byte-slice comparison. Always scans both slices in full
/// regardless of where (or whether) they differ, so equality can't be
/// inferred from how long the compare took.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
        devices: Arc<dyn DeviceListService>,
        verifier: Arc<dyn TokenVerifier>,
        media: Arc<dyn MediaProxy>,
        search: Option<Arc<dyn SearchQuery>>,
    ) -> Self {
        Self {
            store,
            live,
            sender,
            ephemeral: Arc::new(NoopEphemeralSender),
            lifecycle,
            verify,
            trust,
            devices,
            verifier,
            ws_revalidation_interval: DEFAULT_WS_REVALIDATION_INTERVAL,
            media,
            member_profiles: Arc::new(NoopMemberProfileService),
            uploads: Arc::new(DisabledStagedUploads),
            search,
            backfill_status: Arc::new(NoBackfillStatus),
            oauth: None,
            bootstrap: None,
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

    /// Inject the staged-upload service (`POST`/`DELETE media/uploads`). The
    /// binary calls this with the filesystem-backed implementation; tests that
    /// don't touch the route keep the default disabled stub.
    pub fn with_staged_uploads(mut self, uploads: Arc<dyn StagedUploadService>) -> Self {
        self.uploads = uploads;
        self
    }

    /// Inject the ephemeral-outbound port (read receipts, typing notices;
    /// ADR 0067 / ADR 0068 M19a). The binary calls this with an adapter over
    /// the sync engine's gateway, same as `sender`; tests that don't touch
    /// these routes keep the default no-op.
    pub fn with_ephemeral(mut self, ephemeral: Arc<dyn EphemeralSender>) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// Inject cached room-member profile enrichment for `/members`.
    pub fn with_member_profiles(mut self, profiles: Arc<dyn MemberProfileService>) -> Self {
        self.member_profiles = profiles;
        self
    }

    /// Enable `/v1/oauth/*` with the given runtime. The binary calls this
    /// only when `oauth.enabled = true`; tests that don't care leave the
    /// default `None` (every oauth route 404s).
    pub fn with_oauth(mut self, oauth: Arc<OAuthRuntime>) -> Self {
        self.oauth = Some(oauth);
        self
    }

    /// Arm the one-time first-credential web bootstrap.
    pub fn with_bootstrap(mut self, bootstrap: BootstrapConfig) -> Self {
        self.bootstrap = Some(bootstrap);
        self
    }
}

/// The default `ephemeral` port when the binary hasn't injected one:
/// [`AppState::new`] always sets this, and only a caller invoking
/// [`AppState::with_ephemeral`] replaces it with a real adapter.
struct NoopEphemeralSender;

#[async_trait::async_trait]
impl EphemeralSender for NoopEphemeralSender {
    async fn send_read_receipt(
        &self,
        _account_id: uuid::Uuid,
        _room_id: &str,
        _event_id: &str,
    ) -> Result<(), SendError> {
        Err(SendError::Unavailable(
            "ephemeral-outbound port is not configured".to_owned(),
        ))
    }

    async fn send_typing_notice(
        &self,
        _account_id: uuid::Uuid,
        _room_id: &str,
        _typing: bool,
    ) -> Result<(), SendError> {
        Err(SendError::Unavailable(
            "ephemeral-outbound port is not configured".to_owned(),
        ))
    }
}

struct DisabledStagedUploads;

#[async_trait::async_trait]
impl StagedUploadService for DisabledStagedUploads {
    async fn stage_upload(
        &self,
        _request: StageUploadRequest,
        _body: UploadStream,
    ) -> Result<StagedUpload, StageUploadError> {
        Err(StageUploadError::Internal(
            "staged upload service is not configured".to_owned(),
        ))
    }

    async fn delete_upload(
        &self,
        _account_id: uuid::Uuid,
        _upload_id: uuid::Uuid,
    ) -> Result<(), StageUploadError> {
        Err(StageUploadError::Internal(
            "staged upload service is not configured".to_owned(),
        ))
    }

    async fn claim_upload(
        &self,
        _account_id: uuid::Uuid,
        _upload_id: uuid::Uuid,
    ) -> Result<crate::uploads::ClaimedUpload, StageUploadError> {
        Err(StageUploadError::Internal(
            "staged upload service is not configured".to_owned(),
        ))
    }

    async fn complete_upload(
        &self,
        _account_id: uuid::Uuid,
        _upload_id: uuid::Uuid,
    ) -> Result<(), StageUploadError> {
        Err(StageUploadError::Internal(
            "staged upload service is not configured".to_owned(),
        ))
    }

    async fn release_upload(
        &self,
        _account_id: uuid::Uuid,
        _upload_id: uuid::Uuid,
    ) -> Result<(), StageUploadError> {
        Err(StageUploadError::Internal(
            "staged upload service is not configured".to_owned(),
        ))
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

impl FromRef<AppState> for Arc<dyn EphemeralSender> {
    fn from_ref(state: &AppState) -> Arc<dyn EphemeralSender> {
        state.ephemeral.clone()
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

impl FromRef<AppState> for Arc<dyn DeviceListService> {
    fn from_ref(state: &AppState) -> Arc<dyn DeviceListService> {
        state.devices.clone()
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

impl FromRef<AppState> for Arc<dyn MemberProfileService> {
    fn from_ref(state: &AppState) -> Arc<dyn MemberProfileService> {
        state.member_profiles.clone()
    }
}

impl FromRef<AppState> for Arc<dyn StagedUploadService> {
    fn from_ref(state: &AppState) -> Arc<dyn StagedUploadService> {
        state.uploads.clone()
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

impl FromRef<AppState> for Option<BootstrapConfig> {
    fn from_ref(state: &AppState) -> Option<BootstrapConfig> {
        state.bootstrap.clone()
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

#[cfg(test)]
mod tests {
    use super::{
        constant_time_eq, BootstrapAccessCodeError, BootstrapConfig, BOOTSTRAP_MAX_WRONG_URLS,
    };

    #[test]
    fn constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"ABC234", b"ABC234"));
        assert!(!constant_time_eq(b"ABC234", b"WRONG2"));
        assert!(!constant_time_eq(b"ABC234", b"ABC23"));
        assert!(!constant_time_eq(b"", b"ABC234"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn bootstrap_access_code_locks_after_six_wrong_attempts() {
        let bootstrap = BootstrapConfig::new(false, "ABC234".to_owned(), None);

        for _ in 0..(BOOTSTRAP_MAX_WRONG_URLS - 1) {
            assert_eq!(
                bootstrap.validate_access_code("WRONG2"),
                Err(BootstrapAccessCodeError::Invalid)
            );
            assert_eq!(bootstrap.validate_access_code("ABC234"), Ok(()));
        }

        assert_eq!(
            bootstrap.validate_access_code("WRONG2"),
            Err(BootstrapAccessCodeError::Locked)
        );
        assert_eq!(
            bootstrap.validate_access_code("ABC234"),
            Err(BootstrapAccessCodeError::Locked)
        );
    }
}
