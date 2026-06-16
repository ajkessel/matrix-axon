//! The device-verification port the verify handlers depend on.
//!
//! Like [`AccountLifecycle`](crate::lifecycle::AccountLifecycle), `axon-api`
//! defines the capability it *needs* — driving an interactive SAS verification
//! flow — rather than depending on whatever provides it. The real implementation
//! lives in `axon-sync`, adapted onto this port by `axon-server`, so this crate
//! stays free of `axon-sync` and `matrix-sdk`.
//!
//! SAS is an asynchronous, multi-step protocol, so the surface is a small state
//! machine: [`start`](VerificationService::start) kicks a flow off and returns
//! its `flow_id`; [`confirm`](VerificationService::confirm) /
//! [`cancel`](VerificationService::cancel) advance or end it; and
//! [`get`](VerificationService::get) / [`list`](VerificationService::list) read
//! replayable per-flow state, which is how a client that dropped its `/v1/ws`
//! socket resumes without assuming the frames it missed. Live progress is also
//! pushed as `verification.*` frames over `/v1/ws` (see
//! [`VerificationFrame`](axon_core::VerificationFrame)).

use async_trait::async_trait;
use uuid::Uuid;

/// What can go wrong driving a verification flow. Deliberately small and
/// HTTP-shaped, like [`LoginError`](crate::lifecycle::LoginError): the adapter
/// that implements [`VerificationService`] collapses its richer backend error
/// into one of these so the handler layer maps a stable set of statuses (see the
/// `From` impl in [`response`](crate::response)).
#[derive(Debug)]
pub enum VerifyError {
    /// No such account, or no such (live or recently-terminal) flow. → `404`.
    NotFound(String),
    /// The account is logged out (`deactivated`) or mid-teardown (`deleting`), so
    /// it has no live client to verify against. → `409`.
    NotActive(String),
    /// The flow is in a stage that doesn't permit the operation (e.g. confirming
    /// before the SAS is ready, or operating on an already-terminal flow). → `409`.
    Conflict(String),
    /// The request named a device that isn't a known, verifiable device of this
    /// account. → `400`.
    BadRequest(String),
    /// The upstream homeserver rejected or failed a verification to-device send.
    /// → `502`.
    Upstream(String),
    /// An internal failure (e.g. the store, or the live client could not be
    /// reached). The detail is logged, not returned. → `500`.
    Internal,
}

/// Which stage of the SAS exchange a flow is in. Ordered request → … → terminal;
/// `Done`/`Cancelled` are the two terminal stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowStage {
    /// A verification was requested; awaiting the peer.
    Requested,
    /// The peer is ready; SAS not yet computed.
    Ready,
    /// Keys have been exchanged — the SAS emoji/decimals are available to compare.
    KeysExchanged,
    /// This side has confirmed the SAS; awaiting the peer's MAC.
    Confirmed,
    /// Completed successfully; the device is now cross-signed.
    Done,
    /// Cancelled (timeout, mismatch, or either side cancelling).
    Cancelled,
}

/// A replayable snapshot of one verification flow — what
/// [`get`](VerificationService::get) returns and the `verification.*` frames
/// reference. Re-derived from the live SDK object (or a recently-terminal flow's
/// retained outcome), never from a stale cache, so a reconnecting client always
/// reads the true current state.
#[derive(Debug, Clone)]
pub struct FlowSummary {
    /// The verification transaction id.
    pub flow_id: String,
    /// The other device in the flow (the user's trusted device).
    pub target_device_id: String,
    /// The flow's current stage.
    pub stage: FlowStage,
    /// SAS emoji as `(symbol, description)` pairs — present once keys are
    /// exchanged.
    pub emoji: Option<Vec<(String, String)>>,
    /// SAS decimal triple — present alongside `emoji`.
    pub decimals: Option<(u16, u16, u16)>,
    /// The cancel reason, for a `Cancelled` flow.
    pub cancel_reason: Option<String>,
}

/// Drives an interactive SAS device-verification flow at runtime. Implemented
/// outside this crate; held in [`AppState`](crate::AppState) as
/// `Arc<dyn VerificationService>`.
#[async_trait]
pub trait VerificationService: Send + Sync {
    /// Start a SAS verification of the **active** account `account_id` against its
    /// own trusted device `device_id`, returning the new flow's `flow_id`. A
    /// logged-out/mid-teardown account is [`NotActive`](VerifyError::NotActive); a
    /// `device_id` that isn't a known device of the account is
    /// [`BadRequest`](VerifyError::BadRequest).
    async fn start(&self, account_id: Uuid, device_id: &str) -> Result<String, VerifyError>;

    /// List the account's currently-tracked flows (live, plus recently-terminal
    /// ones still within their grace window). An unknown account is
    /// [`NotFound`](VerifyError::NotFound).
    async fn list(&self, account_id: Uuid) -> Result<Vec<FlowSummary>, VerifyError>;

    /// Read one flow's replayable state. An unknown account or an unknown/expired
    /// flow is [`NotFound`](VerifyError::NotFound).
    async fn get(&self, account_id: Uuid, flow_id: &str) -> Result<FlowSummary, VerifyError>;

    /// Confirm that the SAS matches, sending this side's MAC. Requires the flow to
    /// have reached the SAS stage ([`Conflict`](VerifyError::Conflict) otherwise).
    /// Idempotent — confirming an already-confirmed flow succeeds.
    async fn confirm(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError>;

    /// Cancel the flow. Idempotent — cancelling an already-terminal flow succeeds.
    async fn cancel(&self, account_id: Uuid, flow_id: &str) -> Result<(), VerifyError>;
}
