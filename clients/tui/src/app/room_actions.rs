//! M19 room membership and moderation actions.

use crate::api::RoomDto;
use crate::command::UserReasonCommand;

use super::{App, Mode, RoomKey, RoomTargetResolution, Status};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoomActionKind {
    Leave,
    Forget,
    Invite,
    Kick,
    Ban,
    Unban,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingRoomAction {
    pub(crate) kind: RoomActionKind,
    pub(crate) key: RoomKey,
    pub(crate) room_title: String,
    pub(crate) user_id: Option<String>,
    pub(crate) reason: Option<String>,
}

#[derive(Debug)]
pub(crate) struct RoomActionOutcome {
    pub(crate) action: PendingRoomAction,
    pub(crate) result: Result<(), String>,
}

impl RoomActionKind {
    fn present_participle(self) -> &'static str {
        match self {
            Self::Leave => "leaving",
            Self::Forget => "forgetting",
            Self::Invite => "inviting",
            Self::Kick => "kicking",
            Self::Ban => "banning",
            Self::Unban => "unbanning",
        }
    }

    fn past_tense(self) -> &'static str {
        match self {
            Self::Leave => "left",
            Self::Forget => "forgot",
            Self::Invite => "invited",
            Self::Kick => "kicked",
            Self::Ban => "banned",
            Self::Unban => "unbanned",
        }
    }

    fn command_name(self) -> &'static str {
        match self {
            Self::Leave => "leave",
            Self::Forget => "forget",
            Self::Invite => "invite",
            Self::Kick => "kick",
            Self::Ban => "ban",
            Self::Unban => "unban",
        }
    }

    fn needs_confirmation(self) -> bool {
        !matches!(self, Self::Invite)
    }

    fn removes_room_from_list(self) -> bool {
        matches!(self, Self::Leave | Self::Forget)
    }
}

impl App {
    pub(crate) fn start_leave_room(&mut self) {
        let Some(action) = self.action_for_selected_room(RoomActionKind::Leave, None, None) else {
            return;
        };
        self.request_or_perform_room_action(action);
    }

    pub(crate) fn start_forget_room(&mut self, target: Option<&str>) {
        let room = match target.filter(|target| !target.trim().is_empty()) {
            Some(target) => match self.resolve_room_target(target) {
                RoomTargetResolution::Match(index) => self.rooms.rooms.get(index).cloned(),
                RoomTargetResolution::Ambiguous(options) => {
                    self.status =
                        Status::Info(format!("room name is ambiguous: {}", options.join(", ")));
                    return;
                }
                RoomTargetResolution::Missing => {
                    self.status = Status::from(format!("room not found: {target}"));
                    return;
                }
            },
            None => self.selected_room().cloned(),
        };
        let Some(room) = room else {
            self.status = Status::from("select a room before using /forget".to_owned());
            return;
        };
        let action = action_for_room(&room, RoomActionKind::Forget, None, None);
        self.request_or_perform_room_action(action);
    }

    pub(crate) fn start_invite_user(&mut self, user_id: &str) {
        let Some(action) =
            self.action_for_selected_room(RoomActionKind::Invite, Some(user_id), None)
        else {
            return;
        };
        self.request_or_perform_room_action(action);
    }

    pub(crate) fn start_kick_user(&mut self, input: UserReasonCommand) {
        self.start_moderation_action(RoomActionKind::Kick, input);
    }

    pub(crate) fn start_ban_user(&mut self, input: UserReasonCommand) {
        self.start_moderation_action(RoomActionKind::Ban, input);
    }

    pub(crate) fn start_unban_user(&mut self, input: UserReasonCommand) {
        self.start_moderation_action(RoomActionKind::Unban, input);
    }

    fn start_moderation_action(&mut self, kind: RoomActionKind, input: UserReasonCommand) {
        let Some(action) =
            self.action_for_selected_room(kind, Some(&input.user_id), input.reason.as_deref())
        else {
            return;
        };
        self.request_or_perform_room_action(action);
    }

