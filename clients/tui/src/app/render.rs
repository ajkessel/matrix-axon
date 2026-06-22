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

/// Map from `(account_id, mxc_url)` to the rows allocated for its thumbnail.
/// Entries are only needed when a cached image is naturally shorter than
/// `IMAGE_THUMB_ROWS`; absent entries default to `IMAGE_THUMB_ROWS`.
pub(crate) type ImageThumbRows = HashMap<(Uuid, String), usize>;

pub(crate) struct MessageLayout {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) ranges: Vec<Range<usize>>,
    pub(crate) image_body_rows: HashMap<(Uuid, String), usize>,
}

/// Rows reserved in the message list for image/sticker events so the inline
/// thumbnail has enough vertical space to be legible.
pub(crate) const IMAGE_THUMB_ROWS: usize = 6;

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

pub(crate) fn message_index_at_line(ranges: &[Range<usize>], line: usize) -> usize {
    ranges
        .iter()
        .position(|range| line < range.end)
        .unwrap_or_else(|| ranges.len().saturating_sub(1))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn message_layout(
    events: &[&EventDto],
    sender_labels: &[String],
    selected_message: Option<&str>,
    colors: &ColorScheme,
    width: usize,
    reactions: &HashMap<String, Vec<(String, usize)>>,
    own_senders: &HashMap<Uuid, String>,
    image_thumb_rows: &ImageThumbRows,
) -> MessageLayout {
    let reaction_style = Style::default().fg(colors.input_hint);
    let mut lines = Vec::new();
    let mut ranges = Vec::with_capacity(events.len());
    let mut image_body_rows = HashMap::new();

    for (event, sender_label) in events.iter().zip(sender_labels) {
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
        let mut body_lines = message_body_lines(
            event,
            sender_label,
            first_body_width(sender_label, event.origin_ts, width),
            continuation_body_width(width),
            colors,
        );
        let body_row_count = body_lines.len().max(1);
        if let Some((account_id, mxc_url)) = event.image_mxc() {
            let key = (account_id, mxc_url);
            image_body_rows.insert(key.clone(), body_row_count);
            let thumbnail_rows = image_thumb_rows
                .get(&key)
                .copied()
                .unwrap_or(IMAGE_THUMB_ROWS);
            body_lines.resize_with(body_row_count + thumbnail_rows, Vec::new);
        }

        let range_start = lines.len();
        for (index, body) in body_lines.into_iter().enumerate() {
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
                lines.push(Line::from(spans));
            } else {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(body);
                lines.push(Line::from(spans));
            }
        }

        if let Some(event_reactions) = reactions.get(&event.event_id) {
            if !event_reactions.is_empty() {
                let text = event_reactions
                    .iter()
                    .map(|(key, count)| format!("{key} {count}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(text, reaction_style),
                ]));
            }
        }
        ranges.push(range_start..lines.len());
    }

    MessageLayout {
        lines,
        ranges,
        image_body_rows,
    }
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
