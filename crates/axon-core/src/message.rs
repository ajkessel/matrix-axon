//! Wire-neutral content types for outbound message mutations.
//!
//! [`Formatted`] is the rich-text companion to a message's plain `body`. Like the
//! read-side [`LiveEvent`](crate::LiveEvent), it lives in this lowest crate so the
//! two sibling crates that pass it — `axon-api` (the `MessageSender` port) and
//! `axon-sync` (the SDK gateway) — share one type without depending on each other.
//! The composition root in `axon-server` therefore forwards it verbatim, with no
//! mapping.

/// Rich-text rendering of a message body, supplied alongside the plain `body` on
/// send/edit. `format` names the markup (Matrix defines only
/// `org.matrix.custom.html`) and `body` is the rendered source (HTML).
///
/// Borrowed because it lives only for the duration of a single send/edit call.
/// Validated at the API boundary (both fields present together, recognized
/// `format`); the gateway carries it verbatim onto the Matrix event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Formatted<'a> {
    /// The markup name — `org.matrix.custom.html`.
    pub format: &'a str,
    /// The rendered body (HTML).
    pub body: &'a str,
}
