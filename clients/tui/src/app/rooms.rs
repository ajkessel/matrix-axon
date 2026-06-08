use crate::api::{EventDto, RoomDto};

use super::{
    display_body_with_sender, format_time, relative_room_index, App, RoomKey, RoomTargetResolution,
    Status, TIMELINE_LIMIT,
};

impl App {
    pub(crate) async fn refresh_rooms(&mut self) {
        match self.client.list_rooms(self.account_filter).await {
            Ok(rooms) => self.apply_room_refresh(rooms),
            Err(err) => {
                self.status = Status::from(format!("room refresh failed: {err}"));
            }
        }
    }

    pub(crate) fn apply_room_refresh(&mut self, rooms: Vec<RoomDto>) {
        let selected_key = self.selected_room().map(RoomKey::from);
        self.rooms.rooms = rooms;
        self.rooms.unread.retain(|key, _| {
            self.rooms
                .rooms
                .iter()
                .any(|room| RoomKey::from(room) == *key)
        });
        self.rooms.selected = selected_key
            .and_then(|key| {
                self.rooms
                    .rooms
                    .iter()
                    .position(|room| RoomKey::from(room) == key)
            })
            .or_else(|| {
                self.rooms
                    .selected
                    .filter(|index| *index < self.rooms.rooms.len())
            });
        self.seed_own_senders_from_rooms();
        if self.rooms.rooms.is_empty() {
            self.rooms.selected = None;
            self.status = Status::from("no rooms returned by Axon".to_owned());
        } else if self.rooms.selected.is_none() {
            self.rooms.selected = Some(0);
            self.status = Status::from(format!("loaded {} rooms", self.rooms.rooms.len()));
        } else {
            self.status = Status::from(format!("refreshed {} rooms", self.rooms.rooms.len()));
        }
    }

    pub(crate) fn seed_own_senders_from_rooms(&mut self) {
        self.live
            .own_senders
            .extend(self.rooms.rooms.iter().filter_map(|room| {
                room.account_user_id
                    .as_ref()
                    .map(|user_id| (room.account_id, user_id.clone()))
            }));
    }

    pub(crate) async fn load_selected_timeline(&mut self) {
        let Some(room) = self.selected_room().cloned() else {
            return;
        };
        self.messages.selection = None;
        self.messages.scroll = usize::MAX;
        match self
            .client
            .room_timeline(room.account_id, &room.room_id, None, TIMELINE_LIMIT)
            .await
        {
            Ok(mut page) => {
                page.events.reverse();
                apply_edits(&mut page.events);
                let has_more = page.next_cursor.is_some();
                self.rebuild_display_names(&room, &page.events);
                self.messages
                    .events
                    .insert(RoomKey::from(&room), page.events);
                self.rooms.unread.remove(&RoomKey::from(&room));
                self.status = Status::Info(if has_more {
                    format!("showing {} (older history available later)", room.title())
                } else {
                    format!("showing {}", room.title())
                });
            }
            Err(err) => {
                self.status = Status::from(format!("timeline load failed: {err}"));
            }
        }
    }

    pub(super) async fn switch_room(&mut self, target: &str) {
        let index = match self.resolve_room_target(target) {
            RoomTargetResolution::Match(index) => index,
            RoomTargetResolution::Ambiguous(options) => {
                self.status =
                    Status::Info(format!("room name is ambiguous: {}", options.join(", ")));
                return;
            }
            RoomTargetResolution::Missing => {
                self.status = Status::from(format!("room not found: {target}"));
                return;
            }
        };
        self.rooms.selected = Some(index);
        self.load_selected_timeline().await;
    }

    pub(crate) async fn switch_relative_room(&mut self, offset: isize) {
        if self.rooms.rooms.is_empty() {
            self.status = Status::from("no rooms to switch".to_owned());
            return;
        }
        let current = self.rooms.selected.unwrap_or(0);
        let len = self.rooms.rooms.len();
        let next = relative_room_index(current, len, offset);
        self.rooms.selected = Some(next);
        self.load_selected_timeline().await;
    }

    pub(super) async fn show_event(&mut self, event_id: &str) {
        let Some(room) = self.selected_room() else {
            self.status = Status::from("select a room before using /event".to_owned());
            return;
        };
        match self.client.get_event(room.account_id, event_id).await {
            Ok(event) => {
                let sender = self.sender_label(&event);
                let relation = if event.relates_to.is_some() {
                    " related"
                } else {
                    ""
                };
                let redaction = event
                    .redaction_event_id
                    .as_deref()
                    .map(|id| format!(" redacted_by={id}"))
                    .unwrap_or_default();
                self.status = Status::Info(format!(
                    "{} {} {} {}{}{}",
                    format_time(event.origin_ts),
                    sender,
                    event.event_id,
                    display_body_with_sender(&event, &sender)
                        .chars()
                        .take(120)
                        .collect::<String>(),
                    relation,
                    redaction
                ));
            }
            Err(err) => self.status = Status::Info(format!("event read failed: {err}")),
        }
    }
}

fn apply_edits(events: &mut Vec<EventDto>) {
    let edits: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| {
            let (target, body) = event.edit_relation()?;
            Some((target.to_owned(), body.to_owned()))
        })
        .collect();
    for (target_id, new_body) in &edits {
        if let Some(event) = events.iter_mut().find(|event| &event.event_id == target_id) {
            event.body = Some(new_body.clone());
        }
    }
    events.retain(|event| event.edit_relation().is_none());
}
