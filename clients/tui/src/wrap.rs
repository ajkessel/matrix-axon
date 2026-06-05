use ratatui::style::Style;
use ratatui::text::Span;

#[derive(Clone, Debug)]
pub(crate) struct RichSpan {
    text: String,
    style: Style,
}

impl RichSpan {
    pub(crate) fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}
pub(crate) fn plain_rich_lines(text: &str) -> Vec<Vec<RichSpan>> {
    let mut lines = text
        .split('\n')
        .map(|line| vec![RichSpan::new(line, Style::default())])
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}
pub(crate) fn push_rich_span(lines: &mut [Vec<RichSpan>], span: RichSpan) {
    if span.text.is_empty() {
        return;
    }
    let line = lines
        .last_mut()
        .expect("rich lines always has a current line");
    if let Some(last) = line.last_mut() {
        if last.style == span.style {
            last.text.push_str(&span.text);
            return;
        }
    }
    line.push(span);
}

pub(crate) fn push_rich_line_break(lines: &mut Vec<Vec<RichSpan>>) {
    lines.push(Vec::new());
}

pub(crate) fn push_rich_line_break_if_needed(lines: &mut Vec<Vec<RichSpan>>) {
    if !rich_current_line_is_empty(lines) {
        push_rich_line_break(lines);
    }
}

pub(crate) fn rich_current_line_is_empty(lines: &[Vec<RichSpan>]) -> bool {
    lines
        .last()
        .is_none_or(|line| line.iter().all(|span| span.text.is_empty()))
}

pub(crate) fn trim_empty_rich_edges(lines: &mut Vec<Vec<RichSpan>>) {
    while lines.len() > 1 && lines.first().is_some_and(Vec::is_empty) {
        lines.remove(0);
    }
    while lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
}

pub(crate) fn rich_lines_are_empty(lines: &[Vec<RichSpan>]) -> bool {
    lines
        .iter()
        .all(|line| line.iter().all(|span| span.text.trim().is_empty()))
}

pub(crate) fn wrap_rich_lines(
    lines: Vec<Vec<RichSpan>>,
    first_width: usize,
    continuation_width: usize,
) -> Vec<Vec<RichSpan>> {
    let mut width = first_width.max(1);
    let continuation_width = continuation_width.max(1);
    let mut wrapped = Vec::new();
    for raw_line in lines {
        let mut current = Vec::new();
        let mut remaining = width;
        for span in raw_line {
            for ch in span.text.chars() {
                if remaining == 0 {
                    wrapped.push(current);
                    current = Vec::new();
                    width = continuation_width;
                    remaining = width;
                }
                push_wrapped_char(&mut current, ch, span.style);
                remaining = remaining.saturating_sub(1);
            }
        }
        wrapped.push(current);
        width = continuation_width;
    }
    if wrapped.is_empty() {
        wrapped.push(Vec::new());
    }
    wrapped
}

fn push_wrapped_char(line: &mut Vec<RichSpan>, ch: char, style: Style) {
    if let Some(last) = line.last_mut() {
        if last.style == style {
            last.text.push(ch);
            return;
        }
    }
    line.push(RichSpan::new(ch.to_string(), style));
}

pub(crate) fn append_rich_lines(lines: &mut Vec<Vec<RichSpan>>, mut extra: Vec<Vec<RichSpan>>) {
    if extra.is_empty() {
        return;
    }

    let first = extra.remove(0);
    for span in first {
        push_rich_span(lines, span);
    }
    lines.extend(extra);
}

pub(crate) fn rich_lines_text(lines: &[Vec<RichSpan>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|span| span.text.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn rich_lines_to_spans(lines: Vec<Vec<RichSpan>>) -> Vec<Vec<Span<'static>>> {
    lines
        .into_iter()
        .map(|line| {
            line.into_iter()
                .map(|span| Span::styled(span.text, span.style))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_span_wrap_preserves_styles_across_lines() {
        let style = Style::default().fg(ratatui::style::Color::Yellow);
        let wrapped = wrap_rich_lines(vec![vec![RichSpan::new("abcd", style)]], 2, 2);

        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0][0].text, "ab");
        assert_eq!(wrapped[0][0].style, style);
        assert_eq!(wrapped[1][0].text, "cd");
        assert_eq!(wrapped[1][0].style, style);
    }
}
