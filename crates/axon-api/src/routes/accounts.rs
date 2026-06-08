//! Account read endpoints: list the accounts this Axon manages, and read one.
//!
//! These are pure store reads (no secrets — the access token is never exposed)
//! and, unlike the destructive/secret-bearing lifecycle verbs (login / recover /
//! logout / delete, later subphases), are ordinary `/v1/` routes, not
//! loopback-restricted.

use axon_store::Store;
use axum::extract::State;
use uuid::Uuid;

use crate::dto::AccountDto;
use crate::extract::Path;
use crate::response::{ApiError, ApiResponse};

/// List the accounts this Axon manages, oldest first — the `active` ones.
///
/// Logged-out (`deactivated`) accounts are not yet surfaced here: nothing can
/// produce one until the logout verb lands, and showing them is part of that
/// reactivation UX (a later subphase will widen this list, likely behind an
/// explicit filter). A specific account can still be read in any state by id
/// via [`get_account`].
#[utoipa::path(
    get,
    path = "/v1/accounts",
    responses(
        (status = 200, description = "Active accounts, oldest first", body = ApiResponse<Vec<AccountDto>>),
    ),
    tag = "accounts",
)]
pub async fn list_accounts(
    State(store): State<Store>,
) -> Result<ApiResponse<Vec<AccountDto>>, ApiError> {
    let accounts = store.list_accounts().await?;
    Ok(ApiResponse::new(
        accounts.into_iter().map(AccountDto::from).collect(),
    ))
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
