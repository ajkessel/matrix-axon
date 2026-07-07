//! `/v1/oauth/*` handlers (M14b, ADR 0054): axon as its own OAuth 2.0
//! Authorization Server, and an OIDC Relying Party to upstream providers.
//!
//! Deliberately **un-gated** — not behind `require_bearer` — since this is
//! how a client obtains a bearer token in the first place. Every handler
//! checks `oauth.enabled` (and, where relevant, the named provider's
//! `enabled`) first and returns `404` when off, exactly like
//! [`routes::search`](crate::routes::search) does for `search.enabled` — but
//! `404`, not `503`: an unauthenticated caller of a *disabled* surface should
//! see "this route doesn't exist" the same as a genuinely unregistered path,
//! not "come back later" (`/v1/ws` is the existing router's proof that a
//! specific route already beats the authed sub-router's catch-all regardless
//! of merge order; this router adds a third, equally-specific sibling).
//!
//! `POST /v1/oauth/token`'s body is plain RFC 6749 JSON — `{"access_token",
//! "token_type", "expires_in", "refresh_token"}` on success,
//! `{"error", "error_description"}` on failure — not this crate's
//! `ApiResponse`/`ApiError` envelope; see [`TokenSuccessBody`]/[`OAuthErrorBody`].

use std::sync::Arc;

