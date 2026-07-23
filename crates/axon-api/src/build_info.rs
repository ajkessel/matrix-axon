//! Build-info the binary knows about itself (M10 `/v1/status` extension).
//!
//! Plain compile-time data, not a port: unlike [`crate::backfill`]'s disk-space
//! health, there's nothing to poll at request time, so the binary just injects
//! the strings once via [`crate::state::AppState::with_build_info`] instead of
//! wiring up a trait object.

/// Identifies the exact build serving this instance, mirroring the fields the
/// binary logs in its startup "axon starting" line.
#[derive(Debug, Clone, Default)]
pub struct BuildInfo {
    pub version: String,
    pub git_hash: String,
    pub profile: String,
    pub build_time: String,
    pub rustc_version: String,
}
