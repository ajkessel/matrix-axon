//! Extractor wrappers that surface rejections through the API error envelope.
//!
//! axum's stock [`Path`](axum::extract::Path) / [`Query`](axum::extract::Query)
//! reject malformed input (e.g. a path segment or query value that isn't a valid
//! UUID) with a plain-text `400` that bypasses our `{ "error": … }` envelope.
//! These newtype wrappers delegate to the stock extractors but map any rejection
//! into [`ApiError::bad_request`], so *every* `/v1/` error — handler-raised or
//! extractor-raised — has the same JSON shape.

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::response::ApiError;

/// Drop-in replacement for [`axum::extract::Path`] whose rejection is an
/// [`ApiError`] (`400`) rather than axum's default plain-text body.
pub struct Path<T>(pub T);

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Path(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}

/// Drop-in replacement for [`axum::extract::Query`] whose rejection is an
/// [`ApiError`] (`400`) rather than axum's default plain-text body.
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Query(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}

/// Drop-in replacement for [`axum::Json`] whose rejection is an [`ApiError`]
/// (`400`) — a malformed or missing request body returns the same
/// `{ "error": … }` envelope as every other failure rather than axum's default
/// plain-text body. Body-consuming, so it must be the last extractor in a handler.
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Json(value)),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }
}
