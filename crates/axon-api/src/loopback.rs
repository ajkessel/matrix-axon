//! A loopback-only guard for the destructive / secret-bearing lifecycle routes.
//!
//! Until the bearer-token auth layer lands, endpoints that take a password or
//! tear an account down must not be reachable from off-box. This middleware
//! rejects any request whose peer address isn't loopback (`127.0.0.0/8`, `::1`)
//! with a `403`, regardless of the configured bind address. It **fails closed**:
//! if the peer address is unavailable (the server wasn't built with
//! [`ConnectInfo`](axum::extract::ConnectInfo)), the request is refused.
//!
//! It is applied as a `route_layer` on individual routes, so ordinary read routes
//! are unaffected. The whole guard is lifted once auth applies uniformly.

use std::net::SocketAddr;

use axum::extract::ConnectInfo;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::response::ApiError;

/// Reject the request unless its peer is a loopback address. The peer address is
/// read from the [`ConnectInfo<SocketAddr>`] extension the server installs via
/// `into_make_service_with_connect_info`; its absence is treated as non-loopback.
pub async fn require_loopback(req: Request, next: Next) -> Response {
    let is_loopback = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback());

    if is_loopback {
        next.run(req).await
    } else {
        ApiError::forbidden("this endpoint is restricted to loopback clients").into_response()
    }
}
