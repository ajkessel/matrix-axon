//! Shared types, errors, and configuration for Axon.
//!
//! `axon-core` is the lowest crate in the workspace: it depends on no other
//! `axon-*` crate, so every other crate may depend on it. It owns the typed
//! [`Config`] loader and the top-level [`Error`] enum that downstream crate
//! errors convert into.

pub mod config;
pub mod error;

pub use config::{AccountProvision, Config, Credential, SyncConfig};
pub use error::{ConfigError, Error, Result};
