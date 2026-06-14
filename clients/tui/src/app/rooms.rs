use uuid::Uuid;

use crate::api::{EventDto, RoomDto};

use super::{
    display_body_with_sender, format_time, relative_room_index, AccountSelection, App, RoomKey,
    RoomTargetResolution, Status, TIMELINE_LIMIT,
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
            self.status = Status::from("no rooms returned by Axon".to_owned());
        } else if self
            .rooms
            .selected
            .is_none_or(|selected| !self.visible_room_indices().contains(&selected))
        {
            let visible = self.visible_room_indices();
            self.rooms.selected = visible.first().copied();
            self.status = Status::from(format!("loaded {} rooms", self.rooms.rooms.len()));
        } else {
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
        let n = self.accounts.accounts.len();
        if n == 0 {
            return;
        }
        let total = n + 1;
        let current_pos = match self.accounts.selected {
            AccountSelection::All => 0,
            AccountSelection::Account(i) => i + 1,
        };
        let step: isize = if forward { 1 } else { -1 };
        let q = query.to_lowercase();
        for delta in 1..=total {
            let pos = ((current_pos as isize + step * delta as isize).rem_euclid(total as isize))
                as usize;
            let label = if pos == 0 {
                AccountSelection::All.display_label(None)
            } else {
                AccountSelection::Account(pos - 1)
                    .display_label(Some(&self.accounts.accounts[pos - 1].user_id))
            };
            if label.to_lowercase().contains(&q) {
                self.accounts.selected = if pos == 0 {
                    AccountSelection::All
                } else {
                    AccountSelection::Account(pos - 1)
                };
                self.sync_room_selection_to_account_filter();
                self.last_search = Some(query.to_owned());
                return;
            }
        }
    }

    pub(crate) fn commit_account_search(&mut self, query: String) -> bool {
        let query_lower = query.to_lowercase();
        let selection = std::iter::once((
            AccountSelection::All.display_label(None),
            AccountSelection::All,
        ))
        .chain(
            self.accounts
                .accounts
                .iter()
                .enumerate()
                .map(|(index, account)| {
                    let selection = AccountSelection::Account(index);
                    (selection.display_label(Some(&account.user_id)), selection)
                }),
        )
        .find(|(label, _)| label.to_lowercase().contains(&query_lower))
        .map(|(_, selection)| selection);

        self.last_search = Some(query.clone());
        let Some(selection) = selection else {
            self.status = Status::from(format!("no account matches: {query}"));
            return false;
        };
        self.accounts.selected = selection;
        self.sync_room_selection_to_account_filter();
        true
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