use axon_store::{NewAuthorizationRequest, Store};
use axum::extract::{FromRequest, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Json;
use chrono::{Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use crate::extract::{Path, Query};
use crate::oauth::provider::OidcError;
use crate::oauth::tokens::{self, TokenError, TokenPair};
use crate::oauth::OAuthRuntime;
use crate::response::ApiError;

/// How long a Path A flow (and its axon-minted code) stays redeemable.
const AUTHORIZATION_REQUEST_TTL: ChronoDuration = ChronoDuration::minutes(10);

/// `GET /v1/oauth/authorize` query parameters (RFC 6749 §4.1.1, PKCE's
/// `code_challenge`/`code_challenge_method` per RFC 7636, plus `provider` to
/// pick which upstream IdP this flow uses).
#[derive(Debug, Deserialize)]
pub struct AuthorizeQuery {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub provider: String,
    /// The client's own CSRF-binding state, if it sent one. Opaque to axon;
    /// echoed back verbatim on the final redirect.
    pub state: Option<String>,
}

/// `GET /v1/oauth/{provider}/callback` query parameters — what the upstream
/// provider's own authorization server appends to its redirect back to axon.
#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
}

/// Start Path A: validate the request, record it, and redirect the browser
/// to the named upstream provider. An invalid `client_id`/`redirect_uri`
/// pair is rejected directly (`400`), never redirected — redirecting to an
/// unregistered URI is exactly what the exact-match allow-list exists to
/// prevent.
pub async fn authorize(
    State(store): State<Store>,
    State(runtime): State<Option<Arc<OAuthRuntime>>>,
    Query(q): Query<AuthorizeQuery>,
) -> Result<Response, ApiError> {
    let Some(runtime) = runtime else {
        return Err(ApiError::not_found("oauth is disabled"));
    };

    if q.response_type != "code" {
        return Err(ApiError::bad_request("response_type must be \"code\""));
    }
    if q.code_challenge_method != "S256" {
        return Err(ApiError::bad_request(
            "code_challenge_method must be \"S256\"",
        ));
    }
    if !runtime.redirect_uri_allowed(&q.client_id, &q.redirect_uri) {
        return Err(ApiError::bad_request("unknown client_id or redirect_uri"));
    }
    let Some(provider) = runtime.provider(&q.provider) else {
        return Err(ApiError::bad_request("unknown or disabled provider"));
    };

    let upstream_state = tokens::generate_opaque_value();
    let upstream_nonce = tokens::generate_opaque_value();
    let expires_at = Utc::now() + AUTHORIZATION_REQUEST_TTL;
    let callback_uri = callback_url(&runtime, &q.provider);

    store
        .create_authorization_request(&NewAuthorizationRequest {
            client_id: &q.client_id,
            redirect_uri: &q.redirect_uri,
            code_challenge: &q.code_challenge,
            code_challenge_method: &q.code_challenge_method,
            client_state: q.state.as_deref(),
            provider: &q.provider,
            upstream_state: &upstream_state,
            upstream_nonce: &upstream_nonce,
            expires_at,
        })
        .await?;

    let redirect_url = provider.authorize_url(&upstream_state, &upstream_nonce, &callback_uri);
    Ok(Redirect::to(&redirect_url).into_response())
}

/// Finish Path A's upstream leg: exchange the provider's code, verify the
/// id_token, confirm the asserted identity is already bound, mint axon's own
/// authorization code, and redirect the browser back to the client's
/// `redirect_uri`. Path A only ever authenticates an **already-bound**
/// owner — binding itself is the separate `axon oauth bind` flow (M14c).
pub async fn callback(
    State(store): State<Store>,
    State(runtime): State<Option<Arc<OAuthRuntime>>>,
    Path(provider_name): Path<String>,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, ApiError> {
    let Some(runtime) = runtime else {
        return Err(ApiError::not_found("oauth is disabled"));
    };
    let Some(provider) = runtime.provider(&provider_name) else {
        return Err(ApiError::not_found("unknown or disabled provider"));
    };

    let request = store
        .find_authorization_request_by_upstream_state(&provider_name, &q.state)
        .await?
        .ok_or_else(|| ApiError::bad_request("unknown or expired authorization flow"))?;

    let callback_uri = callback_url(&runtime, &provider_name);
    let upstream = provider
        .exchange_code(&q.code, &callback_uri)
        .await
        .map_err(oidc_error_to_api_error)?;
    let verified = provider
        .verify_identity_token(&upstream.id_token, Some(&request.upstream_nonce))
        .await
        .map_err(oidc_error_to_api_error)?;

    let identity = store
        .find_identity(&provider_name, &verified.subject)
        .await?
        .ok_or_else(|| ApiError::forbidden("this identity is not bound to this axon instance"))?;

    let axon_code = tokens::generate_opaque_value();
    if !store
        .complete_authorization(request.id, identity.id, &tokens::hash_secret(&axon_code))
        .await?
    {
        return Err(ApiError::conflict(
            "authorization flow already completed or expired",
        ));
    }

    let mut redirect_url =
        url::Url::parse(&request.redirect_uri).map_err(|_| ApiError::internal())?;
    redirect_url
        .query_pairs_mut()
        .append_pair("code", &axon_code);
    if let Some(client_state) = &request.client_state {
        redirect_url
            .query_pairs_mut()
            .append_pair("state", client_state);
    }
    Ok(Redirect::to(redirect_url.as_str()).into_response())
}

/// `POST /v1/oauth/token` (RFC 6749 §4.1.3, §6, and Path B's custom
/// `urn:axon:identity_token` grant): redeem an authorization code, a refresh
/// token, or a native identity token for an axon access/refresh pair.
pub async fn token(
    State(store): State<Store>,
    State(runtime): State<Option<Arc<OAuthRuntime>>>,
    OAuthForm(body): OAuthForm<TokenRequest>,
) -> Response {
    let Some(runtime) = runtime else {
        return ApiError::not_found("oauth is disabled").into_response();
    };

    let result = match body.grant_type.as_str() {
        "authorization_code" => {
            match (
                body.code.as_deref(),
                body.code_verifier.as_deref(),
                body.client_id.as_deref(),
                body.redirect_uri.as_deref(),
            ) {
                (Some(code), Some(verifier), Some(client_id), Some(redirect_uri)) => {
                    tokens::redeem_authorization_code(
                        &store, &runtime, code, verifier, client_id, redirect_uri,
                    )
                    .await
                }
                _ => Err(TokenError::InvalidGrant(
                    "authorization_code grant requires code, code_verifier, client_id, redirect_uri",
                )),
            }
        }
        "refresh_token" => match body.refresh_token.as_deref() {
            Some(refresh_token) => {
                tokens::redeem_refresh_token(&store, &runtime, refresh_token).await
            }
            None => Err(TokenError::InvalidGrant(
                "refresh_token grant requires refresh_token",
            )),
        },
        "urn:axon:identity_token" => {
            match (
                body.provider.as_deref(),
                body.identity_token.as_deref(),
                body.client_id.as_deref(),
            ) {
                (Some(provider), Some(identity_token), Some(client_id)) => {
                    tokens::redeem_identity_token(
                        &store,
                        &runtime,
                        provider,
                        identity_token,
                        client_id,
                    )
                    .await
                }
                _ => Err(TokenError::InvalidGrant(
                    "urn:axon:identity_token grant requires provider, identity_token, client_id",
                )),
            }
        }
        other => {
            return oauth_error_response(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                format!("unsupported grant_type {other:?}"),
            )
        }
    };

    match result {
        Ok(pair) => token_success_response(pair),
        Err(err) => token_error_into_response(err),
    }
}

/// `GET /v1/oauth/bind` — the CLI device-code handshake's browser leg. Not
/// yet implemented: the `axon oauth bind` CLI verb that drives this ships in
/// M14c. Always `404` in M14b, matching every other disabled-surface route.
pub async fn bind() -> ApiError {
    ApiError::not_found("oauth bind is not yet implemented")
}

fn callback_url(runtime: &OAuthRuntime, provider_name: &str) -> String {
    format!(
        "{}/v1/oauth/{provider_name}/callback",
        runtime.external_base_url
    )
}

fn oidc_error_to_api_error(err: OidcError) -> ApiError {
    tracing::warn!(error = %err, "oidc verification failed");
    ApiError::bad_gateway("upstream identity verification failed")
}

/// `POST /v1/oauth/token`'s form body. Fields are `Option` because which are
/// required depends on `grant_type` — validated per-grant in [`token`], not
/// via serde, so a request naming an unrecognized grant type still gets a
/// clean `unsupported_grant_type` rather than a generic deserialization
/// error about unrelated missing fields.
#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub identity_token: Option<String>,
}

/// `application/x-www-form-urlencoded` extractor (RFC 6749 §4.1.3 mandates
/// this content type for the token endpoint, not JSON) whose rejection is an
/// RFC 6749 `invalid_request` body rather than axum's default plain text.
pub struct OAuthForm<T>(pub T);

impl<T, S> FromRequest<S> for OAuthForm<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Form::<T>::from_request(req, state).await {
            Ok(axum::extract::Form(value)) => Ok(OAuthForm(value)),
            Err(rejection) => Err(oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                rejection.body_text(),
            )),
        }
    }
}

