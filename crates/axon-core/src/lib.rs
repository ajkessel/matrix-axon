//! Shared types, errors, and configuration for Axon.
//!
//! `axon-core` is the lowest crate in the workspace: it depends on no other
//! `axon-*` crate, so every other crate may depend on it. It owns the typed
//! [`Config`] loader and the top-level [`Error`] enum that downstream crate
//! errors convert into.

pub mod config;
pub mod error;
pub mod live;
pub mod message;

pub use config::{AccountProvision, Config, Credential, SearchConfig, SyncConfig};
pub use error::{ConfigError, Error, Result};
pub use live::{LiveEvent, LiveFrame, SenderTrustFrame, VerificationFrame, VerificationFrameKind};
pub use message::{Formatted, Relation};
