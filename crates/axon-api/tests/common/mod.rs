//! Shared test doubles for the API integration tests.
//!
//! [`StubSender`] is an in-memory [`MessageSender`] that records the calls it
//! receives and returns a preset outcome, so the mutation handlers can be
//! exercised (routing, request decoding, error mapping) without a real
//! homeserver or sync engine.

#![allow(dead_code)] // each tests/*.rs is its own crate; not all use every helper.

use std::sync::Mutex;

use async_trait::async_trait;
use axon_api::{AccountLifecycle, LoginError, MessageSender, SendError};
use uuid::Uuid;

/// One recorded call to the stub, with the arguments the handler passed through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Send {
        account_id: Uuid,
        room_id: String,
        body: String,
    },
    Edit {
        account_id: Uuid,
        room_id: String,
        event_id: String,
        body: String,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginCall {
    pub homeserver_url: String,
    pub username: String,
    pub password: String,
}

/// An in-memory [`AccountLifecycle`] for tests: records each `login` call and
/// returns a preset outcome, so the login route can be exercised (loopback guard,
/// request decoding, `LoginError` → status mapping) without a real homeserver.
pub struct StubLifecycle {
    outcome: LoginOutcome,
    calls: Mutex<Vec<LoginCall>>,
}

impl StubLifecycle {
    /// A stub that returns `Ok(account_id)` for every login.
    pub fn ok(account_id: Uuid) -> Self {
        Self {
            outcome: LoginOutcome::Ok(account_id),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A stub that returns the given failure for every login.
    pub fn failing(outcome: LoginOutcome) -> Self {
        Self {
            outcome,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// The login calls recorded so far, in order.
    pub fn calls(&self) -> Vec<LoginCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AccountLifecycle for StubLifecycle {
    async fn login(
        &self,
        homeserver_url: &str,
        username: &str,
        password: &str,
    ) -> Result<Uuid, LoginError> {
        self.calls.lock().unwrap().push(LoginCall {
            homeserver_url: homeserver_url.to_owned(),
            username: username.to_owned(),
            password: password.to_owned(),
        });
        self.outcome.to_result()
    }
}

#[async_trait]
impl MessageSender for StubSender {
    async fn send_message(
        &self,
        account_id: Uuid,
        room_id: &str,
        body: &str,
    ) -> Result<String, SendError> {
        self.calls.lock().unwrap().push(Call::Send {
            account_id,
            room_id: room_id.to_owned(),
            body: body.to_owned(),
        });
        self.outcome.to_result()
    }

    async fn edit(
        &self,
        account_id: Uuid,
        room_id: &str,
        event_id: &str,
        body: &str,
    ) -> Result<String, SendError> {
        self.calls.lock().unwrap().push(Call::Edit {
            account_id,
            room_id: room_id.to_owned(),
            event_id: event_id.to_owned(),
            body: body.to_owned(),
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
