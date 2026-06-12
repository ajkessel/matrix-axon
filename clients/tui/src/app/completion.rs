use crate::api::RoomDto;
use crate::command::{SlashCommand, SLASH_COMMANDS};

use super::{cycle_index, emoji_matches, App, RoomTargetResolution, Status};

impl App {
    pub(crate) fn complete_input(&mut self) {
        self.complete_input_in_direction(false);
    }

    pub(crate) fn complete_input_reverse(&mut self) {
        self.complete_input_in_direction(true);
    }

    fn complete_input_in_direction(&mut self, reverse: bool) {
        if self.complete_react_command_input(reverse) {
            return;
        }
        if self.complete_logout_command_input(reverse) {
            return;
        }
        if self.complete_command_input() {
            return;
        }
        self.complete_switch_input();
    }

    pub(crate) fn complete_logout_command_input(&mut self, reverse: bool) -> bool {
        let Some(target) = logout_target_prefix(&self.input.buffer) else {
            return false;
        };
        let query = self
            .input
            .logout_command_completion
            .as_ref()
            .map(|(query, _)| query.clone())
            .unwrap_or_else(|| target.to_owned());
        let candidates = self.active_logout_candidates(&query);
        if candidates.is_empty() {
            self.input.logout_command_completion = None;
            self.status = Status::Info(if query.is_empty() {
                "no active accounts".to_owned()
            } else {
                format!("no active account matches: {query}")
            });
            return true;
        }

        let selected = if let Some((_, current)) = self.input.logout_command_completion.as_ref() {
            cycle_index(*current, candidates.len(), reverse)
        } else if reverse {
            candidates.len() - 1
        } else {
            0
        };
        let user_id = &candidates[selected];
        self.input.buffer = format!("/logout {user_id}");
        self.move_cursor_to_end();
        self.input.logout_command_completion = Some((query, selected));
        self.status = Status::Info(format!(
            "[{}/{}] {} - Tab/Shift-Tab to cycle, Enter to log out",
            selected + 1,
            candidates.len(),
            user_id
        ));
        true
    }

    pub(crate) fn complete_react_command_input(&mut self, reverse: bool) -> bool {
        let (query, next) =
            if let Some((query, current)) = self.input.react_command_completion.as_ref() {
                let matches = emoji_matches(query);
                let next = cycle_index(*current, matches.len(), reverse);
                (query.clone(), next)
            } else {
                let Some(query) = react_command_emoji_prefix(&self.input.buffer) else {
                    return false;
                };
                let matches = emoji_matches(query);
                let next = if reverse {
                    matches.len().saturating_sub(1)
                } else {
                    0
                };
                (query.to_owned(), next)
            };
        let matches = emoji_matches(&query);
        if matches.is_empty() {
            self.input.react_command_completion = None;
            self.status = Status::Info(format!("no emoji matches '{query}'"));
            return true;
        }
        let selected = next % matches.len();
        let emoji = matches[selected];
        self.input.buffer = format!("/react {}", emoji.as_str());
        self.move_cursor_to_end();
        self.input.react_command_completion = Some((query, selected));
        self.status = Status::Info(emoji_cycle_status(selected, &matches));
        true
    }

