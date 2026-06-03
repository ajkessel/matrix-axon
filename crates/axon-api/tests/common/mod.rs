//! Shared test doubles for the API integration tests.
//!
//! [`StubSender`] is an in-memory [`MessageSender`] that records the calls it
//! receives and returns a preset outcome, so the mutation handlers can be
//! exercised (routing, request decoding, error mapping) without a real
//! homeserver or sync engine.

#![allow(dead_code)] // each tests/*.rs is its own crate; not all use every helper.

use std::sync::Mutex;

use async_trait::async_trait;
use axon_api::{MessageSender, SendError};
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
    Unavailable(String),
    Invalid(String),
    Upstream(String),
}

impl Outcome {
    fn to_result(&self) -> Result<String, SendError> {
        match self {
            Outcome::Ok(id) => Ok(id.clone()),
            Outcome::NotFound(m) => Err(SendError::NotFound(m.clone())),
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