    fn action_for_selected_room(
        &mut self,
        kind: RoomActionKind,
        user_id: Option<&str>,
        reason: Option<&str>,
    ) -> Option<PendingRoomAction> {
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from(format!(
                "select a room before using /{}",
                kind.command_name()
            ));
            return None;
        };
        Some(action_for_room(&room, kind, user_id, reason))
    }

    fn request_or_perform_room_action(&mut self, action: PendingRoomAction) {
        if self.room_action_busy {
            self.status = Status::from("a room action is already in progress".to_owned());
            return;
        }
        if action.kind.needs_confirmation() {
            self.clear_input_buffer();
            self.status = Status::from(confirmation_status(&action));
            self.mode = Mode::ConfirmRoomAction { action };
        } else {
            self.perform_room_action(action);
        }
    }

    pub(crate) fn cancel_room_action_confirmation(&mut self) {
        self.mode = Mode::Compose;
        self.status = Status::Info("room action canceled".to_owned());
    }

    pub(crate) fn perform_room_action(&mut self, action: PendingRoomAction) {
        self.clear_input_buffer();
        self.mode = Mode::Compose;
        let Some(tx) = self.room_action_tx.clone() else {
            return;
        };
        let status = progress_status(&action);
        self.status = Status::from(status);
        self.room_action_busy = true;
        if matches!(action.kind, RoomActionKind::Leave | RoomActionKind::Forget) {
            self.stop_typing_for_room(&action.key);
        }
        let client = self.client.clone();
        let task_action = action.clone();
        tokio::spawn(async move {
            let account_id = task_action.key.account_id;
            let room_id = task_action.key.room_id.as_str();
            let result = match task_action.kind {
                RoomActionKind::Leave => client.leave_room(account_id, room_id).await,
                RoomActionKind::Forget => client.forget_room(account_id, room_id).await,
                RoomActionKind::Invite => {
                    client
                        .invite_user(
                            account_id,
                            room_id,
                            task_action.user_id.as_deref().unwrap_or_default(),
                        )
                        .await
                }
                RoomActionKind::Kick => {
                    client
                        .kick_user(
                            account_id,
                            room_id,
                            task_action.user_id.as_deref().unwrap_or_default(),
                            task_action.reason.as_deref(),
                        )
                        .await
                }
                RoomActionKind::Ban => {
                    client
                        .ban_user(
                            account_id,
                            room_id,
                            task_action.user_id.as_deref().unwrap_or_default(),
                            task_action.reason.as_deref(),
                        )
                        .await
                }
                RoomActionKind::Unban => {
                    client
                        .unban_user(
                            account_id,
                            room_id,
                            task_action.user_id.as_deref().unwrap_or_default(),
                            task_action.reason.as_deref(),
                        )
                        .await
                }
            }
            .map_err(|err| err.to_string());
            let _ = tx.send(RoomActionOutcome {
                action: task_action,
                result,
            });
        });
    }

    pub(crate) async fn handle_room_action_outcome(&mut self, outcome: RoomActionOutcome) {
        self.room_action_busy = false;
        match outcome.result {
            Ok(()) => {
                let departed_selected =
                    self.selected_room().map(RoomKey::from) == Some(outcome.action.key.clone());
                let refresh_warning = match self.client.list_rooms(self.account_filter).await {
                    Ok(rooms) => {
                        self.apply_room_refresh(rooms);
                        None
                    }
                    Err(err) => Some(err.to_string()),
                };
                if outcome.action.kind.removes_room_from_list() && departed_selected {
                    self.sync_draft_on_room_change();
                    if self.selected_room().is_some() {
                        self.load_selected_timeline().await;
                    }
                }
                if matches!(
                    outcome.action.kind,
                    RoomActionKind::Invite
                        | RoomActionKind::Kick
                        | RoomActionKind::Ban
                        | RoomActionKind::Unban
                ) {
                    self.spawn_members_refresh(outcome.action.key.clone());
                }
                self.status = Status::from(match refresh_warning {
                    Some(err) => format!(
                        "{}; room refresh failed: {err}",
                        success_status(&outcome.action)
                    ),
                    None => success_status(&outcome.action),
                });
            }
            Err(err) => {
                self.status = Status::from(format!(
                    "/{} failed in {}: {err}",
                    outcome.action.kind.command_name(),
                    outcome.action.room_title
                ));
            }
        }
    }
}

fn action_for_room(
    room: &RoomDto,
    kind: RoomActionKind,
    user_id: Option<&str>,
    reason: Option<&str>,
) -> PendingRoomAction {
    PendingRoomAction {
        kind,
        key: RoomKey::from(room),
        room_title: room.title().to_owned(),
        user_id: user_id.map(str::to_owned),
        reason: reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(str::to_owned),
    }
}

fn confirmation_status(action: &PendingRoomAction) -> String {
    match action.kind {
        RoomActionKind::Leave => format!("Leave {}? [y/N]", action.room_title),
        RoomActionKind::Forget => format!("Forget {}? [y/N]", action.room_title),
        RoomActionKind::Kick => format!(
            "Kick {} from {}? [y/N]",
            action.user_id.as_deref().unwrap_or("user"),
            action.room_title
        ),
        RoomActionKind::Ban => format!(
            "Ban {} from {}? [y/N]",
            action.user_id.as_deref().unwrap_or("user"),
            action.room_title
        ),
        RoomActionKind::Unban => format!(
            "Unban {} from {}? [y/N]",
            action.user_id.as_deref().unwrap_or("user"),
            action.room_title
        ),
        RoomActionKind::Invite => progress_status(action),
    }
}

fn progress_status(action: &PendingRoomAction) -> String {
    match action.user_id.as_deref() {
        Some(user_id) => format!(
            "{} {user_id} in {}...",
            action.kind.present_participle(),
            action.room_title
        ),
        None => format!(
            "{} {}...",
            action.kind.present_participle(),
            action.room_title
        ),
    }
}

fn success_status(action: &PendingRoomAction) -> String {
    match action.user_id.as_deref() {
        Some(user_id) => format!(
            "{} {user_id} in {}",
            action.kind.past_tense(),
            action.room_title
        ),
        None => format!("{} {}", action.kind.past_tense(), action.room_title),
    }
}
