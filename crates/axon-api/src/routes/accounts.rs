//! Account read endpoints: list the accounts this Axon manages, and read one.
//!
//! These are pure store reads (no secrets — the access token is never exposed)
//! and, unlike the destructive/secret-bearing lifecycle verbs (login / recover /
//! logout / delete, later subphases), are ordinary `/v1/` routes, not
//! loopback-restricted.

use std::sync::Arc;

use axon_store::Store;
use axum::extract::State;
use uuid::Uuid;

use crate::dto::{AccountDto, LoginRequest};
use crate::extract::{Json, Path};
use crate::lifecycle::AccountLifecycle;
use crate::response::{ApiError, ApiResponse};

/// List the accounts this Axon manages, oldest first — the **client-visible**
/// set: `active` and `deactivated`.
///
/// Logged-out (`deactivated`) accounts are included so a client that has lost the
/// `account_id` can still discover one to offer re-login (the login verb both
/// produces and reactivates them). The transient `deleting` teardown state is
/// excluded — a row mid-removal isn't something to act on — but any account, in
/// any state, can still be read by id via [`get_account`].
#[utoipa::path(
    get,
    path = "/v1/accounts",
    responses(
        (status = 200, description = "Client-visible accounts (active + deactivated), oldest first", body = ApiResponse<Vec<AccountDto>>),
    ),
    tag = "accounts",
)]
pub async fn list_accounts(
    State(store): State<Store>,
) -> Result<ApiResponse<Vec<AccountDto>>, ApiError> {
    let accounts = store.list_client_visible_accounts().await?;
    Ok(ApiResponse::new(
        accounts.into_iter().map(AccountDto::from).collect(),
    ))
}

/// Add or reactivate a Matrix account at runtime, then return the resulting
/// active account. Idempotent by `(homeserver_url, username)`: a new identity is
/// minted, a logged-out (`deactivated`) account is reactivated, and an
/// already-`active` account is returned **unchanged** (a no-op — the desired end
/// state already holds, so the password isn't consulted and nothing is touched).
/// An account mid-deletion (`deleting`) is a `409`. `username` must be a full
/// Matrix user ID (a malformed one is a `400`); bad credentials are a `401`. The
/// password is used once and never stored.
///
/// `homeserver_url` is optional: when omitted, the server discovers the
/// canonical homeserver from the user ID's server name (`.well-known/matrix/client`,
/// falling back to the server name itself) — so clients need only username +
/// password, and one canonical URL keys the identity no matter who logs it in.
/// A failed discovery is a `502`. The `username` domain is then checked against
/// the homeserver's own declared server name (best-effort): a user ID written
/// with the homeserver's hostname (`@adam:matrix.example.org`) is a `400` whose
/// message suggests the ID they almost certainly meant (`@adam:example.org`),
/// rather than a misleading `401` — and never a silent login as a different
/// identity.
///
/// Secret-bearing, so this route is loopback-only until the auth layer lands (see
/// the `route_layer` in [`router`](crate::router)).
#[utoipa::path(
    post,
    path = "/v1/accounts/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "The active account (newly logged in, reactivated, or already active)", body = ApiResponse<AccountDto>),
        (status = 400, description = "Malformed request (e.g. invalid user ID, or a user ID written with the homeserver's hostname — the message suggests the canonical spelling)", body = crate::response::ErrorResponse),
        (status = 401, description = "Credentials rejected by the homeserver", body = crate::response::ErrorResponse),
        (status = 409, description = "The account is being deleted", body = crate::response::ErrorResponse),
        (status = 502, description = "Upstream homeserver error (including failed homeserver discovery)", body = crate::response::ErrorResponse),
    ),
    tag = "accounts",
)]
pub async fn login(
    State(store): State<Store>,
    State(lifecycle): State<Arc<dyn AccountLifecycle>>,
    Json(req): Json<LoginRequest>,
) -> Result<ApiResponse<AccountDto>, ApiError> {
    let account_id = lifecycle
        .login(req.homeserver_url.as_deref(), &req.username, &req.password)
        .await?;
    // Read the row back so the response reflects the persisted state (device id,
    // timestamps) rather than re-deriving it. It was just made active, so a
    // missing row here is a real internal inconsistency.
    let account = store
        .get_account(account_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    Ok(ApiResponse::new(AccountDto::from(account)))
}

/// Log a Matrix account out, then return the resulting (now `deactivated`)
/// account. Stops syncing it, invalidates its device token upstream (best-effort
/// — an unreachable homeserver never fails the logout), and moves it to a
/// logged-out state, **retaining all of its data** (archive, search, media) so a
/// later login reactivates the same `account_id`. Idempotent: logging out an
/// already-logged-out account is a `200` no-op. An account mid-deletion
/// (`deleting`) is a `409`; an unknown id is a `404`.
///
/// Secret-bearing / destructive, so this route is loopback-only until the auth
/// layer lands (see the `route_layer` in [`router`](crate::router)).
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/logout",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
    ),
    responses(
        (status = 200, description = "The logged-out (deactivated) account", body = ApiResponse<AccountDto>),
        (status = 404, description = "No such account", body = crate::response::ErrorResponse),
        (status = 409, description = "The account is being deleted", body = crate::response::ErrorResponse),
    ),
    tag = "accounts",
)]
pub async fn logout(
    State(store): State<Store>,
    State(lifecycle): State<Arc<dyn AccountLifecycle>>,
    Path(account_id): Path<Uuid>,
) -> Result<ApiResponse<AccountDto>, ApiError> {
    lifecycle.logout(account_id).await?;
    // Read the row back so the response reflects the persisted (deactivated) state.
    // It was just transitioned, so a missing row here is a real inconsistency.
    let account = store
        .get_account(account_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    Ok(ApiResponse::new(AccountDto::from(account)))
}

/// Read a single account by id, in whatever lifecycle state it is — unlike the
/// list, a direct by-id read is not filtered to `active` (so a client can poll
/// an account it knows and watch it transition). An unknown id is a 404.
#[utoipa::path(
    get,
    path = "/v1/accounts/{account_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
    ),
    responses(
        (status = 200, description = "The account", body = ApiResponse<AccountDto>),
        (status = 404, description = "No such account", body = crate::response::ErrorResponse),
    ),
    tag = "accounts",
)]
pub async fn get_account(
    State(store): State<Store>,
    Path(account_id): Path<Uuid>,
) -> Result<ApiResponse<AccountDto>, ApiError> {
    let account = store
        .get_account(account_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("account {account_id} not found")))?;
    Ok(ApiResponse::new(AccountDto::from(account)))
}
