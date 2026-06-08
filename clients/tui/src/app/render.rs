use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, UNIX_EPOCH};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use uuid::Uuid;

use crate::api::EventDto;
use crate::config::ColorScheme;
use crate::html::formatted_message_body_lines;
use crate::wrap::{plain_rich_lines, rich_lines_to_spans, wrap_rich_lines};

pub(crate) fn format_time(origin_ts: i64) -> String {
    let Ok(millis) = u64::try_from(origin_ts) else {
        return "--:--:--".to_owned();
    };
    let Some(time) = UNIX_EPOCH.checked_add(Duration::from_millis(millis)) else {
        return "--:--:--".to_owned();
    };
    let Ok(since_midnight) = time.duration_since(UNIX_EPOCH) else {
        return "--:--:--".to_owned();
    };
    let seconds = since_midnight.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

pub(crate) fn message_line_ranges(
    events: &[&EventDto],
    sender_labels: &[String],
    width: usize,
    reactions: &HashMap<String, Vec<(String, usize)>>,
    colors: &ColorScheme,
) -> Vec<Range<usize>> {
    let empty = vec![];
    let mut start = 0;
    events
        .iter()
        .zip(sender_labels)
        .map(|(event, sender_label)| {
            let event_reactions = reactions
                .get(&event.event_id)
                .map(Vec::as_slice)
                .unwrap_or(&empty);
            let count =
                message_display_line_count(event, sender_label, width, event_reactions, colors);
            let range = start..start + count;
            start += count;
            range
        })
        .collect()
}

pub(crate) fn message_index_at_line(ranges: &[Range<usize>], line: usize) -> usize {
    ranges
        .iter()
        .position(|range| line < range.end)
        .unwrap_or_else(|| ranges.len().saturating_sub(1))
}

fn message_display_line_count(
    event: &EventDto,
    sender_label: &str,
    width: usize,
    event_reactions: &[(String, usize)],
    colors: &ColorScheme,
) -> usize {
    let body_lines = message_body_lines(
        event,
        sender_label,
        first_body_width(sender_label, event.origin_ts, width),
        continuation_body_width(width),
        colors,
    )
    .len()
    .max(1);
    body_lines + usize::from(!event_reactions.is_empty())
}

pub(crate) fn message_display_lines(
    events: &[&EventDto],
    sender_labels: &[String],
    selected_message: Option<&str>,
    colors: &ColorScheme,
    width: usize,
    reactions: &HashMap<String, Vec<(String, usize)>>,
    own_senders: &HashMap<Uuid, String>,
) -> Vec<Line<'static>> {
    let reaction_style = Style::default().fg(colors.input_hint);
    events
        .iter()
        .zip(sender_labels)
        .flat_map(|(event, sender_label)| {
            let is_selected = selected_message == Some(event.event_id.as_str());
            let is_own = own_senders.get(&event.account_id) == Some(&event.sender);
            let marker = if is_selected { "> " } else { "  " };
            let time_style = if is_selected {
                Style::default().fg(colors.selected_room)
            } else {
                Style::default()
            };
            let sender_color = if is_own {
                colors.own_message_sender
            } else {
                colors.message_sender
            };
            let body_lines = message_body_lines(
                event,
                sender_label,
                first_body_width(sender_label, event.origin_ts, width),
                continuation_body_width(width),
                colors,
            );
            let event_reactions = reactions.get(&event.event_id).cloned().unwrap_or_default();
            let reaction_line = if event_reactions.is_empty() {
                None
            } else {
                let text = event_reactions
                    .iter()
                    .map(|(key, count)| format!("{key} {count}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                Some(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(text, reaction_style),
                ]))
            };
            let body_iter = body_lines
                .into_iter()
                .enumerate()
                .map(move |(index, body)| {
                    if index == 0 {
                        let mut spans = vec![
                            Span::styled(marker, time_style),
                            Span::styled(format!("{} ", format_time(event.origin_ts)), time_style),
                            Span::styled(
                                format!("{sender_label}: "),
                                Style::default()
                                    .fg(sender_color)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ];
                        spans.extend(body);
                        Line::from(spans)
                    } else {
                        let mut spans = vec![Span::raw("  ")];
                        spans.extend(body);
                        Line::from(spans)
                    }
                });
            body_iter.chain(reaction_line)
        })
        .collect()
}

fn first_body_width(sender_label: &str, origin_ts: i64, width: usize) -> usize {
    let prefix_width =
        2 + format_time(origin_ts).chars().count() + 1 + sender_label.chars().count() + 2;
    width.saturating_sub(prefix_width).max(1)
}

pub(crate) fn display_body_with_sender(event: &EventDto, sender_label: &str) -> String {
    if event.redacted {
        return "[redacted]".to_owned();
    }
    if let Some(membership) = event.membership_change() {
        return match membership.as_str() {
            "join" => format!("{sender_label} joined the room"),
            "leave" => format!("{sender_label} left the room"),
            "ban" => format!("{sender_label} was banned from the room"),
            "invite" => format!("{sender_label} was invited to the room"),
            _ => format!("{sender_label} membership changed: {membership}"),
        };
    }
    event.display_body()
}

fn message_body_lines(
    event: &EventDto,
    sender_label: &str,
    first_width: usize,
    continuation_width: usize,
    colors: &ColorScheme,
) -> Vec<Vec<Span<'static>>> {
    if !event.redacted && event.membership_change().is_none() {
        if let Some(lines) = event.formatted_body().and_then(|html| {
            formatted_message_body_lines(html, first_width, continuation_width, colors)
        }) {
            return lines;
        }
    }

    rich_lines_to_spans(wrap_rich_lines(
        plain_rich_lines(&display_body_with_sender(event, sender_label)),
        first_width,
        continuation_width,
    ))
}

fn continuation_body_width(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}