/// `POST /v1/oauth/token`'s success body (RFC 6749 §5.1) — plain, not
/// wrapped in `{"data": ...}`.
#[derive(Debug, Serialize)]
struct TokenSuccessBody {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    refresh_token: String,
}

/// `POST /v1/oauth/token`'s error body (RFC 6749 §5.2).
#[derive(Debug, Serialize)]
struct OAuthErrorBody {
    error: &'static str,
    error_description: String,
}

fn token_success_response(pair: TokenPair) -> Response {
    Json(TokenSuccessBody {
        access_token: pair.access_token,
        token_type: "Bearer",
        expires_in: pair.expires_in,
        refresh_token: pair.refresh_token,
    })
    .into_response()
}

fn oauth_error_response(
    status: StatusCode,
    error: &'static str,
    description: impl Into<String>,
) -> Response {
    (
        status,
        Json(OAuthErrorBody {
            error,
            error_description: description.into(),
        }),
    )
        .into_response()
}

fn token_error_into_response(err: TokenError) -> Response {
    match err {
        TokenError::InvalidGrant(msg) => {
            oauth_error_response(StatusCode::BAD_REQUEST, "invalid_grant", msg)
        }
        TokenError::UnknownProvider => oauth_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "unknown or disabled provider",
        ),
        TokenError::NotBound => oauth_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "identity is not bound to this axon instance",
        ),
        TokenError::Oidc(err) => {
            tracing::warn!(error = %err, "oidc verification failed redeeming a token");
            oauth_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "identity token verification failed",
            )
        }
        TokenError::Store(err) => {
            tracing::error!(error = %err, "store error serving oauth token request");
            oauth_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                "internal server error",
            )
        }
    }
}
