//! Room member profile enrichment for `GET …/rooms/{room_id}/members`.
//!
//! `axon-api` owns the HTTP route and DTO shape, but it must not depend on
//! `axon-sync` or `matrix-sdk`. This port lets the route ask for cached SDK
//! room-member profile data as a best-effort fallback when Axon's persisted
//! `m.room.member` state row lacks an avatar URL.

use async_trait::async_trait;
use uuid::Uuid;

/// Cached profile data for one room member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberProfile {
    pub user_id: String,
    pub avatar_url: Option<String>,
}

/// Best-effort enrichment failure. The `/members` route logs this and returns
/// the stored state rows unchanged; avatar fallback must not make the member
/// list unavailable.
#[derive(Debug)]
pub enum MemberProfileError {
    /// No such account.
    NotFound(String),
    /// The account is logged out or mid-teardown, so no SDK cache is available.
    NotActive(String),
    /// The room id or a user id was syntactically invalid.
    BadRequest(String),
    /// SDK/client-manager failure.
    Upstream(String),
    /// Local store or SDK cache failure.
    Internal(String),
}

impl std::fmt::Display for MemberProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MemberProfileError::NotFound(msg)
            | MemberProfileError::NotActive(msg)
            | MemberProfileError::BadRequest(msg)
            | MemberProfileError::Upstream(msg)
            | MemberProfileError::Internal(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for MemberProfileError {}

/// Reads cached room-member profiles. Implemented outside this crate; held in
/// [`AppState`](crate::AppState) as `Arc<dyn MemberProfileService>`.
#[async_trait]
pub trait MemberProfileService: Send + Sync {
    async fn profiles(
        &self,
        account_id: Uuid,
        room_id: &str,
        user_ids: &[String],
    ) -> Result<Vec<MemberProfile>, MemberProfileError>;
}

/// No-op implementation used by tests and configurations that do not inject a
/// sync-backed profile service.
pub struct NoopMemberProfileService;

#[async_trait]
impl MemberProfileService for NoopMemberProfileService {
    async fn profiles(
        &self,
        _account_id: Uuid,
        _room_id: &str,
        _user_ids: &[String],
    ) -> Result<Vec<MemberProfile>, MemberProfileError> {
        Ok(Vec::new())
    }
}
