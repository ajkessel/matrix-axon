//! Cached room-member profile reads for API avatar enrichment.
//!
//! This is intentionally a local SDK-cache read. It does not call
//! `sync_members()` and does not fetch global Matrix profiles from the
//! homeserver, so `GET /members` cannot fan out into one upstream request per
//! avatar-less user.

use std::collections::HashSet;

use axon_store::{AccountState, Store};
use matrix_sdk::{ruma::OwnedRoomId, RoomMemberships};
use uuid::Uuid;

use crate::error::GatewayError;
use crate::manager::ClientManager;

/// What can go wrong reading cached member profiles. The adapter in
/// `axon-server` maps these onto `axon-api`'s best-effort error type.
#[derive(Debug, thiserror::Error)]
pub enum MemberProfileError {
    /// No account exists for the given id.
    #[error("no such account: {0}")]
    AccountNotFound(Uuid),
    /// The account is logged out (`deactivated`) or mid-teardown (`deleting`).
    #[error("account is not active: {0}")]
    NotActive(Uuid),
    /// The room id is not syntactically valid.
    #[error("invalid room id: {0}")]
    InvalidRoomId(String),
    /// One requested user id is not syntactically valid.
    #[error("invalid user id: {0}")]
    InvalidUserId(String),
    /// The SDK client could not be restored or queried.
    #[error("upstream error: {0}")]
    Upstream(String),
    /// Local store or SDK cache failure.
    #[error("store error: {0}")]
    Store(String),
}

/// Cached profile data for one room member.
#[derive(Debug, Clone)]
pub struct MemberProfile {
    pub user_id: String,
    pub avatar_url: Option<String>,
}

/// Reads cached room-member profiles. Cheap to clone (handles only). Built by
/// [`SyncEngine::member_profiles`](crate::SyncEngine::member_profiles) and
/// adapted onto the API's member-profile port by `axon-server`.
#[derive(Clone)]
pub struct MemberProfileEngine {
    store: Store,
    manager: ClientManager,
}

impl MemberProfileEngine {
    pub(crate) fn new(store: Store, manager: ClientManager) -> Self {
        Self { store, manager }
    }

    /// Return cached SDK room-member profile data for `user_ids`.
    ///
    /// Read-only and no lifecycle lock: this is a best-effort cache lookup used
    /// to decorate the already-durable `/members` response. A concurrent
    /// logout/delete can at worst make enrichment unavailable, in which case the
    /// API route keeps returning the store-backed member rows unchanged.
    pub async fn profiles(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_ids: &[String],
    ) -> Result<Vec<MemberProfile>, MemberProfileError> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let account = self
            .store
            .get_account(account_id)
            .await
            .map_err(|e| MemberProfileError::Store(e.to_string()))?
            .ok_or(MemberProfileError::AccountNotFound(account_id))?;
        if account.state != AccountState::Active {
            return Err(MemberProfileError::NotActive(account_id));
        }

        let room_id: OwnedRoomId = room_id
            .parse()
            .map_err(|_| MemberProfileError::InvalidRoomId(room_id.to_owned()))?;
        for user_id in user_ids {
            user_id
                .parse::<matrix_sdk::ruma::OwnedUserId>()
                .map_err(|_| MemberProfileError::InvalidUserId(user_id.to_owned()))?;
        }
        let requested: HashSet<&str> = user_ids.iter().map(String::as_str).collect();

        let client = self
            .manager
            .get_or_connect(account_id)
            .await
            .map_err(|e| match e {
                GatewayError::UnknownAccount(id) => MemberProfileError::AccountNotFound(id),
                GatewayError::AccountNotActive(id) => MemberProfileError::NotActive(id),
                other => MemberProfileError::Upstream(other.to_string()),
            })?;
        let Some(room) = client.get_room(&room_id) else {
            return Ok(Vec::new());
        };

        let members = room
            .members_no_sync(RoomMemberships::empty())
            .await
            .map_err(|e| MemberProfileError::Store(e.to_string()))?;

        Ok(members
            .into_iter()
            .filter(|member| requested.contains(member.user_id().as_str()))
            .map(|member| MemberProfile {
                user_id: member.user_id().to_string(),
                avatar_url: member.avatar_url().map(ToString::to_string),
            })
            .collect())
    }
}
