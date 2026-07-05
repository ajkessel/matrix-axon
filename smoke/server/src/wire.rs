use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub run_id: String,
    pub axon_base_url: String,
    pub axon_bearer_token: String,
    pub homeserver_url: String,
    pub axon_account_id: Uuid,
    pub manual: Manual,
    pub accounts: Accounts,
    pub rooms: Rooms,
    pub fixtures: Fixtures,
    pub paths: Paths,
    pub keep_up: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manual {
    pub tui_command: String,
    pub matrix_client_homeserver: String,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Accounts {
    pub target: MatrixAccount,
    pub peer: MatrixAccount,
    pub observer: MatrixAccount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixAccount {
    pub user_id: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rooms {
    pub general: RoomInfo,
    pub long_timeline: TimelineRoomInfo,
    pub relations: RoomInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub room_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineRoomInfo {
    pub room_id: String,
    pub name: String,
    pub jump_dates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixtures {
    pub relations: RelationFixtures,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationFixtures {
    pub root_event_id: String,
    pub reply_event_id: String,
    pub edit_event_id: String,
    pub peer_reaction_event_id: String,
    pub own_reaction_event_id: String,
    pub formatted_event_id: String,
    pub redacted_event_id: String,
    pub redaction_event_id: String,
    pub thread_root_event_id: String,
    pub thread_member_event_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    pub run_dir: PathBuf,
    pub axon_log: PathBuf,
    pub compose_project: String,
    pub database_name: String,
    pub axon_pid: u32,
    pub postgres_port: u16,
    pub synapse_port: u16,
    pub axon_port: u16,
}

#[derive(Debug, Deserialize)]
pub struct ApiResponse<T> {
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub struct AccountDto {
    pub account_id: Uuid,
    pub user_id: String,
    pub homeserver_url: String,
    pub device_id: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct RoomDto {
    pub account_id: Uuid,
    pub room_id: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TimelinePage {
    pub events: Vec<EventDto>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct EventDto {
    pub account_id: Uuid,
    pub event_id: String,
    pub room_id: String,
    pub sender: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub content: Option<Value>,
    pub body: Option<String>,
    pub redacted: bool,
    pub redaction_event_id: Option<String>,
    pub edited: bool,
    pub edit_count: i64,
    pub reactions: Option<BTreeMap<String, ReactionDto>>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ReactionDto {
    pub count: i64,
    pub me: bool,
    #[serde(default)]
    pub senders: Vec<String>,
    #[serde(default)]
    pub my_event_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SendResultDto {
    pub event_id: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ThreadSummaryDto {
    pub root_event_id: String,
    pub reply_count: i64,
    pub latest_reply: Option<EventDto>,
}

#[derive(Debug, Deserialize)]
pub struct WsEnvelope {
    #[serde(rename = "type")]
    pub kind: String,
    pub account_id: Uuid,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct MatrixLoginResponse {
    pub access_token: String,
}

#[derive(Debug, Deserialize)]
pub struct MatrixSendResponse {
    pub event_id: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct MatrixSyncResponse {
    #[serde(default)]
    pub rooms: MatrixSyncRooms,
    #[serde(default)]
    pub next_batch: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatrixSyncRooms {
    #[serde(default)]
    pub join: BTreeMap<String, MatrixJoinedRoom>,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatrixJoinedRoom {
    #[serde(default)]
    pub timeline: MatrixTimeline,
}

#[derive(Debug, Default, Deserialize)]
pub struct MatrixTimeline {
    #[serde(default)]
    pub events: Vec<MatrixEvent>,
}

#[derive(Debug, Deserialize)]
pub struct MatrixEvent {
    pub event_id: Option<String>,
    #[serde(default)]
    pub content: Value,
}
