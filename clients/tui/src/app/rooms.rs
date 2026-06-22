use uuid::Uuid;

use crate::api::{EventDto, RoomDto};

use super::{
    display_body_with_sender, format_time, match_status, next_match_index, relative_room_index,
    AccountSelection, App, RoomKey, RoomTargetResolution, Status, TIMELINE_LIMIT,
};

impl App {
    pub(crate) async fn refresh_rooms(&mut self) {
        match self.client.list_rooms(self.account_filter).await {
            Ok(rooms) => self.apply_room_refresh(rooms),
            Err(err) => {
                if !self.is_mid_command() {
                    self.status = Status::from(format!("room refresh failed: {err}"));
                }
            }
        }
    }

    pub(crate) fn apply_room_refresh(&mut self, mut rooms: Vec<RoomDto>) {
        // A logged-out (deactivated) account keeps its rows in Axon's `events`
        // table, and `GET /v1/rooms` joins accounts without a state filter, so it
        // still lists that account's rooms. Drop rooms for any account we know is
        // not active so a logout actually clears them. Rooms for accounts we don't
        // know about are kept, so a stale or failed account fetch never blanks the
        // whole list.
        rooms.retain(|room| !self.is_known_inactive_account(room.account_id));
        rooms.sort_by_key(|room| std::cmp::Reverse(room.last_activity_ts));
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
            if !self.is_mid_command() {
                self.status = Status::from("no rooms returned by Axon".to_owned());
            }
        } else if self
            .rooms
            .selected
            .is_none_or(|selected| !self.visible_room_indices().contains(&selected))
        {
            let visible = self.visible_room_indices();
            self.rooms.selected = visible.first().copied();
            if !self.is_mid_command() {
                self.status = Status::from(format!("loaded {} rooms", self.rooms.rooms.len()));
            }
        } else if !self.is_mid_command() {
            self.status = Status::from(format!("refreshed {} rooms", self.rooms.rooms.len()));
        }
    }

    /// Whether `account_id` is an account we've listed and that is *not* active
    /// (e.g. logged out). Unknown accounts return `false` so we never hide a
    /// room just because our account list is empty or stale.
    fn is_known_inactive_account(&self, account_id: Uuid) -> bool {
        self.accounts.inactive_ids.contains(&account_id)
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
                if !self.is_mid_command() {
                    self.status = Status::Info(if has_more {
                        format!("showing {} (older history available later)", room.title())
                    } else {
                        format!("showing {}", room.title())
                    });
                }
            }
            Err(err) => {
                if !self.is_mid_command() {
                    self.status = Status::from(format!("timeline load failed: {err}"));
                }
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
        let visible = self.visible_room_indices();
        if visible.is_empty() {
            self.status = Status::from("no rooms to switch".to_owned());
            return;
        }
        let current_vis = self
            .rooms
            .selected
            .and_then(|sel| visible.iter().position(|&i| i == sel))
            .unwrap_or(0);
        let next_vis = relative_room_index(current_vis, visible.len(), offset);
        self.rooms.selected = Some(visible[next_vis]);
        self.load_selected_timeline().await;
    }

    pub(crate) fn sync_room_selection_to_account_filter(&mut self) {
        let visible = self.visible_room_indices();
        let current_ok = self
            .rooms
            .selected
            .is_some_and(|sel| visible.contains(&sel));
        if !current_ok {
            self.rooms.selected = visible.first().copied();
            self.messages.selection = None;
            self.messages.scroll = usize::MAX;
        }
    }

    pub(crate) fn cycle_account(&mut self, offset: isize) {
        let n = self.accounts.accounts.len();
        if n == 0 {
            return;
        }
        let total = n + 1;
        let current = match self.accounts.selected {
            AccountSelection::All => 0,
            AccountSelection::Account(i) => i + 1,
        };
        let next = ((current as isize + offset).rem_euclid(total as isize)) as usize;
        self.accounts.selected = if next == 0 {
            AccountSelection::All
        } else {
            AccountSelection::Account(next - 1)
        };
        self.sync_room_selection_to_account_filter();
    }

    pub(crate) fn search_adjacent_account(&mut self, query: &str, forward: bool) {
        let q = query.to_lowercase();
        let all_matches = self.account_search_matches(&q);
        if all_matches.is_empty() {
            self.status = Status::from("no more matches".to_owned());
            return;
        }
        let current_pos = match self.accounts.selected {
            AccountSelection::All => 0,
            AccountSelection::Account(i) => i + 1,
        };
        let found = next_match_index(
            &all_matches,
            Some(current_pos),
            forward,
            self.display.search_wrap,
        );
        if let Some(pos) = found {
            self.accounts.selected = if pos == 0 {
                AccountSelection::All
            } else {
                AccountSelection::Account(pos - 1)
            };
            self.sync_room_selection_to_account_filter();
            self.last_search = Some(query.to_owned());
            let match_num = all_matches.iter().position(|&p| p == pos).unwrap_or(0) + 1;
            self.status = match_status(match_num, all_matches.len());
        }
    }

    pub(crate) fn commit_account_search(&mut self, query: String) -> bool {
        let query_lower = query.to_lowercase();
        let all_matches = self.account_search_matches(&query_lower);
        self.last_search = Some(query.clone());
        let Some(&pos) = all_matches.first() else {
            self.status = Status::from(format!("no account matches: {query}"));
            return false;
        };
        self.accounts.selected = if pos == 0 {
            AccountSelection::All
        } else {
            AccountSelection::Account(pos - 1)
        };
        self.sync_room_selection_to_account_filter();
        self.status = match_status(1, all_matches.len());
        true
    }

    fn account_search_matches(&self, query_lower: &str) -> Vec<usize> {
        std::iter::once(AccountSelection::All.display_label(None))
            .chain(
                self.accounts
                    .accounts
                    .iter()
                    .enumerate()
                    .map(|(i, a)| AccountSelection::Account(i).display_label(Some(&a.user_id))),
            )
            .enumerate()
            .filter(|(_, label)| label.to_lowercase().contains(query_lower))
            .map(|(pos, _)| pos)
            .collect()
    }

    pub(super) fn switch_account(&mut self, target: &str) -> bool {
        let target = target.trim();

        if target.eq_ignore_ascii_case("all") || target == "0" {
            self.accounts.selected = AccountSelection::All;
            self.sync_room_selection_to_account_filter();
            self.status = Status::from("showing all accounts".to_owned());
            return true;
        }

        if let Ok(n) = target.parse::<usize>() {
            return match n
                .checked_sub(1)
                .filter(|&i| i < self.accounts.accounts.len())
            {
                Some(idx) => {
                    let user_id = self.accounts.accounts[idx].user_id.clone();
                    self.accounts.selected = AccountSelection::Account(idx);
                    self.sync_room_selection_to_account_filter();
                    self.status = Status::from(format!("account: {user_id}"));
                    true
                }
                None => {
                    self.status = Status::from(format!("account index out of range: {target}"));
                    false
                }
            };
        }

        let target_lower = target.to_lowercase();
        let exact: Vec<usize> = self
            .accounts
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.user_id.to_lowercase() == target_lower)
            .map(|(i, _)| i)
            .collect();
        if let Some(idx) = single_match(exact) {
            let user_id = self.accounts.accounts[idx].user_id.clone();
            self.accounts.selected = AccountSelection::Account(idx);
            self.sync_room_selection_to_account_filter();
            self.status = Status::from(format!("account: {user_id}"));
            return true;
        }

        let localpart = target.trim_start_matches('@');
        let local_matches: Vec<usize> = self
            .accounts
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| account_localpart(&a.user_id) == Some(localpart))
            .map(|(i, _)| i)
            .collect();
        if let Some(result) = resolve_account_matches(self, local_matches) {
            return result.apply(self);
        }

        let prefix_matches: Vec<usize> = self
            .accounts
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.user_id.to_lowercase().contains(&target_lower))
            .map(|(i, _)| i)
            .collect();
        if let Some(result) = resolve_account_matches(self, prefix_matches) {
            return result.apply(self);
        }

        self.status = Status::from(format!("account not found: {target}"));
        false
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

