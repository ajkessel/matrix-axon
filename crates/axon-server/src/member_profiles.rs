//! Composition-root adapter: binds `axon-sync`'s cached member-profile engine
//! to `axon-api`'s member-profile enrichment port.

use async_trait::async_trait;
use axon_api::{MemberProfile, MemberProfileError, MemberProfileService};
use axon_sync::{
    MemberProfile as SyncMemberProfile, MemberProfileEngine,
    MemberProfileError as SyncMemberProfileError,
};
use uuid::Uuid;

/// Wraps the sync engine's profile backend so it satisfies the API port. The
/// orphan rule requires a local newtype.
pub struct MemberProfileAdapter(pub MemberProfileEngine);

fn map_err(err: SyncMemberProfileError) -> MemberProfileError {
    match err {
        SyncMemberProfileError::AccountNotFound(id) => {
            MemberProfileError::NotFound(format!("account {id} not found"))
        }
        SyncMemberProfileError::NotActive(id) => MemberProfileError::NotActive(format!(
            "account {id} is not active; cached member profiles unavailable"
        )),
        SyncMemberProfileError::InvalidRoomId(room_id) => {
            MemberProfileError::BadRequest(format!("invalid room id: {room_id}"))
        }
        SyncMemberProfileError::InvalidUserId(user_id) => {
            MemberProfileError::BadRequest(format!("invalid user id: {user_id}"))
        }
        SyncMemberProfileError::Upstream(msg) => MemberProfileError::Upstream(msg),
        SyncMemberProfileError::Store(msg) => {
            tracing::error!(error = %msg, "store error reading cached member profiles");
            MemberProfileError::Internal("cached member profile store error".to_owned())
        }
    }
}

fn map_profile(profile: SyncMemberProfile) -> MemberProfile {
    MemberProfile {
        user_id: profile.user_id,
        avatar_url: profile.avatar_url,
    }
}

#[async_trait]
impl MemberProfileService for MemberProfileAdapter {
    async fn profiles(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_ids: &[String],
    ) -> Result<Vec<MemberProfile>, MemberProfileError> {
        self.0
            .profiles(account_id, room_id, user_ids)
            .await
            .map(|profiles| profiles.into_iter().map(map_profile).collect())
            .map_err(map_err)
    }
}
