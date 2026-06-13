use std::collections::HashMap;

use crate::api::{EventDto, LiveFrame, RoomDto};
use crate::config::{DisplayOptions, SenderNameStyle};

use super::{
    collect_reactions, match_status, message_index_at_line, message_line_ranges, next_match_index,
    selected_message_target_index, App, ConnectionState, LiveFrameAction, RoomKey, Status,
};

impl App {
    pub(crate) fn handle_live_frame(&mut self, frame: LiveFrame) -> LiveFrameAction {
        match frame {
            LiveFrame::Connected => {
                self.connection_state = ConnectionState::Connected;
                if !self.is_mid_command() {
                    self.status = Status::Debug("live WebSocket connected".to_owned());
                }
                LiveFrameAction::None
            }
            LiveFrame::Reconnecting { reason, delay } => {
                self.connection_state = ConnectionState::Reconnecting {
                    reason: reason.clone(),
                    delay,
                };
                if !self.is_mid_command() {
                    self.status = Status::Info(format!(
                        "live WebSocket reconnecting in {}s: {reason}",
                        delay.as_secs()
                    ));
                }
                LiveFrameAction::None
            }
            LiveFrame::Disconnected(reason) => {
                self.connection_state = ConnectionState::Disconnected(reason.clone());
                if !self.is_mid_command() {
                    self.status = Status::Debug(format!("live WebSocket disconnected: {reason}"));
                }
                LiveFrameAction::None
            }
            LiveFrame::ProtocolError(err) => {
                self.connection_state = ConnectionState::ProtocolError(err.clone());
                if !self.is_mid_command() {
                    self.status = Status::Debug(format!("ignored malformed live frame: {err}"));
                }
                LiveFrameAction::None
            }
            LiveFrame::Timeline(event) => self.append_live_event(*event),
        }
    }

    fn append_live_event(&mut self, event: EventDto) -> LiveFrameAction {
        let key = RoomKey {
            account_id: event.account_id,
            room_id: event.room_id.clone(),
        };
        if let Some((target_id, new_body)) = event.edit_relation() {
            if let Some(events) = self.messages.events.get_mut(&key) {
                if let Some(target) = events.iter_mut().find(|item| item.event_id == target_id) {
                    target.body = Some(new_body.to_owned());
                }
            }
            return LiveFrameAction::None;
        }
        let known_room = self
            .rooms
            .rooms
            .iter()
            .any(|room| RoomKey::from(room) == key);
        if self
            .selected_room()
            .is_some_and(|room| RoomKey::from(room) == key)
        {
            let visible_before = self.selected_display_line_count();
            let old_scroll_bottom = visible_before.saturating_sub(self.messages.page_size);
            let should_follow_tail =
                self.messages.scroll == usize::MAX || self.messages.scroll >= old_scroll_bottom;
            let should_select =
                self.messages.selection.is_none() && should_show_event(&event, &self.display);
            let event_id = event.event_id.clone();
            if self.live.pending_own_event_id.as_deref() == Some(&event_id) {
                self.live
                    .own_senders
                    .insert(event.account_id, event.sender.clone());
                self.live.pending_own_event_id = None;
            }
            self.remember_display_name_from_event(&key, &event);
            if self
                .messages
                .events
                .get(&key)
                .is_some_and(|events| events.iter().any(|e| e.event_id == event.event_id))
            {
                return LiveFrameAction::None;
            }
            // Kick off a background download for incoming image events.
            if let Some((account_id, mxc_url)) = event.image_mxc() {
                self.request_image(account_id, mxc_url);
            }
            self.messages
                .events
                .entry(key.clone())
                .or_default()
                .push(event);
            if should_follow_tail {
                self.messages.scroll = usize::MAX;
            }
            if should_select {
                self.messages.selection = Some(event_id);
            }
            self.rooms.unread.remove(&key);
            LiveFrameAction::None
        } else {
            if should_show_event(&event, &self.display) {
                *self.rooms.unread.entry(key).or_default() += 1;
            }
            if known_room {
                LiveFrameAction::None
            } else {
                LiveFrameAction::RefreshRooms
            }
        }
    }

    pub(crate) fn rebuild_display_names(&mut self, room: &RoomDto, events: &[EventDto]) {
        let key = RoomKey::from(room);
        self.rooms.display_names.remove(&key);
        for event in events {
            self.remember_display_name_from_event(&key, event);
        }
    }

