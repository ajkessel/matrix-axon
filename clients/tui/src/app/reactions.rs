use std::collections::HashMap;

use super::completion::emoji_cycle_status;
use super::{cycle_index, App, Mode, OwnReaction, RoomKey, Status};

impl App {
    pub(crate) async fn start_unreact_from_selected_message(&mut self) {
        let Some(target_event_id) = self.selected_message_id().map(str::to_owned) else {
            self.status = Status::from("select a displayed message before unreacting".to_owned());
            return;
        };
        let choices = match self.own_reactions_for(&target_event_id) {
            Ok(choices) => choices,
            Err(message) => {
                self.status = Status::from(message);
                return;
            }
        };
        match choices.as_slice() {
            [] => {
                self.status = Status::from("you have no reactions on this message".to_owned());
            }
            [only] => {
                self.withdraw_reaction(only.clone()).await;
            }
            _ => {
                self.status = unreact_selection_status(&choices, 0);
                self.mode = Mode::Unreacting {
                    target_event_id,
                    choices,
                    selected: 0,
                };
            }
        }
    }

    pub(super) fn own_reactions_for(
        &self,
        target_event_id: &str,
    ) -> Result<Vec<OwnReaction>, String> {
        let room = self
            .selected_room()
            .ok_or_else(|| "no room selected".to_owned())?;
        let user_id = room
            .account_user_id
            .as_deref()
            .ok_or_else(|| "current user is unavailable for this room".to_owned())?;
        let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
        for event in self.selected_raw_events() {
            if event.redacted || event.sender != user_id {
                continue;
            }
            let Some((target, key)) = event.reaction_annotation() else {
                continue;
            };
            if target == target_event_id {
                grouped
                    .entry(key.to_owned())
                    .or_default()
                    .push(event.event_id.clone());
            }
        }
        let mut choices: Vec<_> = grouped
            .into_iter()
            .map(|(key, event_ids)| OwnReaction { key, event_ids })
            .collect();
        choices.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(choices)
    }

    pub(crate) async fn withdraw_reaction(&mut self, reaction: OwnReaction) {
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from("no room selected".to_owned());
            return;
        };
        let total = reaction.event_ids.len();
        for (index, event_id) in reaction.event_ids.iter().enumerate() {
            match self
                .client
                .redact_event(room.account_id, &room.room_id, event_id, None)
                .await
            {
                Ok(_) => {
                    let room_key = RoomKey::from(&room);
                    if let Some(events) = self.messages.events.get_mut(&room_key) {
                        if let Some(event) = events.iter_mut().find(|e| e.event_id == *event_id) {
                            event.redacted = true;
                        }
                    }
                }
                Err(err) => {
                    self.status = Status::Info(format!(
                        "unreact failed after {index}/{total} withdrawals: {err}"
                    ));
                    return;
                }
            }
        }
        self.status = Status::EventAction {
            debug: format!("withdrew reaction {}", reaction.key),
            redacted: "withdrew reaction",
        };
    }

    pub(crate) fn start_react_to_selected_message(&mut self) {
        let Some(event) = self.selected_message_event() else {
            self.status = Status::from("select a displayed message before reacting".to_owned());
            return;
        };
        if event.event_id.starts_with("local-echo:") {
            self.status = Status::from(super::PENDING_ECHO_MSG.to_owned());
            return;
        }
        let event_id = event.event_id.clone();
        self.input.buffer.clear();
        self.input.cursor = 0;
        self.input.react_tab = None;
        self.mode = Mode::Reacting {
            event_id: event_id.clone(),
        };
        self.status = Status::EventAction {
            debug: format!("react to {} - type emoji, Enter to send", event_id),
            redacted: "react to message - type emoji, Enter to send",
        };
    }

    pub(crate) async fn send_react(&mut self, event_id: &str, key: &str) {
        if key.is_empty() {
            self.status = Status::from("reaction key cannot be empty".to_owned());
            return;
        }
        let Some(room) = self.selected_room().cloned() else {
            self.status = Status::from("no room selected".to_owned());
            return;
        };
        self.status = match self
            .client
            .react(room.account_id, &room.room_id, event_id, key)
            .await
        {
            Ok(r) => Status::EventAction {
                debug: format!("reacted: {}", r.event_id),
                redacted: "reacted",
            },
            Err(err) => Status::Info(format!("react failed: {err}")),
        };
    }

    pub(crate) fn update_react_status(&mut self, event_id: &str) {
        if self.input.buffer.is_empty() {
            self.status = Status::EventAction {
                debug: format!("react to {event_id} - type emoji name, Enter to send"),
                redacted: "react to message - type emoji name, Enter to send",
            };
            return;
        }
        let matches = emoji_matches(&self.input.buffer);
        self.status = Status::Info(match matches.as_slice() {
            [] => format!("no emoji matches '{}'", self.input.buffer),
            [single] => format!("{} {} - Enter to send", single.as_str(), single.name()),
            _ => {
                let shown: Vec<_> = matches
                    .iter()
                    .take(5)
                    .map(|e| format!("{} {}", e.as_str(), e.name()))
                    .collect();
                let more = matches.len().saturating_sub(5);
                if more == 0 {
                    format!("matches: {} - Tab/Shift-Tab to select", shown.join(", "))
                } else {
                    format!(
                        "matches: {}, +{more} more - Tab/Shift-Tab to select",
                        shown.join(", ")
                    )
                }
            }
        });
    }

    pub(crate) fn complete_react_input(&mut self, reverse: bool) {
        let matches = emoji_matches(&self.input.buffer);
        if matches.len() < 2 {
            return;
        }
        let next = self
            .input
            .react_tab
            .map(|index| cycle_index(index, matches.len(), reverse))
            .unwrap_or_else(|| if reverse { matches.len() - 1 } else { 0 });
        self.input.react_tab = Some(next);
        self.status = Status::Info(emoji_cycle_status(next, &matches));
    }
}

pub(crate) fn unreact_selection_status(choices: &[OwnReaction], selected: usize) -> Status {
    let choice = &choices[selected];
    Status::Info(format!(
        "[{}/{}] withdraw {} - Tab/Shift-Tab to cycle, Enter to confirm",
        selected + 1,
        choices.len(),
        choice.key
    ))
}

pub(crate) fn emoji_matches(query: &str) -> Vec<&'static emojis::Emoji> {
    if query.is_empty() {
        return vec![];
    }
    let q = query.to_ascii_lowercase();
    emojis::iter()
        .filter(|emoji| emoji.name().to_ascii_lowercase().contains(q.as_str()))
        .collect()
}

pub(crate) fn collect_reactions(
    events: &[crate::api::EventDto],
) -> HashMap<String, Vec<(String, usize)>> {
    let mut map: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for event in events {
        if event.redacted {
            continue;
        }
        if let Some((target, key)) = event.reaction_annotation() {
            *map.entry(target.to_owned())
                .or_default()
                .entry(key.to_owned())
                .or_default() += 1;
        }
    }
    map.into_iter()
        .map(|(event_id, counts)| {
            let mut pairs: Vec<(String, usize)> = counts.into_iter().collect();
            pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            (event_id, pairs)
        })
        .collect()
}