fn single_match(indices: Vec<usize>) -> Option<usize> {
    match indices.as_slice() {
        [idx] => Some(*idx),
        _ => None,
    }
}

enum AccountResolution {
    Match(usize),
    Ambiguous(Vec<String>),
}

impl AccountResolution {
    fn apply(self, app: &mut App) -> bool {
        match self {
            AccountResolution::Match(idx) => {
                let user_id = app.accounts.accounts[idx].user_id.clone();
                app.accounts.selected = AccountSelection::Account(idx);
                app.sync_room_selection_to_account_filter();
                app.status = Status::from(format!("account: {user_id}"));
                true
            }
            AccountResolution::Ambiguous(options) => {
                app.status = Status::from(format!("account is ambiguous: {}", options.join(", ")));
                false
            }
        }
    }
}

fn resolve_account_matches(app: &App, indices: Vec<usize>) -> Option<AccountResolution> {
    match indices.as_slice() {
        [] => None,
        [idx] => Some(AccountResolution::Match(*idx)),
        _ => Some(AccountResolution::Ambiguous(
            indices
                .iter()
                .map(|&i| app.accounts.accounts[i].user_id.clone())
                .collect(),
        )),
    }
}

pub(crate) fn account_localpart(user_id: &str) -> Option<&str> {
    user_id
        .strip_prefix('@')?
        .split_once(':')
        .map(|(local, _)| local)
}

