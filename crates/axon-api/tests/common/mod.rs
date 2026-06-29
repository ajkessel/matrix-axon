//! Shared test doubles for the API integration tests.
//!
//! [`StubSender`] is an in-memory [`MessageSender`] that records the calls it
//! receives and returns a preset outcome, so the mutation handlers can be
//! exercised (routing, request decoding, error mapping) without a real
//! homeserver or sync engine.

#![allow(dead_code)] // each tests/*.rs is its own crate; not all use every helper.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axon_api::{
    AccountLifecycle, ApiError, CurrentTrust, DeleteError, FlowStage, FlowSummary, Formatted,
    LoginError, LogoutError, MediaContent, MediaError, MediaProxy, MessageSender, RecoverError,
    Relation, SendError, SenderTrustService, TokenVerifier, TrustBundle, TrustError, TrustSnapshot,
    VerificationService, VerifyError,
};
use uuid::Uuid;

/// The bearer token the test router's [`StubTokenVerifier`] accepts. Helpers
/// attach it as `Authorization: Bearer {TEST_TOKEN}` so the auth gate (M7b) lets
/// functional requests through; auth-specific tests send a wrong token or none.
pub const TEST_TOKEN: &str = "axon_test-token";

/// An in-memory [`TokenVerifier`] for tests: accepts exactly one token string
/// (while "active") and rejects everything else, so the `/v1/` auth gate can be
/// exercised without a `tokens` row. The real DB-backed `StoreTokenVerifier` is
/// covered by the axon-store token tests.
///
/// The shared `active` flag lets a test simulate out-of-process revocation of a
/// live token: grab [`revocation_handle`](Self::revocation_handle) before wrapping
/// the stub in an `Arc`, then flip it to `false` to make `verify` start rejecting.
pub struct StubTokenVerifier {
    accepted: String,
    active: Arc<AtomicBool>,
}

impl StubTokenVerifier {
    /// A verifier accepting [`TEST_TOKEN`] — the default for functional tests.
    pub fn ok() -> Self {
        Self {
            accepted: TEST_TOKEN.to_owned(),
            active: Arc::new(AtomicBool::new(true)),
        }
    }

    /// A handle to this stub's active flag. Store `false` into it to revoke the
    /// token (subsequent `verify` calls return `Ok(false)`).
    pub fn revocation_handle(&self) -> Arc<AtomicBool> {
        self.active.clone()
    }
}

#[async_trait]
impl TokenVerifier for StubTokenVerifier {
    async fn verify(&self, token: &str) -> Result<bool, ApiError> {
        Ok(token == self.accepted && self.active.load(Ordering::SeqCst))
    }
}

/// One recorded call to the stub, with the arguments the handler passed through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Send {
        account_id: Uuid,
        room_id: String,
        body: String,
        /// The `(format, formatted_body)` the handler passed, if any.
        formatted: Option<(String, String)>,
        /// The `reply_to` event id the handler passed, if any.
        reply_to: Option<String>,
        /// The `thread_root` event id the handler passed, if any.
        thread_root: Option<String>,
    },
    Edit {
        account_id: Uuid,
        room_id: String,
        event_id: String,
        body: String,
        /// The `(format, formatted_body)` the handler passed, if any.
        formatted: Option<(String, String)>,
    },
    Redact {
        account_id: Uuid,
        room_id: String,
        event_id: String,
        reason: Option<String>,
    },
    React {
        account_id: Uuid,
        room_id: String,
        event_id: String,
        key: String,
    },
}

/// The outcome the stub returns for every call. `Clone` (unlike [`SendError`])
/// so one stub can answer repeated calls.
#[derive(Clone)]
pub enum Outcome {
    Ok(String),
    NotFound(String),
    Forbidden(String),
    Unavailable(String),
    Invalid(String),
    Upstream(String),
}

