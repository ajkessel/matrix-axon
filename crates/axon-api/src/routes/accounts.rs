//! Account read endpoints: list the accounts this Axon manages, and read one.
//!
//! These are pure store reads (no secrets — the access token is never exposed).
//! Like every `/v1/` route — the reads here and the destructive/secret-bearing
//! lifecycle verbs alike — they sit behind the bearer-token gate (M7b, ADR 0029).

use std::sync::Arc;

use axon_store::Store;
use axum::extract::State;
use uuid::Uuid;

use crate::dto::{AccountDto, LoginRequest, RecoverRequest, RedecryptUtdsResponse};
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
/// active account. Idempotent by Matrix `username`: a new identity is minted, a
/// logged-out (`deactivated`) account is reactivated using its stored endpoint,
/// and an already-`active` account is returned **unchanged** (a no-op — the desired end
/// state already holds, so the password isn't consulted and nothing is touched).
/// An account mid-deletion (`deleting`) is a `409`. `username` must be a full
/// Matrix user ID (a malformed one is a `400`); bad credentials are a `401`. The
/// password is used once and never stored.
///
/// `homeserver_url` is optional: when omitted, the server discovers the
/// canonical homeserver from the user ID's server name (`.well-known/matrix/client`,
/// falling back to the server name itself) — so clients need only username +
/// password. The URL is a connection endpoint; the Matrix ID keys identity.
/// A failed discovery is a `502`. The `username` domain is then checked against
/// the homeserver's own declared server name (best-effort): a user ID written
/// with the homeserver's hostname (`@adam:matrix.example.org`) is a `400` whose
/// message suggests the ID they almost certainly meant (`@adam:example.org`),
/// rather than a misleading `401` — and never a silent login as a different
/// identity.
///
/// Secret-bearing; gated by the bearer-token auth layer like every `/v1/` route
/// (M7b, ADR 0029).
#[utoipa::path(
    post,
    path = "/v1/accounts/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "The active account (newly logged in, reactivated, or already active)", body = ApiResponse<AccountDto>),
        (status = 400, description = "Malformed request (e.g. invalid user ID, or a user ID written with the homeserver's hostname — the message suggests the canonical spelling)", body = crate::response::ErrorResponse),
        (status = 401, description = "Either the bearer gate rejected the request before the handler ran (missing, malformed, or revoked token — carries a WWW-Authenticate: Bearer challenge), or the request was authorized but the Matrix homeserver rejected the supplied credentials (post-auth — no challenge).", body = crate::response::ErrorResponse, headers(
            ("WWW-Authenticate" = String, description = "RFC 6750 bearer challenge, present only on a gate rejection: `Bearer` for a missing/malformed credential, `Bearer error=\"invalid_token\"` for an unknown or revoked token. Absent when the 401 is the homeserver rejecting the supplied Matrix credentials."),
        )),
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
/// Secret-bearing / destructive; gated by the bearer-token auth layer like every
/// `/v1/` route (M7b, ADR 0029).
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