fn apply_edits(events: &mut Vec<EventDto>) {
    // Collect all edit relations, tracking whether each target is present on
    // this page. An edit whose target lives on an older (not-yet-loaded) page
    // must NOT be removed: suppressing it would make the edit invisible.
    let edits: Vec<(String, String)> = events
        .iter()
        .filter_map(|event| {
            let (target, body) = event.edit_relation()?;
            Some((target.to_owned(), body.to_owned()))
        })
        .collect();
    let mut applied: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (target_id, new_body) in &edits {
        if let Some(event) = events.iter_mut().find(|event| &event.event_id == target_id) {
            event.body = Some(new_body.clone());
            applied.insert(target_id.clone());
        }
    }
    // Only collapse an edit event once its target has been updated on this page.
    events.retain(|event| {
        event
            .edit_relation()
            .is_none_or(|(target, _)| !applied.contains(target))
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uuid::Uuid;

    use crate::api::EventDto;

    use super::apply_edits;

    fn msg(event_id: &str, body: &str) -> EventDto {
        EventDto {
            account_id: Uuid::nil(),
            event_id: event_id.to_owned(),
            room_id: "!room:localhost".to_owned(),
            sender: "@alice:localhost".to_owned(),
            state_key: None,
            origin_ts: 0,
            event_type: "m.room.message".to_owned(),
            content: Some(json!({"msgtype": "m.text", "body": body})),
            body: Some(body.to_owned()),
            relates_to: None,
            redacted: false,
            redaction_event_id: None,
            reactions: None,
        }
    }

    fn edit(event_id: &str, target_id: &str, new_body: &str) -> EventDto {
        EventDto {
            account_id: Uuid::nil(),
            event_id: event_id.to_owned(),
            room_id: "!room:localhost".to_owned(),
            sender: "@alice:localhost".to_owned(),
            state_key: None,
            origin_ts: 1,
            event_type: "m.room.message".to_owned(),
            content: Some(json!({
                "msgtype": "m.text",
                "body": format!("* {new_body}"),
                "m.new_content": {"msgtype": "m.text", "body": new_body},
                "m.relates_to": {"rel_type": "m.replace", "event_id": target_id},
            })),
            body: Some(format!("* {new_body}")),
            relates_to: Some(json!({"rel_type": "m.replace", "event_id": target_id})),
            redacted: false,
            redaction_event_id: None,
            reactions: None,
        }
    }

    #[test]
    fn apply_edits_updates_original_and_removes_edit_event() {
        let mut events = vec![msg("$orig", "hello"), edit("$edit", "$orig", "hello world")];
        apply_edits(&mut events);
        assert_eq!(events.len(), 1, "edit event should be removed");
        assert_eq!(
            events[0].body.as_deref(),
            Some("hello world"),
            "original body should be replaced with new content"
        );
    }

    #[test]
    fn apply_edits_leaves_unrelated_messages_untouched() {
        let mut events = vec![
            msg("$a", "first"),
            msg("$b", "second"),
            edit("$edit", "$a", "first edited"),
        ];
        apply_edits(&mut events);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].body.as_deref(), Some("first edited"));
        assert_eq!(events[1].body.as_deref(), Some("second"));
    }

    #[test]
    fn apply_edits_keeps_edit_event_when_original_not_in_page() {
        // If the original message is on an older page that hasn't been loaded,
        // the edit event must remain visible rather than disappearing silently.
        let mut events = vec![msg("$b", "second"), edit("$edit", "$missing", "edited")];
        apply_edits(&mut events);
        // edit event kept (target not in this page); $b untouched
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_id, "$b");
        assert_eq!(events[1].event_id, "$edit");
    }

    #[test]
    fn edit_relation_returns_none_without_relates_to() {
        let event = msg("$m", "body");
        assert!(event.edit_relation().is_none());
    }

    #[test]
    fn edit_relation_returns_none_when_content_missing_new_content() {
        let event = EventDto {
            account_id: Uuid::nil(),
            event_id: "$e".to_owned(),
            room_id: "!r:localhost".to_owned(),
            sender: "@a:localhost".to_owned(),
            state_key: None,
            origin_ts: 0,
            event_type: "m.room.message".to_owned(),
            content: Some(json!({"msgtype": "m.text", "body": "* edited"})),
            body: Some("* edited".to_owned()),
            relates_to: Some(json!({"rel_type": "m.replace", "event_id": "$orig"})),
            redacted: false,
            redaction_event_id: None,
            reactions: None,
        };
        // relates_to is set but content has no m.new_content → None
        assert!(event.edit_relation().is_none());
    }
}