impl Outcome {
    fn to_result(&self) -> Result<String, SendError> {
        match self {
            Outcome::Ok(id) => Ok(id.clone()),
            Outcome::NotFound(m) => Err(SendError::NotFound(m.clone())),
            Outcome::Forbidden(m) => Err(SendError::Forbidden(m.clone())),
            Outcome::Unavailable(m) => Err(SendError::Unavailable(m.clone())),
            Outcome::Invalid(m) => Err(SendError::Invalid(m.clone())),
            Outcome::Upstream(m) => Err(SendError::Upstream(m.clone())),
        }
    }
}

/// An in-memory [`MessageSender`] for tests.
pub struct StubSender {
    outcome: Outcome,
    calls: Mutex<Vec<Call>>,
}

impl StubSender {
    /// A stub that returns `Ok(event_id)` for every call.
    pub fn ok(event_id: &str) -> Self {
        Self {
            outcome: Outcome::Ok(event_id.to_owned()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub that returns the given failure for every call.
    pub fn failing(outcome: Outcome) -> Self {
        Self {
            outcome,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The calls recorded so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

/// The outcome the [`StubLifecycle`] returns for every `login` call. `Clone` so
/// one stub can answer repeated calls; mirrors [`LoginError`]'s variants.
#[derive(Clone)]
pub enum LoginOutcome {
    Ok(Uuid),
    InvalidRequest(String),
    AuthFailed(String),
    Conflict(String),
    Upstream(String),
    Internal,
}

impl LoginOutcome {
    fn to_result(&self) -> Result<Uuid, LoginError> {
        match self {
            LoginOutcome::Ok(id) => Ok(*id),
            LoginOutcome::InvalidRequest(m) => Err(LoginError::InvalidRequest(m.clone())),
            LoginOutcome::AuthFailed(m) => Err(LoginError::AuthFailed(m.clone())),
            LoginOutcome::Conflict(m) => Err(LoginError::Conflict(m.clone())),
            LoginOutcome::Upstream(m) => Err(LoginError::Upstream(m.clone())),
            LoginOutcome::Internal => Err(LoginError::Internal),
        }
    }
}

/// One recorded `login` call, with the arguments the handler passed through.
/// `homeserver_url` is `None` when the request omitted it (server-side
/// discovery — the stub records exactly what the handler forwarded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginCall {
    pub homeserver_url: Option<String>,
    pub username: String,
    pub password: String,
}

/// The outcome the [`StubLifecycle`] returns for every `logout` call. `Clone` so
/// one stub can answer repeated calls; mirrors [`LogoutError`]'s variants.
#[derive(Clone)]
pub enum LogoutOutcome {
    Ok,
    NotFound(String),
    Conflict(String),
    Internal,
}

impl LogoutOutcome {
    fn to_result(&self) -> Result<(), LogoutError> {
        match self {
            LogoutOutcome::Ok => Ok(()),
            LogoutOutcome::NotFound(m) => Err(LogoutError::NotFound(m.clone())),
            LogoutOutcome::Conflict(m) => Err(LogoutError::Conflict(m.clone())),
            LogoutOutcome::Internal => Err(LogoutError::Internal),
        }
    }
}

/// The outcome the [`StubLifecycle`] returns for every `delete` call. `Clone` so
/// one stub can answer repeated calls; mirrors [`DeleteError`]'s variants.
#[derive(Clone)]
pub enum DeleteOutcome {
    Ok,
    NotFound(String),
    Conflict(String),
    Internal,
}

impl DeleteOutcome {
    fn to_result(&self) -> Result<(), DeleteError> {
        match self {
            DeleteOutcome::Ok => Ok(()),
            DeleteOutcome::NotFound(m) => Err(DeleteError::NotFound(m.clone())),
            DeleteOutcome::Conflict(m) => Err(DeleteError::Conflict(m.clone())),
            DeleteOutcome::Internal => Err(DeleteError::Internal),
        }
    }
}

/// The outcome the [`StubLifecycle`] returns for every `recover` call. `Clone` so
/// one stub can answer repeated calls; mirrors [`RecoverError`]'s variants.
#[derive(Clone)]
pub enum RecoverOutcome {
    Ok,
    NotFound(String),
    Conflict(String),
    BadRequest(String),
    Internal,
}

impl RecoverOutcome {
    fn to_result(&self) -> Result<(), RecoverError> {
        match self {
            RecoverOutcome::Ok => Ok(()),
            RecoverOutcome::NotFound(m) => Err(RecoverError::NotFound(m.clone())),
            RecoverOutcome::Conflict(m) => Err(RecoverError::Conflict(m.clone())),
            RecoverOutcome::BadRequest(m) => Err(RecoverError::BadRequest(m.clone())),
            RecoverOutcome::Internal => Err(RecoverError::Internal),
        }
    }
}

/// An in-memory [`AccountLifecycle`] for tests: records each
/// `login`/`logout`/`delete`/`recover` call and returns a preset outcome, so the
/// lifecycle routes can be exercised (auth gate, request decoding, error →
/// status mapping) without a real homeserver.
pub struct StubLifecycle {
    login_outcome: LoginOutcome,
    logout_outcome: LogoutOutcome,
    delete_outcome: DeleteOutcome,
    recover_outcome: RecoverOutcome,
    login_calls: Mutex<Vec<LoginCall>>,
    logout_calls: Mutex<Vec<Uuid>>,
    delete_calls: Mutex<Vec<Uuid>>,
    recover_calls: Mutex<Vec<(Uuid, String)>>,
}

impl StubLifecycle {
    /// A stub that succeeds for every login, logout, and delete.
    pub fn ok(account_id: Uuid) -> Self {
        Self {
            login_outcome: LoginOutcome::Ok(account_id),
            logout_outcome: LogoutOutcome::Ok,
            delete_outcome: DeleteOutcome::Ok,
            login_calls: Mutex::new(Vec::new()),
            logout_calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
            recover_outcome: RecoverOutcome::Ok,
            recover_calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub whose login returns the given failure (logout/delete succeed).
    pub fn failing(outcome: LoginOutcome) -> Self {
        Self {
            login_outcome: outcome,
            logout_outcome: LogoutOutcome::Ok,
            delete_outcome: DeleteOutcome::Ok,
            login_calls: Mutex::new(Vec::new()),
            logout_calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
            recover_outcome: RecoverOutcome::Ok,
            recover_calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub whose logout returns the given failure (login returns a nil id).
    pub fn logout_failing(outcome: LogoutOutcome) -> Self {
        Self {
            login_outcome: LoginOutcome::Ok(Uuid::nil()),
            logout_outcome: outcome,
            delete_outcome: DeleteOutcome::Ok,
            login_calls: Mutex::new(Vec::new()),
            logout_calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
            recover_outcome: RecoverOutcome::Ok,
            recover_calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub whose delete returns the given failure (login returns a nil id).
    pub fn delete_failing(outcome: DeleteOutcome) -> Self {
        Self {
            login_outcome: LoginOutcome::Ok(Uuid::nil()),
            logout_outcome: LogoutOutcome::Ok,
            delete_outcome: outcome,
            login_calls: Mutex::new(Vec::new()),
            logout_calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
            recover_outcome: RecoverOutcome::Ok,
            recover_calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub whose recover returns the given failure (login returns a nil id).
    pub fn recover_failing(outcome: RecoverOutcome) -> Self {
        Self {
            login_outcome: LoginOutcome::Ok(Uuid::nil()),
            logout_outcome: LogoutOutcome::Ok,
            delete_outcome: DeleteOutcome::Ok,
            recover_outcome: outcome,
            login_calls: Mutex::new(Vec::new()),
            logout_calls: Mutex::new(Vec::new()),
            delete_calls: Mutex::new(Vec::new()),
            recover_calls: Mutex::new(Vec::new()),
        }
    }

    /// The login calls recorded so far, in order.
    pub fn calls(&self) -> Vec<LoginCall> {
        self.login_calls.lock().unwrap().clone()
    }

    /// The account ids passed to logout, in order.
    pub fn logout_calls(&self) -> Vec<Uuid> {
        self.logout_calls.lock().unwrap().clone()
    }

    /// The account ids passed to delete, in order.
    pub fn delete_calls(&self) -> Vec<Uuid> {
        self.delete_calls.lock().unwrap().clone()
    }

    /// The `(account_id, recovery_key)` pairs passed to recover, in order.
    pub fn recover_calls(&self) -> Vec<(Uuid, String)> {
        self.recover_calls.lock().unwrap().clone()
    }
}

/// One recorded call to the [`StubVerification`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyCall {
    Start { account_id: Uuid, device_id: String },
    List { account_id: Uuid },
    Get { account_id: Uuid, flow_id: String },
    Confirm { account_id: Uuid, flow_id: String },
    Cancel { account_id: Uuid, flow_id: String },
}

/// The error the [`StubVerification`] returns for every call (or `Ok`). `Clone`
/// so one stub can answer repeated calls; mirrors [`VerifyError`]'s variants.
#[derive(Clone)]
pub enum VerifyOutcome {
    Ok,
    NotFound(String),
    NotActive(String),
    Conflict(String),
    BadRequest(String),
    Upstream(String),
    Internal,
}

impl VerifyOutcome {
    /// The error to return, or `None` when the outcome is `Ok` (the method then
    /// returns its natural success value).
    fn as_error(&self) -> Option<VerifyError> {
        match self {
            VerifyOutcome::Ok => None,
            VerifyOutcome::NotFound(m) => Some(VerifyError::NotFound(m.clone())),
            VerifyOutcome::NotActive(m) => Some(VerifyError::NotActive(m.clone())),
            VerifyOutcome::Conflict(m) => Some(VerifyError::Conflict(m.clone())),
            VerifyOutcome::BadRequest(m) => Some(VerifyError::BadRequest(m.clone())),
            VerifyOutcome::Upstream(m) => Some(VerifyError::Upstream(m.clone())),
            VerifyOutcome::Internal => Some(VerifyError::Internal),
        }
    }
}

/// An in-memory [`VerificationService`] for tests: records each call and returns
/// a preset outcome, so the verify routes can be exercised (auth gate, path
/// /body decoding, error → status mapping, DTO shape) without a real client.
pub struct StubVerification {
    outcome: VerifyOutcome,
    flow_id: String,
    summary: FlowSummary,
    calls: Mutex<Vec<VerifyCall>>,
}

impl StubVerification {
    /// A stub that succeeds for every op: `start` returns `flow_id`, and
    /// `get`/`list` return a representative SAS-stage flow (emoji + decimals
    /// populated) so a test can assert the [`FlowDto`](axon_api) wire mapping.
    pub fn ok(flow_id: &str) -> Self {
        Self {
            outcome: VerifyOutcome::Ok,
            flow_id: flow_id.to_owned(),
            summary: FlowSummary {
                flow_id: flow_id.to_owned(),
                target_device_id: "TRUSTEDDEV".to_owned(),
                stage: FlowStage::KeysExchanged,
                emoji: Some(vec![
                    ("🐶".to_owned(), "Dog".to_owned()),
                    ("🐱".to_owned(), "Cat".to_owned()),
                ]),
                decimals: Some((1234, 5678, 9012)),
                cancel_reason: None,
            },
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub that returns the given failure for every op.
    pub fn failing(outcome: VerifyOutcome) -> Self {
        let mut s = Self::ok("$unused-flow");
        s.outcome = outcome;
        s
    }

    /// The calls recorded so far, in order.
    pub fn calls(&self) -> Vec<VerifyCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl VerificationService for StubVerification {
    async fn start(&self, account_id: Uuid, device_id: &str) -> Result<String, VerifyError> {
        self.calls.lock().unwrap().push(VerifyCall::Start {
            account_id,
            device_id: device_id.to_owned(),
        });
        match self.outcome.as_error() {
            Some(err) => Err(err),
            None => Ok(self.flow_id.clone()),
        }
    }

    async fn list(&self, account_id: Uuid) -> Result<Vec<FlowSummary>, VerifyError> {
        self.calls
            .lock()
            .unwrap()
            .push(VerifyCall::List { account_id });
        match self.outcome.as_error() {
            Some(err) => Err(err),
            None => Ok(vec![self.summary.clone()]),
        }
    }

    async fn get(&self, account_id: Uuid, flow_id: &str) -> Result<FlowSummary, VerifyError> {
        self.calls.lock().unwrap().push(VerifyCall::Get {
            account_id,
            flow_id: flow_id.to_owned(),
        });
        match self.outcome.as_error() {
            Some(err) => Err(err),
            None => Ok(self.summary.clone()),
        }
    }

    async fn confirm(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError> {
        self.calls.lock().unwrap().push(VerifyCall::Confirm {
            account_id,
            flow_id: flow_id.to_owned(),
        });
        match self.outcome.as_error() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    async fn cancel(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError> {
        self.calls.lock().unwrap().push(VerifyCall::Cancel {
            account_id,
            flow_id: flow_id.to_owned(),
        });
        match self.outcome.as_error() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl AccountLifecycle for StubLifecycle {
    async fn login(
        &self,
        homeserver_url: Option<&str>,
        username: &str,
        password: &str,
    ) -> Result<Uuid, LoginError> {
        self.login_calls.lock().unwrap().push(LoginCall {
            homeserver_url: homeserver_url.map(str::to_owned),
            username: username.to_owned(),
            password: password.to_owned(),
        });
        self.login_outcome.to_result()
    }

    async fn logout(&self, account_id: Uuid) -> Result<(), LogoutError> {
        self.logout_calls.lock().unwrap().push(account_id);
        self.logout_outcome.to_result()
    }

    async fn delete(&self, account_id: Uuid) -> Result<(), DeleteError> {
        self.delete_calls.lock().unwrap().push(account_id);
        self.delete_outcome.to_result()
    }

    async fn recover(&self, account_id: Uuid, recovery_key: &str) -> Result<(), RecoverError> {
        self.recover_calls
            .lock()
            .unwrap()
            .push((account_id, recovery_key.to_owned()));
        self.recover_outcome.to_result()
    }
}

/// A no-op [`MediaProxy`] for tests that don't exercise the media route.
pub struct StubMediaProxy;

#[async_trait]
impl MediaProxy for StubMediaProxy {
    async fn get_media(
        &self,
        _account_id: Uuid,
        _mxc_url: &str,
        _encrypted_file: Option<serde_json::Value>,
    ) -> Result<MediaContent, MediaError> {
        Err(MediaError::NotFound("stub: no media".to_owned()))
    }
}

/// One call recorded by [`ConfiguredMediaProxy`].
#[derive(Clone, Debug, PartialEq)]
pub struct MediaCall {
    pub account_id: Uuid,
    pub mxc_url: String,
    pub encrypted_file: Option<serde_json::Value>,
}

/// The outcome [`ConfiguredMediaProxy`] returns for every call.
#[derive(Clone)]
pub enum MediaOutcome {
    Ok(Vec<u8>),
    Forbidden(String),
    NotConnected(String),
}

/// A configurable media proxy for route-level status and call-path tests.
pub struct ConfiguredMediaProxy {
    outcome: MediaOutcome,
    calls: Mutex<Vec<MediaCall>>,
}

impl ConfiguredMediaProxy {
    pub fn ok(data: &[u8]) -> Self {
        Self {
            outcome: MediaOutcome::Ok(data.to_vec()),
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn failing(outcome: MediaOutcome) -> Self {
        Self {
            outcome,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<MediaCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl MediaProxy for ConfiguredMediaProxy {
    async fn get_media(
        &self,
        account_id: Uuid,
        mxc_url: &str,
        encrypted_file: Option<serde_json::Value>,
    ) -> Result<MediaContent, MediaError> {
        self.calls.lock().unwrap().push(MediaCall {
            account_id,
            mxc_url: mxc_url.to_owned(),
            encrypted_file,
        });
        match &self.outcome {
            MediaOutcome::Ok(data) => Ok(MediaContent {
                data: data.clone(),
                content_type: "application/octet-stream".to_owned(),
            }),
            MediaOutcome::Forbidden(message) => Err(MediaError::Forbidden(message.clone())),
            MediaOutcome::NotConnected(message) => Err(MediaError::NotConnected(message.clone())),
        }
    }
}

#[async_trait]
impl MessageSender for StubSender {
    async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
        relation: Relation<'_>,
    ) -> Result<String, SendError> {
        self.calls.lock().unwrap().push(Call::Send {
            account_id,
            room_id: room_id.to_owned(),
            body: body.to_owned(),
            formatted: formatted.map(|f| (f.format.to_owned(), f.body.to_owned())),
            reply_to: relation.reply_to.map(str::to_owned),
            thread_root: relation.thread_root.map(str::to_owned),
        });
        self.outcome.to_result()
    }

    async fn edit(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
        formatted: Option<Formatted<'_>>,
    ) -> Result<String, SendError> {
        self.calls.lock().unwrap().push(Call::Edit {
            account_id,
            room_id: room_id.to_owned(),
            event_id: event_id.to_owned(),
            body: body.to_owned(),
            formatted: formatted.map(|f| (f.format.to_owned(), f.body.to_owned())),
        });
        self.outcome.to_result()
    }

    async fn redact(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String, SendError> {
        self.calls.lock().unwrap().push(Call::Redact {
            account_id,
            room_id: room_id.to_owned(),
            event_id: event_id.to_owned(),
            reason: reason.map(str::to_owned),
        });
        self.outcome.to_result()
    }

    async fn react(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<String, SendError> {
        self.calls.lock().unwrap().push(Call::React {
            account_id,
            room_id: room_id.to_owned(),
            event_id: event_id.to_owned(),
            key: key.to_owned(),
        });
        self.outcome.to_result()
    }
}

/// An in-memory [`SenderTrustService`] for tests: returns a preset bundle (or
/// error), so the verification-bundle route can be exercised (auth gate, path
/// decoding, error → status mapping, DTO shape) without a real client.
pub struct StubTrust {
    bundle: Option<TrustBundle>,
    error: Option<fn() -> TrustError>,
}

impl StubTrust {
    /// A stub returning a representative bundle: a `verified` at-decrypt snapshot
    /// paired with a currently-cross-signed, verified sender.
    pub fn ok() -> Self {
        Self {
            bundle: Some(TrustBundle {
                sender: "@bob:localhost".to_owned(),
                snapshot: Some(TrustSnapshot {
                    sender_trust: Some("verified".to_owned()),
                    verification_state: Some("verified".to_owned()),
                    device_id: Some("BOBDEVICE".to_owned()),
                    curve25519_key: Some("CURVE".to_owned()),
                    ed25519_key: Some("ED".to_owned()),
                    session_id: Some("session-1".to_owned()),
                    forwarded: Some(false),
                    forwarder_user_id: None,
                    forwarder_device_id: None,
                }),
                current: CurrentTrust {
                    device_known: true,
                    device_cross_signed: Some(true),
                    identity_known: true,
                    identity_verified: Some(true),
                    verification_violation: Some(false),
                    previously_verified: Some(true),
                    master_key: Some("MASTER".to_owned()),
                },
            }),
            error: None,
        }
    }

    /// A stub returning the given error for every call.
    pub fn failing(error: fn() -> TrustError) -> Self {
        Self {
            bundle: None,
            error: Some(error),
        }
    }
}

#[async_trait]
impl SenderTrustService for StubTrust {
    async fn bundle(&self, _account_id: Uuid, _event_id: &str) -> Result<TrustBundle, TrustError> {
        if let Some(make) = self.error {
            return Err(make());
        }
        Ok(self.bundle.clone().expect("ok stub has a bundle"))
    }
}