    fn remember_display_name_from_event(&mut self, key: &RoomKey, event: &EventDto) {
        if event.event_type != "m.room.member" {
            return;
        }
        let user_id = event.state_key().unwrap_or(&event.sender);
        let Some(display_name) = event.membership_display_name() else {
            return;
        };
        self.rooms
            .display_names
            .entry(key.clone())
            .or_default()
            .insert(user_id.to_owned(), display_name.to_owned());
    }

    pub(crate) fn sender_label(&self, event: &EventDto) -> String {
        if self.display.sender_name == SenderNameStyle::MatrixAddress {
            return event.sender.clone();
        }
        let key = RoomKey {
            account_id: event.account_id,
            room_id: event.room_id.clone(),
        };
        self.rooms
            .display_names
            .get(&key)
            .and_then(|names| names.get(&event.sender))
            .filter(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| event.sender.clone())
    }

    pub(crate) fn selected_room(&self) -> Option<&RoomDto> {
        self.rooms
            .selected
            .and_then(|index| self.rooms.rooms.get(index))
    }

    pub(crate) fn selected_raw_events(&self) -> &[EventDto] {
        self.selected_room()
            .and_then(|room| self.messages.events.get(&RoomKey::from(room)))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub(crate) fn selected_reactions(&self) -> HashMap<String, Vec<(String, usize)>> {
        collect_reactions(self.selected_raw_events())
    }

    pub(crate) fn selected_events(&self) -> Vec<&EventDto> {
        self.selected_room()
            .and_then(|room| self.messages.events.get(&RoomKey::from(room)))
            .map(|events| {
                events
                    .iter()
                    .filter(|event| should_show_event(event, &self.display))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn selected_message_id(&self) -> Option<&str> {
        self.messages.selection.as_deref()
    }

    pub(super) fn selected_message_event(&self) -> Option<&EventDto> {
        let selected_message = self.messages.selection.as_deref()?;
        self.selected_events()
            .into_iter()
            .find(|event| event.event_id == selected_message)
    }

    pub(crate) fn select_first_message(&mut self) {
        let events = self.selected_events();
        if events.is_empty() {
            self.messages.selection = None;
            self.status = Status::from("no displayed messages".to_owned());
            return;
        }
        let count = events.len();
        let event_id = events[0].event_id.clone();
        self.messages.selection = Some(event_id);
        self.ensure_message_index_visible(0);
        self.status = Status::from(format!("selected message 1 of {count}"));
    }

    pub(crate) fn select_last_message(&mut self) {
        let events = self.selected_events();
        if events.is_empty() {
            self.messages.selection = None;
            self.status = Status::from("no displayed messages".to_owned());
            return;
        }
        let count = events.len();
        let last = count - 1;
        let event_id = events[last].event_id.clone();
        self.messages.selection = Some(event_id);
        self.ensure_message_index_visible(last);
        self.status = Status::from(format!("selected message {count} of {count}"));
    }

    pub(crate) fn move_selected_message(&mut self, offset: isize) {
        let Some((event_id, next, event_count)) = ({
            let events = self.selected_events();
            if events.is_empty() {
                None
            } else {
                let next = selected_message_target_index(
                    events.as_slice(),
                    self.messages.selection.as_deref(),
                    offset,
                );
                Some((events[next].event_id.clone(), next, events.len()))
            }
        }) else {
            self.messages.selection = None;
            self.status = Status::from("no displayed messages".to_owned());
            return;
        };
        self.messages.selection = Some(event_id);
        self.ensure_message_index_visible(next);
        self.status = Status::from(format!("selected message {} of {}", next + 1, event_count));
    }

    pub(crate) fn page_selected_message(&mut self, direction: isize) {
        let page = self.messages.page_size.max(1);
        let Some((event_id, next, event_count)) = ({
            let events = self.selected_events();
            if events.is_empty() {
                None
            } else {
                let sender_labels = self.sender_labels(events.as_slice());
                let reactions = self.selected_reactions();
                let ranges = message_line_ranges(
                    events.as_slice(),
                    sender_labels.as_slice(),
                    self.messages.width,
                    &reactions,
                    &self.colors,
                );
                let total_lines = ranges
                    .last()
                    .map(|range| range.end)
                    .unwrap_or_default()
                    .max(1);
                let current_index = self
                    .messages
                    .selection
                    .as_deref()
                    .and_then(|event_id| events.iter().position(|event| event.event_id == event_id))
                    .unwrap_or_else(|| {
                        if direction.is_negative() {
                            message_index_at_line(
                                ranges.as_slice(),
                                self.messages.scroll.saturating_add(page.saturating_sub(1)),
                            )
                        } else {
                            message_index_at_line(ranges.as_slice(), self.messages.scroll)
                        }
                    });
                let current_line = ranges
                    .get(current_index)
                    .map(|range| range.start)
                    .unwrap_or_default();
                let target_line = if direction.is_negative() {
                    current_line.saturating_sub(page)
                } else {
                    current_line
                        .saturating_add(page)
                        .min(total_lines.saturating_sub(1))
                };
                let next = message_index_at_line(ranges.as_slice(), target_line);
                Some((events[next].event_id.clone(), next, events.len()))
            }
        }) else {
            self.messages.selection = None;
            self.status = Status::from("no displayed messages".to_owned());
            return;
        };
        self.messages.selection = Some(event_id);
        self.ensure_message_index_visible(next);
        self.status = Status::from(format!("selected message {} of {}", next + 1, event_count));
    }

    pub(super) fn ensure_message_index_visible(&mut self, index: usize) {
        let events = self.selected_events();
        let sender_labels = self.sender_labels(events.as_slice());
        let reactions = self.selected_reactions();
        let ranges = message_line_ranges(
            events.as_slice(),
            sender_labels.as_slice(),
            self.messages.width,
            &reactions,
            &self.colors,
        );
        let Some(range) = ranges.get(index) else {
            return;
        };
        let page_size = self.messages.page_size.max(1);
        let total_lines = ranges.last().map(|range| range.end).unwrap_or_default();
        let max_scroll = total_lines.saturating_sub(page_size);
        let mut scroll = self.messages.scroll.min(max_scroll);
        if range.start < scroll || range.end > scroll.saturating_add(page_size) {
            scroll = range.start;
        }
        self.messages.scroll = scroll.min(max_scroll);
    }

    pub(crate) async fn commit_room_search(&mut self, query: String) {
        if query.is_empty() {
            return;
        }
        let query_lower = query.to_ascii_lowercase();
        let all_matches: Vec<usize> = self
            .visible_room_indices()
            .into_iter()
            .filter(|&i| room_matches_search(&self.rooms.rooms[i], &query_lower))
            .collect();
        let found = all_matches.first().copied();
        self.last_search = Some(query);
        match found {
            Some(index) => {
                self.rooms.selected = Some(index);
                self.load_selected_timeline().await;
                self.status = match_status(1, all_matches.len());
            }
            None => self.status = Status::Info("no match".to_owned()),
        }
    }

    pub(crate) async fn search_adjacent_room(&mut self, query: &str, forward: bool) {
        let query = query.to_ascii_lowercase();
        let all_matches: Vec<usize> = self
            .visible_room_indices()
            .into_iter()
            .filter(|&i| room_matches_search(&self.rooms.rooms[i], &query))
            .collect();
        if all_matches.is_empty() {
            self.status = Status::Info("no more matches".to_owned());
            return;
        }
        let found = next_match_index(
            &all_matches,
            self.rooms.selected,
            forward,
            self.display.search_wrap,
        );
        match found {
            Some(index) => {
                self.rooms.selected = Some(index);
                self.load_selected_timeline().await;
                let match_num = all_matches.iter().position(|&i| i == index).unwrap_or(0) + 1;
                self.status = match_status(match_num, all_matches.len());
            }
            None => self.status = Status::Info("no more matches".to_owned()),
        }
    }

    pub(crate) fn commit_message_search(&mut self, query: String) {
        if query.is_empty() {
            return;
        }
        let query_lower = query.to_ascii_lowercase();
        let current_id = self.messages.selection.clone();
        let (found, total_matches) = {
            let events = self.selected_events();
            let all_matches: Vec<(usize, String)> = events
                .iter()
                .enumerate()
                .filter(|(_, event)| message_matches_search(event, &query_lower))
                .map(|(i, event)| (i, event.event_id.clone()))
                .collect();
            let total = all_matches.len();
            let cursor_pos = current_id
                .as_deref()
                .and_then(|id| events.iter().position(|e| e.event_id == id));
            let found = if let Some(pos) = cursor_pos {
                all_matches
                    .iter()
                    .find(|(i, _)| *i > pos)
                    .or_else(|| all_matches.first())
                    .cloned()
            } else {
                all_matches.first().cloned()
            };
            let match_num = found
                .as_ref()
                .and_then(|(i, _)| all_matches.iter().position(|(j, _)| j == i))
                .map(|p| p + 1)
                .unwrap_or(1);
            (found.map(|(i, id)| (i, id, match_num)), total)
        };
        self.last_search = Some(query);
        match found {
            Some((index, event_id, match_num)) => {
                self.messages.selection = Some(event_id);
                self.ensure_message_index_visible(index);
                self.status = match_status(match_num, total_matches);
            }
            None => self.status = Status::Info("no match".to_owned()),
        }
    }

    pub(crate) fn search_adjacent_message(&mut self, query: &str, forward: bool) {
        let query = query.to_ascii_lowercase();
        let current_id = self.messages.selection.clone();
        let (found, total_matches) = {
            let events = self.selected_events();
            let current_pos = current_id
                .as_deref()
                .and_then(|id| events.iter().position(|event| event.event_id == id));
            let all_matches: Vec<(usize, String)> = events
                .iter()
                .enumerate()
                .filter(|(_, event)| message_matches_search(event, &query))
                .map(|(i, event)| (i, event.event_id.clone()))
                .collect();
            let total = all_matches.len();
            let found = if forward {
                let start = current_pos.map(|i| i + 1).unwrap_or(0);
                let direct = all_matches.iter().find(|(i, _)| *i >= start).cloned();
                if direct.is_some() || !self.display.search_wrap {
                    direct
                } else {
                    all_matches.first().cloned()
                }
            } else {
                let end = current_pos.unwrap_or(events.len());
                let direct = all_matches.iter().rev().find(|(i, _)| *i < end).cloned();
                if direct.is_some() || !self.display.search_wrap {
                    direct
                } else {
                    all_matches.last().cloned()
                }
            };
            let match_num = found
                .as_ref()
                .and_then(|(i, _)| all_matches.iter().position(|(j, _)| j == i))
                .map(|p| p + 1);
            (found.map(|(i, id)| (i, id, match_num.unwrap_or(1))), total)
        };
        match found {
            Some((index, event_id, match_num)) => {
                self.messages.selection = Some(event_id);
                self.ensure_message_index_visible(index);
                self.status = match_status(match_num, total_matches);
            }
            None => self.status = Status::Info("no more matches".to_owned()),
        }
    }

    fn selected_display_line_count(&self) -> usize {
        let events = self.selected_events();
        let sender_labels = self.sender_labels(events.as_slice());
        let reactions = self.selected_reactions();
        message_line_ranges(
            events.as_slice(),
            sender_labels.as_slice(),
            self.messages.width,
            &reactions,
            &self.colors,
        )
        .last()
        .map(|range| range.end)
        .unwrap_or_default()
    }

    pub(crate) fn sender_labels(&self, events: &[&EventDto]) -> Vec<String> {
        events
            .iter()
            .map(|event| self.sender_label(event))
            .collect()
    }

    pub(crate) fn set_message_viewport(&mut self, page_size: usize, width: usize) {
        self.messages.page_size = page_size.max(1);
        self.messages.width = width.max(1);
        let line_count = self.selected_display_line_count();
        let max_scroll = line_count.saturating_sub(self.messages.page_size);
        self.messages.scroll = if self.messages.scroll == usize::MAX {
            max_scroll
        } else {
            self.messages.scroll.min(max_scroll)
        };
    }
}

pub(crate) fn should_show_event(event: &EventDto, display: &DisplayOptions) -> bool {
    if event.event_type == "m.reaction" {
        return false;
    }
    display.show_state_events || event.is_message_event() || event.is_membership_event()
}

fn room_matches_search(room: &RoomDto, query: &str) -> bool {
    [
        Some(room.room_id.as_str()),
        room.canonical_alias.as_deref(),
        room.name.as_deref(),
        room.topic.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_ascii_lowercase().contains(query))
}

fn message_matches_search(event: &EventDto, query: &str) -> bool {
    if event.redacted {
        return false;
    }
    event
        .body
        .as_deref()
        .is_some_and(|body| body.to_ascii_lowercase().contains(query))
}
