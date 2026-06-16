//! The shared JSON envelope every `/v1/` handler returns.
//!
//! Success bodies are `{ "data": <T> }` ([`ApiResponse`]); error bodies are
//! `{ "error": { "code", "message" } }` ([`ApiError`]). Handlers return
//! `Result<ApiResponse<T>, ApiError>` and the [`IntoResponse`] impls here pick
//! the status code and serialize the body, so every route is consistent.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

/// Success envelope: a 2xx body is always `{ "data": <T> }`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiResponse<T> {
    /// The route-specific payload.
    pub data: T,
}

impl<T> ApiResponse<T> {
    /// Wrap a payload in the success envelope.
    pub fn new(data: T) -> Self {
        Self { data }
    }
}

impl<T: Serialize> IntoResponse for ApiResponse<T> {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

/// The error body shape: a machine-readable `code` and a human-readable
/// `message`, nested under `error`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Stable, machine-readable code, e.g. `not_found`, `bad_request`.
    pub code: String,
    /// Human-readable explanation. Safe to surface to a developer; never carries
    /// internal error detail for `internal` errors.
    pub message: String,
}

/// The error envelope: `{ "error": { "code", "message" } }`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// The error detail.
    pub error: ErrorBody,
}

/// An API error: the wire body plus the HTTP status to send it with. Construct
/// via the helpers; a bare [`StoreError`](axon_store::StoreError) converts into a
/// logged `500`.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorResponse,
}

impl ApiError {
    // Only the statuses the current endpoints raise have named constructors
    // below. Others (401/403 with auth, 409, 422, 503, …) slot in as one-line
    // wrappers over this private `new` as the API surface grows — the wire shape
    // and `IntoResponse` stay unchanged.
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorResponse {
                error: ErrorBody {
                    code: code.to_owned(),
                    message: message.into(),
                },
            },
        }
    }

    /// `404 Not Found` — the addressed resource does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// `400 Bad Request` — a malformed parameter (e.g. an undecodable cursor).
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// `401 Unauthorized` — the supplied credentials were rejected.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    /// `403 Forbidden` — the operation isn't permitted (e.g. editing a message
    /// the account didn't author).
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// `409 Conflict` — the request collides with the resource's current state
    /// (e.g. logging in an account that is already active).
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// `502 Bad Gateway` — the upstream homeserver rejected or failed the request.
    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "bad_gateway", message)
    }

    /// `503 Service Unavailable` — the account isn't reachable right now (e.g. its
    /// homeserver is down); the caller should retry.
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
            message,
        )
    }

    /// `500 Internal Server Error` with a generic message; the real cause is
    /// logged, never returned.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal server error",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl From<axon_store::StoreError> for ApiError {
    fn from(err: axon_store::StoreError) -> Self {
        // The store error can carry SQL detail; log it, return a generic body.
        tracing::error!(error = %err, "store error serving request");
        ApiError::internal()
    }
}

impl From<crate::sender::SendError> for ApiError {
    fn from(err: crate::sender::SendError) -> Self {
        use crate::sender::SendError;
        match err {
            SendError::NotFound(msg) => ApiError::not_found(msg),
            SendError::Forbidden(msg) => ApiError::forbidden(msg),
            SendError::Unavailable(msg) => ApiError::service_unavailable(msg),
            SendError::Invalid(msg) => ApiError::bad_request(msg),
            SendError::Upstream(msg) => ApiError::bad_gateway(msg),
        }
    }
}

impl From<crate::lifecycle::LoginError> for ApiError {
    fn from(err: crate::lifecycle::LoginError) -> Self {
        use crate::lifecycle::LoginError;
        match err {
            LoginError::InvalidRequest(msg) => ApiError::bad_request(msg),
            LoginError::AuthFailed(msg) => ApiError::unauthorized(msg),
            LoginError::Conflict(msg) => ApiError::conflict(msg),
            LoginError::Upstream(msg) => ApiError::bad_gateway(msg),
            // The real cause is logged at the adapter/store boundary; return generic.
            LoginError::Internal => ApiError::internal(),
        }
    }
}

impl From<crate::lifecycle::LogoutError> for ApiError {
    fn from(err: crate::lifecycle::LogoutError) -> Self {
        use crate::lifecycle::LogoutError;
        match err {
            LogoutError::NotFound(msg) => ApiError::not_found(msg),
            LogoutError::Conflict(msg) => ApiError::conflict(msg),
            // The real cause is logged at the adapter/store boundary; return generic.
            LogoutError::Internal => ApiError::internal(),
        }
    }
}

impl From<crate::lifecycle::DeleteError> for ApiError {
    fn from(err: crate::lifecycle::DeleteError) -> Self {
        use crate::lifecycle::DeleteError;
        match err {
            DeleteError::NotFound(msg) => ApiError::not_found(msg),
            DeleteError::Conflict(msg) => ApiError::conflict(msg),
            // The real cause is logged at the adapter/store boundary; return generic.
            DeleteError::Internal => ApiError::internal(),
        }
    }
}

impl From<crate::lifecycle::RecoverError> for ApiError {
    fn from(err: crate::lifecycle::RecoverError) -> Self {
        use crate::lifecycle::RecoverError;
        match err {
            RecoverError::NotFound(msg) => ApiError::not_found(msg),
            RecoverError::Conflict(msg) => ApiError::conflict(msg),
            // A wrong/rotated key (or no Secure Backup) is a readable client error.
            RecoverError::BadRequest(msg) => ApiError::bad_request(msg),
            // The real cause is logged at the adapter/store boundary; return generic.
            RecoverError::Internal => ApiError::internal(),
        }
    }
}

impl From<crate::verification::VerifyError> for ApiError {
    fn from(err: crate::verification::VerifyError) -> Self {
        use crate::verification::VerifyError;
        match err {
            VerifyError::NotFound(msg) => ApiError::not_found(msg),
            // Both a logged-out account and a wrong-stage/terminal flow are conflicts
            // with the resource's current state.
            VerifyError::NotActive(msg) | VerifyError::Conflict(msg) => ApiError::conflict(msg),
            VerifyError::BadRequest(msg) => ApiError::bad_request(msg),
            VerifyError::Upstream(msg) => ApiError::bad_gateway(msg),
            // The real cause is logged at the adapter/store boundary; return generic.
            VerifyError::Internal => ApiError::internal(),
        }
    }
}