/// Acquire E2EE keys for an **active** account from its Secure-Storage (4S)
/// recovery key, then return the account with its `verified` flag re-derived. One
/// SDK call imports the account's megolm key backup + cross-signing keys —
/// self-verifying axon's device with no interactive partner — and any
/// already-stored UTDs the keys unlock are back-filled. The recovery key is used
/// once and never persisted.
///
/// A `200` means the **keys were imported** (the recovery succeeded); the returned
/// `verified` reflects the freshly-derived cross-signing state, which is `true` in
/// the normal case but is a *derived observation*, not a guaranteed postcondition
/// (e.g. a partial Secure-Backup that imports the megolm key but not the
/// cross-signing keys would import successfully yet stay unverified).
///
/// The account must be `active`: a logged-out (`deactivated`) account is a `409`
/// (log in first), as is one mid-deletion (`deleting`). A wrong/rotated key, or an
/// account that never set up Secure Backup, is a `400` (a readable error, not a
/// silent permanent UTD). An unknown id is a `404`.
///
/// Secret-bearing; gated by the bearer-token auth layer like every `/v1/` route
/// (M7b, ADR 0029).
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/recover",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
    ),
    request_body = RecoverRequest,
    responses(
        (status = 200, description = "Keys imported; account returned with `verified` re-derived (normally true)", body = ApiResponse<AccountDto>),
        (status = 400, description = "The recovery key was wrong/rotated, or the account has no Secure Backup", body = crate::response::ErrorResponse),
        (status = 404, description = "No such account", body = crate::response::ErrorResponse),
        (status = 409, description = "The account is not active (logged out — log in first) or is being deleted", body = crate::response::ErrorResponse),
    ),
    tag = "accounts",
)]
pub async fn recover(
    State(store): State<Store>,
    State(lifecycle): State<Arc<dyn AccountLifecycle>>,
    Path(account_id): Path<Uuid>,
    Json(req): Json<RecoverRequest>,
) -> Result<ApiResponse<AccountDto>, ApiError> {
    lifecycle.recover(account_id, &req.recovery_key).await?;
    // Read the row back so the response reflects the freshly-derived `verified`
    // state. The account was just operated on, so a missing row is a real
    // inconsistency.
    let account = store
        .get_account(account_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    Ok(ApiResponse::new(AccountDto::from(account)))
}

/// Explicitly retry every pending UTD for an active account. The default startup
/// policy attempts each stored UTD once, then waits for fresh room-key arrivals;
/// this endpoint is the authenticated operator escape hatch for rows whose keys
/// are already in the crypto store or whose initial startup attempt predated key
/// acquisition.
#[utoipa::path(
    post,
    path = "/v1/accounts/{account_id}/utds/redecrypt",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
    ),
    responses(
        (status = 200, description = "Manual UTD re-decryption retry completed or timed out", body = ApiResponse<RedecryptUtdsResponse>),
        (status = 404, description = "No such account", body = crate::response::ErrorResponse),
        (status = 409, description = "The account is not active (logged out — log in first) or is being deleted", body = crate::response::ErrorResponse),
    ),
    tag = "accounts",
)]
pub async fn redecrypt_utds(
    State(lifecycle): State<Arc<dyn AccountLifecycle>>,
    Path(account_id): Path<Uuid>,
) -> Result<ApiResponse<RedecryptUtdsResponse>, ApiError> {
    let stats = lifecycle.redecrypt_utds(account_id).await?;
    Ok(ApiResponse::new(RedecryptUtdsResponse::from(stats)))
}

/// Permanently delete an account and every trace of it: stops syncing it,
/// invalidates its device token upstream, removes its on-disk SDK store, and drops
/// its row (cascading away its archived events, room state, and account data).
/// Returns `204 No Content` — the resource is gone, so there is nothing to return.
///
/// Unlike [`logout`] this is **not** reversible: re-adding the same Matrix account
/// later is a fresh login with a new `account_id`. Idempotent and crash-safe — a
/// delete of an account already mid-teardown resumes it, and an interrupted delete
/// is finished by the next boot's reconcile. An unknown id is a `404`. A `409` means
/// either the account's sync task has not finished shutting down (transient, retry)
/// or the account is still named by `sync.account` — boot provisioning would
/// recreate it, so remove it from config before deleting.
///
/// Destructive; gated by the bearer-token auth layer like every `/v1/` route
/// (M7b, ADR 0029).
#[utoipa::path(
    delete,
    path = "/v1/accounts/{account_id}",
    params(
        ("account_id" = Uuid, Path, description = "Axon account id"),
    ),
    responses(
        (status = 204, description = "The account was deleted (or was already gone after a resumed teardown)"),
        (status = 404, description = "No such account", body = crate::response::ErrorResponse),
        (status = 409, description = "The sync task is still shutting down (retry), or the account is provisioned from config (remove it from sync.account first)", body = crate::response::ErrorResponse),
    ),
    tag = "accounts",
)]
pub async fn delete_account(
    State(lifecycle): State<Arc<dyn AccountLifecycle>>,
    Path(account_id): Path<Uuid>,
) -> Result<axum::http::StatusCode, ApiError> {
    lifecycle.delete(account_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
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
