//! Shared test doubles for the API integration tests.
//!
//! [`StubSender`] is an in-memory [`MessageSender`] that records the calls it
//! receives and returns a preset outcome, so the mutation handlers can be
//! exercised (routing, request decoding, error mapping) without a real
//! homeserver or sync engine.

#![allow(dead_code)] // each tests/*.rs is its own crate; not all use every helper.

use std::sync::Mutex;

use async_trait::async_trait;
use axon_api::{
    AccountLifecycle, DeleteError, LoginError, LogoutError, MessageSender, RecoverError, SendError,
};
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
/// lifecycle routes can be exercised (loopback guard, request decoding, error →
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