    pub(crate) fn complete_command_input(&mut self) -> bool {
        let Some(prefix) = slash_command_prefix(&self.input.buffer) else {
            return false;
        };
        let candidates = slash_command_candidates(prefix);
        match candidates.as_slice() {
            [] => {
                self.status = Status::from(format!("no command matches: /{prefix}"));
            }
            [command] => {
                self.input.buffer = if command.takes_argument {
                    format!("{} ", command.name)
                } else {
                    command.name.to_owned()
                };
                self.move_cursor_to_end();
                self.status = Status::from(format!("completed command: {}", command.name));
            }
            _ => {
                self.status = Status::Info(format!(
                    "command matches: {}",
                    candidates
                        .iter()
                        .map(|command| command.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        true
    }

    pub(crate) fn complete_switch_input(&mut self) {
        let Some(target) = switch_target_prefix(&self.input.buffer) else {
            return;
        };
        let target = target.to_owned();
        let candidates = self.switch_completion_candidates(&target);
        match candidates.as_slice() {
            [] => {
                self.input.partial_switch_completions = None;
                self.status = Status::Info(format!("no room matches: {target}"));
            }
            [completion] => {
                self.input.partial_switch_completions = None;
                self.input.buffer = format!("/switch {completion}");
                self.move_cursor_to_end();
                self.status = Status::from(format!("completed room: {completion}"));
            }
            _ => {
                let prefix_candidates = self.switch_prefix_candidates(&target);
                let common_prefix = longest_common_prefix(&prefix_candidates);
                let completed = common_prefix
                    .as_deref()
                    .filter(|prefix| {
                        prefix.len() > target.len()
                            && prefix
                                .get(..target.len())
                                .is_some_and(|start| start.eq_ignore_ascii_case(&target))
                    })
                    .unwrap_or(&target);
                if completed != target {
                    self.input.buffer = format!("/switch {completed}");
                    self.move_cursor_to_end();
                }
                let shown_candidates = if completed != target {
                    &prefix_candidates
                } else {
                    &candidates
                };
                let displayed: Vec<_> = shown_candidates
                    .iter()
                    .take(5)
                    .map(|candidate| {
                        case_insensitive_suffix(candidate, completed)
                            .filter(|suffix| !suffix.is_empty())
                            .unwrap_or(candidate)
                            .to_owned()
                    })
                    .collect();
                let shown = displayed.join(", ");
                let more = candidates.len().saturating_sub(5);
                self.input.partial_switch_completions = Some(displayed);
                self.status = Status::Info(if more == 0 {
                    format!("room completions: {shown}")
                } else {
                    format!("room completions: {shown}, +{more} more")
                });
            }
        }
    }

    fn switch_completion_candidates(&self, target: &str) -> Vec<String> {
        self.rooms
            .rooms
            .iter()
            .filter(|room| room_matches_completion(room, target))
            .map(room_completion_value)
            .collect()
    }

    fn switch_prefix_candidates(&self, target: &str) -> Vec<String> {
        self.rooms
            .rooms
            .iter()
            .filter_map(|room| room_matching_prefix_value(room, target))
            .collect()
    }

    pub(super) fn resolve_room_target(&self, target: &str) -> RoomTargetResolution {
        let target = target.trim();
        if let Ok(index) = target.parse::<usize>() {
            return index
                .checked_sub(1)
                .filter(|index| *index < self.rooms.rooms.len())
                .map(RoomTargetResolution::Match)
                .unwrap_or(RoomTargetResolution::Missing);
        }
        let target_lower = target.to_lowercase();
        let exact = self.matching_room_indices(|room| {
            room.room_id == target
                || room.canonical_alias.as_deref() == Some(target)
                || room.name.as_deref().map(str::to_lowercase).as_deref()
                    == Some(target_lower.as_str())
        });
        if let Some(resolution) = self.classify_room_matches(target, exact) {
            return resolution;
        }

        if let Some(alias) = room_alias_with_hash(target) {
            let matches = self.matching_room_indices(|room| {
                room.canonical_alias.as_deref() == Some(alias.as_str())
            });
            if let Some(resolution) = self.classify_room_matches(target, matches) {
                return resolution;
            }
        }

        if let Some(target_local) = incomplete_matrix_room_name(target) {
            let local_matches = self.matching_room_indices(|room| {
                room.canonical_alias
                    .as_deref()
                    .and_then(matrix_room_local_name)
                    .is_some_and(|local| local.eq_ignore_ascii_case(target_local))
                    || matrix_room_local_name(&room.room_id)
                        .is_some_and(|local| local.eq_ignore_ascii_case(target_local))
            });
            if let Some(resolution) = self.classify_room_matches(target, local_matches) {
                return resolution;
            }
        }

        let prefix_matches =
            self.matching_room_indices(|room| room_matches_completion(room, target));
        self.classify_room_matches(target, prefix_matches)
            .unwrap_or(RoomTargetResolution::Missing)
    }

    fn matching_room_indices(&self, predicate: impl Fn(&RoomDto) -> bool) -> Vec<usize> {
        self.rooms
            .rooms
            .iter()
            .enumerate()
            .filter_map(|(index, room)| predicate(room).then_some(index))
            .collect()
    }

    fn classify_room_matches(
        &self,
        target: &str,
        indices: Vec<usize>,
    ) -> Option<RoomTargetResolution> {
        match indices.as_slice() {
            [index] => Some(RoomTargetResolution::Match(*index)),
            [_, _, ..] => Some(RoomTargetResolution::Ambiguous(
                self.room_resolution_options(target, &indices),
            )),
            [] => None,
        }
    }

    fn room_resolution_options(&self, target: &str, indices: &[usize]) -> Vec<String> {
        indices
            .iter()
            .filter_map(|index| self.rooms.rooms.get(*index))
            .map(|room| {
                let value = room_matching_prefix_value(room, target)
                    .unwrap_or_else(|| room_completion_value(room));
                case_insensitive_suffix(&value, target)
                    .filter(|suffix| !suffix.is_empty())
                    .unwrap_or(&value)
                    .to_owned()
            })
            .collect()
    }
}

pub(super) fn emoji_cycle_status(selected: usize, matches: &[&'static emojis::Emoji]) -> String {
    let emoji = matches[selected];
    format!(
        "[{}/{}] {} {} - Tab/Shift-Tab to cycle, Enter to send",
        selected + 1,
        matches.len(),
        emoji.as_str(),
        emoji.name()
    )
}

fn slash_command_prefix(input: &str) -> Option<&str> {
    let input = input.trim_start();
    let without_slash = input.strip_prefix('/')?;
    (!without_slash.chars().any(char::is_whitespace)).then_some(without_slash)
}

fn react_command_emoji_prefix(input: &str) -> Option<&str> {
    let input = input.trim_start();
    let query = input.strip_prefix("/react ")?.trim();
    (!query.is_empty() && !query.chars().any(char::is_whitespace)).then_some(query)
}

fn logout_target_prefix(input: &str) -> Option<&str> {
    let rest = input.strip_prefix("/logout")?;
    if rest.is_empty() {
        return Some("");
    }
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| rest.trim_start())
}

fn slash_command_candidates(prefix: &str) -> Vec<SlashCommand> {
    let command_prefix = format!("/{prefix}");
    SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.name.starts_with(&command_prefix))
        .collect()
}

fn switch_target_prefix(input: &str) -> Option<&str> {
    let rest = input.strip_prefix("/switch")?;
    if rest.is_empty() {
        return Some("");
    }
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| rest.trim_start())
}

fn longest_common_prefix(candidates: &[String]) -> Option<String> {
    let first = candidates.first()?;
    let mut prefix_end = first.len();
    for candidate in &candidates[1..] {
        prefix_end = first[..prefix_end]
            .char_indices()
            .map(|(index, ch)| (index + ch.len_utf8(), &first[..index + ch.len_utf8()]))
            .take_while(|(_, prefix)| {
                candidate
                    .get(..prefix.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            })
            .map(|(end, _)| end)
            .last()
            .unwrap_or(0);
        if prefix_end == 0 {
            break;
        }
    }
    Some(first[..prefix_end].to_owned())
}

fn room_completion_value(room: &RoomDto) -> String {
    room.canonical_alias
        .as_deref()
        .or(room.name.as_deref())
        .unwrap_or(&room.room_id)
        .to_owned()
}

fn room_matching_prefix_value(room: &RoomDto, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return Some(room_completion_value(room));
    }
    let target_lower = target.to_lowercase();
    for field in [
        room.name.as_deref(),
        room.canonical_alias.as_deref(),
        Some(room.room_id.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if field.to_lowercase().starts_with(&target_lower) {
            return Some(field.to_owned());
        }
    }
    if let Some(alias) = room_alias_with_hash(target) {
        if let Some(field) = room
            .canonical_alias
            .as_deref()
            .filter(|field| field.to_lowercase().starts_with(&alias.to_lowercase()))
        {
            return Some(field.to_owned());
        }
    }
    let target_local = incomplete_matrix_room_name(target)?;
    room.canonical_alias
        .as_deref()
        .and_then(matrix_room_local_name)
        .filter(|local| {
            local
                .to_lowercase()
                .starts_with(&target_local.to_lowercase())
        })
        .or_else(|| {
            matrix_room_local_name(&room.room_id).filter(|local| {
                local
                    .to_lowercase()
                    .starts_with(&target_local.to_lowercase())
            })
        })
        .map(str::to_owned)
}

fn case_insensitive_suffix<'a>(candidate: &'a str, prefix: &str) -> Option<&'a str> {
    candidate
        .get(..prefix.len())
        .filter(|start| start.eq_ignore_ascii_case(prefix))?;
    candidate.get(prefix.len()..)
}

fn room_matches_completion(room: &RoomDto, target: &str) -> bool {
    room_matching_prefix_value(room, target).is_some()
}

fn incomplete_matrix_room_name(target: &str) -> Option<&str> {
    let target = target.trim();
    if target.is_empty() || target.contains(':') {
        return None;
    }
    Some(target.trim_start_matches(['#', '!'])).filter(|target| !target.is_empty())
}

fn room_alias_with_hash(target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() || target.starts_with(['#', '!']) || !target.contains(':') {
        return None;
    }
    Some(format!("#{target}"))
}

fn matrix_room_local_name(value: &str) -> Option<&str> {
    let value = value.strip_prefix(['#', '!'])?;
    value.split_once(':').map(|(local, _server)| local)
}
