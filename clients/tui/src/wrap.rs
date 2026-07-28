use ratatui::style::Style;
use ratatui::text::Span;
use textwrap::{Options, WordSplitter};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug)]
pub(crate) struct RichSpan {
    pub(crate) text: String,
    pub(crate) style: Style,
    /// When set, this span is a leading "gutter" (e.g. a blockquote bar) that
    /// `wrap_rich_lines` holds aside and re-emits on every wrapped row, wrapping
    /// the body into the width that remains after it.
    pub(crate) repeat_on_wrap: bool,
}

impl RichSpan {
    pub(crate) fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            repeat_on_wrap: false,
        }
    }

    /// A leading gutter span re-emitted on each wrapped continuation row.
    pub(crate) fn gutter(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            repeat_on_wrap: true,
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
        if last.style == span.style && last.repeat_on_wrap == span.repeat_on_wrap {
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

/// Wrap `lines` to `first_width` (first output line) then `continuation_width`
/// (all subsequent lines), breaking at word boundaries where possible and
/// falling back to character-level breaking only when a single word exceeds
/// the available width.
pub(crate) fn wrap_rich_lines(
    lines: Vec<Vec<RichSpan>>,
    first_width: usize,
    continuation_width: usize,
) -> Vec<Vec<RichSpan>> {
    let first_width = first_width.max(1);
    let continuation_width = continuation_width.max(1);
    let mut wrapped: Vec<Vec<RichSpan>> = Vec::new();

    for raw_line in lines {
        if raw_line.is_empty() || raw_line.iter().all(|s| s.text.is_empty()) {
            wrapped.push(Vec::new());
            continue;
        }

        // Hold aside a leading gutter (repeat_on_wrap spans) and wrap the body
        // into the width that remains after it; the gutter is re-emitted on every
        // wrapped row below so multi-row blockquotes keep their bar throughout.
        let split = raw_line
            .iter()
            .position(|s| !s.repeat_on_wrap)
            .unwrap_or(raw_line.len());
        let prefix: Vec<RichSpan> = raw_line[..split].to_vec();
        let body = &raw_line[split..];
        let prefix_width: usize = prefix
            .iter()
            .flat_map(|s| s.text.chars())
            .map(|c| c.width_cjk().unwrap_or(1))
            .sum();
        let first_width = first_width.saturating_sub(prefix_width).max(1);
        let continuation_width = continuation_width.saturating_sub(prefix_width).max(1);

        // Flatten body spans to (plain text, per-char style list).
        let mut plain = String::new();
        let mut char_styles: Vec<Style> = Vec::new();
        for span in body {
            for ch in span.text.chars() {
                plain.push(ch);
                char_styles.push(span.style);
            }
        }

        let plain_chars: Vec<char> = plain.chars().collect();
        let total_chars = plain_chars.len();

        // Collect output lines (as Vec<RichSpan>) before the char-break post-pass.
        let mut out_lines: Vec<Vec<RichSpan>> = Vec::new();
        let mut char_cursor = 0usize; // index into plain_chars / char_styles
        let mut is_first_segment = true;

        while char_cursor < total_chars {
            let seg_width = if is_first_segment && out_lines.is_empty() {
                first_width
            } else {
                continuation_width
            };
            is_first_segment = false;

            let seg_plain: String = plain_chars[char_cursor..].iter().collect();

            let tw_opts = Options::new(seg_width).word_splitter(WordSplitter::NoHyphenation);
            let tw_result = textwrap::wrap(&seg_plain, tw_opts);

            if tw_result.is_empty() {
                // Shouldn't happen for non-empty input; push empty and bail.
                out_lines.push(Vec::new());
                break;
            }

            // Reconstruct styled spans for the first textwrap line.
            let tw_line: &str = tw_result[0].as_ref();
            let line_chars: Vec<char> = tw_line.chars().collect();
            let line_char_count = line_chars.len();

            let mut rich_line: Vec<RichSpan> = Vec::new();
            for (i, &ch) in line_chars.iter().enumerate() {
                let style = char_styles[char_cursor + i];
                push_wrapped_char(&mut rich_line, ch, style);
            }
            out_lines.push(rich_line);
            char_cursor += line_char_count;

            // Skip whitespace that became the line break (inter-word space(s)).
            while char_cursor < total_chars && plain_chars[char_cursor] == ' ' {
                char_cursor += 1;
            }
        }

        // Post-pass: char-break any line whose display width exceeds the allowed width.
        let mut final_lines: Vec<Vec<RichSpan>> = Vec::new();
        for (idx, line) in out_lines.into_iter().enumerate() {
            let max_w = if idx == 0 && final_lines.is_empty() {
                first_width
            } else {
                continuation_width
            };
            // Measure display width of the line.
            let display_w: usize = line
                .iter()
                .flat_map(|s| s.text.chars())
                .map(|c| c.width_cjk().unwrap_or(1))
                .sum();
            if display_w <= max_w {
                final_lines.push(line);
            } else {
                // Char-break this overflowing line.
                let mut current: Vec<RichSpan> = Vec::new();
                let mut remaining = max_w;
                for span in line {
                    for ch in span.text.chars() {
                        let cw = ch.width_cjk().unwrap_or(1);
                        if remaining < cw {
                            final_lines.push(current);
                            current = Vec::new();
                            remaining = continuation_width;
                        }
                        push_wrapped_char(&mut current, ch, span.style);
                        remaining = remaining.saturating_sub(cw);
                    }
                }
                if !current.is_empty() {
                    final_lines.push(current);
                }
            }
        }

        if final_lines.is_empty() {
            final_lines.push(Vec::new());
        }

        // Re-emit the gutter at the start of every wrapped row.
        if !prefix.is_empty() {
            for row in &mut final_lines {
                for (offset, span) in prefix.iter().enumerate() {
                    row.insert(offset, span.clone());
                }
            }
        }

        wrapped.extend(final_lines);
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

    #[test]
    fn gutter_repeats_on_every_wrapped_row_within_width() {
        let line = vec![
            RichSpan::gutter("> ", Style::default()),
            RichSpan::new("alpha beta gamma delta", Style::default()),
        ];
        let wrapped = wrap_rich_lines(vec![line], 10, 10);
        assert!(wrapped.len() >= 2, "long quote should wrap: {wrapped:?}");
        for row in &wrapped {
            let text: String = row.iter().map(|s| s.text.as_str()).collect();
            assert!(text.starts_with("> "), "row must keep the gutter: {text:?}");
            let width: usize = text.chars().map(|c| c.width_cjk().unwrap_or(1)).sum();
            assert!(width <= 10, "row exceeds width budget: {text:?} = {width}");
        }
    }

    #[test]
    fn lines_without_gutter_wrap_unchanged() {
        // A plain line (no repeat_on_wrap span) wraps exactly as before.
        let wrapped = wrap_rich_lines(vec![vec![RichSpan::new("abcd", Style::default())]], 2, 2);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0][0].text, "ab");
        assert_eq!(wrapped[1][0].text, "cd");
    }

    #[test]
    fn wide_chars_count_as_two_columns() {
        // 🟩 is 2 columns wide; a width-4 line fits exactly two of them
        let style = Style::default();
        let wrapped = wrap_rich_lines(vec![vec![RichSpan::new("🟩🟩🟩", style)]], 4, 4);
        assert_eq!(
            wrapped.len(),
            2,
            "three 2-wide emoji must not fit on one 4-wide line"
        );
        assert_eq!(wrapped[0][0].text, "🟩🟩");
        assert_eq!(wrapped[1][0].text, "🟩");
    }

    #[test]
    fn wide_char_does_not_overflow_when_one_column_remains() {
        // If only 1 column remains and the next char is 2-wide, it should wrap first
        let style = Style::default();
        // width 3: "a" takes 1, then 🟩 needs 2 but only 2 remain — fits; "🟩b" needs 3 on line 2
        let wrapped = wrap_rich_lines(vec![vec![RichSpan::new("a🟩b", style)]], 3, 3);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0][0].text, "a🟩");
        assert_eq!(wrapped[1][0].text, "b");
    }

    #[test]
    fn word_boundary_wrap_does_not_break_mid_word() {
        let style = Style::default();
        // "hello world" at width 8: "hello" (5) fits; "world" (5) needs 6 cols with space.
        // Result: two lines, not "hello wo" + "rld".
        let wrapped = wrap_rich_lines(vec![vec![RichSpan::new("hello world", style)]], 8, 8);
        assert_eq!(wrapped.len(), 2);
        let line0: String = wrapped[0].iter().map(|s| s.text.as_str()).collect();
        let line1: String = wrapped[1].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(line0, "hello");
        assert_eq!(line1, "world");
    }

    #[test]
    fn word_boundary_wrap_preserves_span_styles_across_break() {
        let red = Style::default().fg(ratatui::style::Color::Red);
        let blue = Style::default().fg(ratatui::style::Color::Blue);
        // Two differently-styled words; the break must fall between them.
        let wrapped = wrap_rich_lines(
            vec![vec![
                RichSpan::new("hello ", red),
                RichSpan::new("world", blue),
            ]],
            8,
            8,
        );
        assert_eq!(wrapped.len(), 2);
        // Line 0: "hello" or "hello " (trailing space may be stripped) — check first non-empty span
        let line0_text: String = wrapped[0].iter().map(|s| s.text.as_str()).collect();
        assert!(line0_text.trim() == "hello", "line 0 was: {line0_text:?}");
        // Line 1: "world" in blue
        assert_eq!(wrapped[1].len(), 1);
        assert_eq!(wrapped[1][0].text, "world");
        assert_eq!(wrapped[1][0].style, blue);
    }

    #[test]
    fn long_word_exceeding_width_falls_back_to_char_break() {
        let style = Style::default();
        // "abcdefgh" (8 chars) at width 3 — no spaces, must char-break.
        let wrapped = wrap_rich_lines(vec![vec![RichSpan::new("abcdefgh", style)]], 3, 3);
        assert!(wrapped.len() > 1, "expected char-level break for long word");
        let all: String = wrapped
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.as_str()))
            .collect();
        assert_eq!(all, "abcdefgh");
    }
}
