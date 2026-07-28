use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ruma_html::{sanitize_html, Html, HtmlSanitizerMode, NodeData, RemoveReplyFallback};

use crate::config::ColorScheme;
use crate::wrap::{
    append_rich_lines, push_rich_line_break, push_rich_line_break_if_needed, push_rich_span,
    rich_current_line_is_empty, rich_lines_are_empty, rich_lines_text, rich_lines_to_spans,
    trim_empty_rich_edges, wrap_rich_lines, RichSpan,
};

#[derive(Clone)]
struct HtmlContext {
    style: Style,
    preserve_whitespace: bool,
}

/// Converts `text` to HTML if it contains markdown formatting. Returns `None`
/// if the text is plain prose (only paragraphs, text, and line breaks).
pub(crate) fn markdown_to_html_if_detected(text: &str) -> Option<String> {
    let parser = Parser::new_ext(text, Options::all());
    let mut has_formatting = false;
    let events: Vec<_> = parser
        .inspect(|event| {
            if !matches!(
                event,
                Event::Start(Tag::Paragraph)
                    | Event::End(TagEnd::Paragraph)
                    | Event::Text(_)
                    | Event::SoftBreak
                    | Event::HardBreak
            ) {
                has_formatting = true;
            }
        })
        .collect();
    if !has_formatting {
        return None;
    }
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    Some(html)
}

/// Strips HTML tags from `html`, returning the plain-text content.
/// Used to generate the required `body` fallback when sending `/html`.
pub(crate) fn strip_html_to_plain(html: &str) -> String {
    let sanitized = sanitize_html(html, HtmlSanitizerMode::Strict, RemoveReplyFallback::No);
    let doc = Html::parse(&sanitized);
    let mut text = String::new();
    for child in doc.children() {
        collect_plain_text(&child, &mut text);
    }
    text.trim().to_owned()
}

fn collect_plain_text(node: &ruma_html::NodeRef, text: &mut String) {
    match node.data() {
        NodeData::Text(t) => text.push_str(t.borrow().as_ref()),
        NodeData::Element(_) => {
            for child in node.children() {
                collect_plain_text(&child, text);
            }
        }
        _ => {}
    }
}

/// Wraps `text` in a `<span data-mx-spoiler>` element.
/// `reason` is the optional content-warning label (the `data-mx-spoiler` attribute value).
/// The plain-text body uses the convention `<reason>: <text> (Spoiler)`.
pub(crate) fn spoiler_html(reason: Option<&str>, text: &str) -> (String, String) {
    let escaped = html_escape(text);
    let html = match reason {
        Some(r) => format!(
            "<span data-mx-spoiler=\"{}\">{escaped}</span>",
            html_escape(r)
        ),
        None => format!("<span data-mx-spoiler>{escaped}</span>"),
    };
    let plain = match reason {
        Some(r) => format!("{r}: {text} (Spoiler)"),
        None => format!("{text} (Spoiler)"),
    };
    (html, plain)
}

fn html_escape(s: &str) -> String {
    html_escape::encode_double_quoted_attribute(s).into_owned()
}

/// Wraps each character of `text` in a `<font color>` tag cycling through
/// rainbow hues. The Matrix sanitizer converts `<font color>` → `<span data-mx-color>`.
pub(crate) fn rainbow_html(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    let mut html = String::new();
    for (i, ch) in chars.iter().enumerate() {
        let hue = (i as f32 / n as f32) * 360.0;
        let (r, g, b) = hsl_to_rgb(hue, 1.0, 0.5);
        let escaped = html_escape(&ch.to_string());
        html.push_str(&format!(
            "<font color=\"#{r:02x}{g:02x}{b:02x}\">{escaped}</font>"
        ));
    }
    html
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h as u16 / 60 {
        0 => (c, x, 0.0_f32),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

pub(crate) fn formatted_message_body_lines(
    html: &str,
    first_width: usize,
    continuation_width: usize,
    colors: &ColorScheme,
) -> Option<Vec<Vec<Span<'static>>>> {
    let without_dangerous_blocks = remove_dangerous_html_blocks(html);
    let sanitized = sanitize_html(
        &without_dangerous_blocks,
        HtmlSanitizerMode::Strict,
        RemoveReplyFallback::Yes,
    );
    let html = Html::parse(&sanitized);
    let mut lines = vec![Vec::new()];
    let context = HtmlContext {
        style: Style::default(),
        preserve_whitespace: false,
    };
    for child in html.children() {
        render_html_node(&child, &mut lines, context.clone(), colors);
    }
    trim_empty_rich_edges(&mut lines);
    if rich_lines_are_empty(&lines) {
        return None;
    }
    Some(rich_lines_to_spans(wrap_rich_lines(
        lines,
        first_width,
        continuation_width,
    )))
}

fn render_html_node(
    node: &ruma_html::NodeRef,
    lines: &mut Vec<Vec<RichSpan>>,
    context: HtmlContext,
    colors: &ColorScheme,
) {
    match node.data() {
        NodeData::Text(text) => {
            push_html_text(
                lines,
                text.borrow().as_ref(),
                context.style,
                context.preserve_whitespace,
            );
        }
        NodeData::Element(element) => {
            let name = element.name.local.as_ref();
            match name {
                "br" => push_rich_line_break(lines),
                "p" | "div" => render_block_children(node, lines, context, colors),
                "strong" | "b" => render_children_with_style(
                    node,
                    lines,
                    HtmlContext {
                        style: context.style.add_modifier(Modifier::BOLD),
                        ..context
                    },
                    colors,
                ),
                "em" | "i" => render_children_with_style(
                    node,
                    lines,
                    HtmlContext {
                        style: context.style.add_modifier(Modifier::ITALIC),
                        ..context
                    },
                    colors,
                ),
                "code" => render_children_with_style(
                    node,
                    lines,
                    HtmlContext {
                        style: context
                            .style
                            .fg(colors.input_hint)
                            .add_modifier(Modifier::BOLD),
                        ..context
                    },
                    colors,
                ),
                "pre" => {
                    push_rich_line_break_if_needed(lines);
                    render_children_with_style(
                        node,
                        lines,
                        HtmlContext {
                            style: context.style.fg(colors.input_hint),
                            preserve_whitespace: true,
                        },
                        colors,
                    );
                    push_rich_line_break_if_needed(lines);
                }
                "a" => render_anchor(node, lines, context, colors, anchor_href(element)),
                "blockquote" => {
                    push_rich_line_break_if_needed(lines);
                    // Render the quote body into its own buffer, then prefix every
                    // resulting line with a gutter bar. Pushing a single inline
                    // "> " instead orphaned it on its own line as soon as a
                    // block-level child (e.g. <p>) forced a line break.
                    let mut quote_lines = vec![Vec::new()];
                    render_children_with_style(
                        node,
                        &mut quote_lines,
                        HtmlContext {
                            style: context.style.fg(colors.input_hint),
                            ..context
                        },
                        colors,
                    );
                    trim_empty_rich_edges(&mut quote_lines);
                    if !rich_lines_are_empty(&quote_lines) {
                        let gutter = RichSpan::gutter("▌ ", Style::default().fg(colors.input_hint));
                        for line in &mut quote_lines {
                            line.insert(0, gutter.clone());
                        }
                        append_rich_lines(lines, quote_lines);
                        push_rich_line_break_if_needed(lines);
                    }
                }
                "ul" | "ol" => render_block_children(node, lines, context, colors),
                "li" => {
                    push_rich_line_break_if_needed(lines);
                    push_rich_span(
                        lines,
                        RichSpan::new("* ", Style::default().fg(colors.input_hint)),
                    );
                    render_children_with_style(node, lines, context, colors);
                    push_rich_line_break_if_needed(lines);
                }
                "span" => {
                    let mut style = context.style;
                    if let Some(color) = span_fg_color(element) {
                        style = style.fg(color);
                    }
                    if let Some(label) = span_spoiler_label(element) {
                        style = style.add_modifier(Modifier::DIM);
                        push_rich_span(
                            lines,
                            RichSpan::new(label, style.add_modifier(Modifier::BOLD)),
                        );
                    }
                    render_children_with_style(
                        node,
                        lines,
                        HtmlContext { style, ..context },
                        colors,
                    );
                }
                _ => render_children_with_style(node, lines, context, colors),
            }
        }
        _ => {}
    }
}

/// Returns the spoiler prefix label if the element has `data-mx-spoiler`.
/// Yields `"[reason] "` when a reason is set, `"[spoiler] "` otherwise.
fn span_spoiler_label(element: &ruma_html::ElementData) -> Option<String> {
    let attrs = element.attrs.borrow();
    let attr = attrs
        .iter()
        .find(|a| a.name.ns.as_ref().is_empty() && a.name.local.as_ref() == "data-mx-spoiler")?;
    let reason = attr.value.trim();
    Some(if reason.is_empty() {
        "[spoiler] ".to_owned()
    } else {
        format!("[{reason}] ")
    })
}

fn span_fg_color(element: &ruma_html::ElementData) -> Option<ratatui::style::Color> {
    let attrs = element.attrs.borrow();
    let value = attrs.iter().find(|attr| {
        attr.name.ns.as_ref().is_empty() && attr.name.local.as_ref() == "data-mx-color"
    })?;
    parse_hex_color(value.value.trim())
}

fn parse_hex_color(hex: &str) -> Option<ratatui::style::Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(ratatui::style::Color::Rgb(r, g, b))
}

fn remove_dangerous_html_blocks(html: &str) -> String {
    let mut remaining = html;
    let mut output = String::new();
    loop {
        let lower = remaining.to_ascii_lowercase();
        let Some((start, tag)) = find_first_dangerous_block(&lower) else {
            output.push_str(remaining);
            return output;
        };
        output.push_str(&remaining[..start]);
        let close = format!("</{tag}>");
        if let Some(end) = lower[start..].find(&close) {
            remaining = &remaining[start + end + close.len()..];
        } else {
            return output;
        }
    }
}

fn find_first_dangerous_block(lower_html: &str) -> Option<(usize, &'static str)> {
    [
        ("script", lower_html.find("<script")),
        ("style", lower_html.find("<style")),
    ]
    .into_iter()
    .filter_map(|(tag, index)| index.map(|index| (index, tag)))
    .min_by_key(|(index, _)| *index)
}

fn render_block_children(
    node: &ruma_html::NodeRef,
    lines: &mut Vec<Vec<RichSpan>>,
    context: HtmlContext,
    colors: &ColorScheme,
) {
    push_rich_line_break_if_needed(lines);
    render_children_with_style(node, lines, context, colors);
    push_rich_line_break_if_needed(lines);
}

fn render_children_with_style(
    node: &ruma_html::NodeRef,
    lines: &mut Vec<Vec<RichSpan>>,
    context: HtmlContext,
    colors: &ColorScheme,
) {
    for child in node.children() {
        render_html_node(&child, lines, context.clone(), colors);
    }
}

fn render_anchor(
    node: &ruma_html::NodeRef,
    lines: &mut Vec<Vec<RichSpan>>,
    context: HtmlContext,
    colors: &ColorScheme,
    href: Option<String>,
) {
    let link_style = context
        .style
        .fg(colors.status)
        .add_modifier(Modifier::UNDERLINED);
    let Some(href) = href else {
        render_children_with_style(
            node,
            lines,
            HtmlContext {
                style: link_style,
                ..context
            },
            colors,
        );
        return;
    };
    let href = href.trim().to_owned();
    if href.is_empty() {
        return;
    }

    let mut link_lines = vec![Vec::new()];
    render_children_with_style(
        node,
        &mut link_lines,
        HtmlContext {
            style: link_style,
            ..context
        },
        colors,
    );

    let visible_text = rich_lines_text(&link_lines).trim().to_owned();
    if visible_text.is_empty() {
        push_rich_span(lines, RichSpan::new(href, link_style));
        return;
    }

    append_rich_lines(lines, link_lines);
    if visible_text == href {
        return;
    }

    if looks_like_hyperlink(&visible_text) {
        push_rich_span(
            lines,
            RichSpan::new(" ⚠ ", Style::default().fg(colors.status)),
        );
        push_rich_span(lines, RichSpan::new(href, link_style));
    } else {
        push_rich_span(
            lines,
            RichSpan::new(" (", Style::default().fg(colors.input_hint)),
        );
        push_rich_span(lines, RichSpan::new(href, link_style));
        push_rich_span(
            lines,
            RichSpan::new(")", Style::default().fg(colors.input_hint)),
        );
    }
}

fn anchor_href(element: &ruma_html::ElementData) -> Option<String> {
    element
        .attrs
        .borrow()
        .iter()
        .find(|attr| attr.name.ns.as_ref().is_empty() && attr.name.local.as_ref() == "href")
        .map(|attr| attr.value.to_string())
}

fn looks_like_hyperlink(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with("www.") {
        return true;
    }

    let Some((scheme, rest)) = text.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && !rest.is_empty()
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
        && scheme
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic())
}

fn push_html_text(lines: &mut Vec<Vec<RichSpan>>, text: &str, style: Style, preserve: bool) {
    if preserve {
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                push_rich_line_break(lines);
            }
            push_rich_span(lines, RichSpan::new(part, style));
        }
        return;
    }

    let mut normalized = String::new();
    let mut in_whitespace = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !in_whitespace {
                normalized.push(' ');
                in_whitespace = true;
            }
        } else {
            normalized.push(ch);
            in_whitespace = false;
        }
    }
    if rich_current_line_is_empty(lines) {
        normalized = normalized.trim_start().to_owned();
    }
    if !normalized.is_empty() {
        push_rich_span(lines, RichSpan::new(normalized, style));
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::config::TuiConfig;

    #[test]
    fn rainbow_html_wraps_each_char_with_font_color() {
        let html = rainbow_html("hi");
        assert!(html.starts_with("<font color=\"#"));
        assert_eq!(html.matches("<font color=").count(), 2);
        assert!(html.contains(">h<") || html.contains(">h</"));
        assert!(html.contains(">i<") || html.contains(">i</"));
    }

    #[test]
    fn rainbow_html_escapes_special_chars() {
        let html = rainbow_html("<&>");
        assert!(html.contains("&lt;"), "should escape <");
        assert!(html.contains("&amp;"), "should escape &");
        assert!(html.contains("&gt;"), "should escape >");
        assert!(
            !html.contains("\"<\""),
            "raw < should not appear as content"
        );
    }

    #[test]
    fn rainbow_html_renders_with_color_in_tui() {
        let colors = TuiConfig::test_default().colors;
        let html = rainbow_html("hi");
        let lines = formatted_message_body_lines(&html, 80, 80, &colors)
            .expect("rainbow html should render");
        assert!(lines[0]
            .iter()
            .any(|span| matches!(span.style.fg, Some(Color::Rgb(_, _, _)))));
    }

    #[test]
    fn blockquote_keeps_gutter_on_same_line_as_text() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<blockquote><p>quoted text</p></blockquote>",
            80,
            80,
            &colors,
        )
        .expect("blockquote should render");
        let texts: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        // The gutter bar and the quoted text share one line (the bug put the
        // marker alone on its own line, with the text on the next).
        assert!(
            texts
                .iter()
                .any(|t| t.contains('▌') && t.contains("quoted text")),
            "gutter and text should share a line; got {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.trim() == "▌"),
            "gutter must not be orphaned on its own line; got {texts:?}"
        );
    }

    #[test]
    fn blockquote_gutters_every_line_of_a_multiline_quote() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<blockquote><p>line one</p><p>line two</p></blockquote>",
            80,
            80,
            &colors,
        )
        .expect("blockquote should render");
        let texts: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains('▌') && t.contains("line one")),
            "first quoted line should carry the gutter; got {texts:?}"
        );
        assert!(
            texts
                .iter()
                .any(|t| t.contains('▌') && t.contains("line two")),
            "second quoted line should carry the gutter; got {texts:?}"
        );
    }

    #[test]
    fn blockquote_gutter_repeats_on_wrapped_continuation_rows() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<blockquote><p>this is a fairly long quoted sentence that wraps onto several rows</p></blockquote>",
            20,
            20,
            &colors,
        )
        .expect("blockquote should render");
        let rows: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|s| s.content.as_ref()).collect::<String>())
            .filter(|t| !t.trim().is_empty())
            .collect();
        assert!(
            rows.len() >= 2,
            "quote should wrap to multiple rows; got {rows:?}"
        );
        assert!(
            rows.iter().all(|t| t.contains('▌')),
            "every wrapped quote row must carry the gutter; got {rows:?}"
        );
    }

    #[test]
    fn parse_hex_color_parses_valid_and_rejects_invalid() {
        assert_eq!(parse_hex_color("#ff0000"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(parse_hex_color("00ff00"), Some(Color::Rgb(0, 255, 0)));
        assert_eq!(parse_hex_color("#xyz"), None);
        assert_eq!(parse_hex_color("#fff"), None);
    }

    #[test]
    fn spoiler_html_without_reason() {
        let (html, plain) = spoiler_html(None, "big reveal");
        assert_eq!(html, "<span data-mx-spoiler>big reveal</span>");
        assert_eq!(plain, "big reveal (Spoiler)");
    }

    #[test]
    fn spoiler_html_with_reason() {
        let (html, plain) = spoiler_html(Some("CW: ending"), "he dies");
        assert_eq!(html, "<span data-mx-spoiler=\"CW: ending\">he dies</span>");
        assert_eq!(plain, "CW: ending: he dies (Spoiler)");
    }

    #[test]
    fn spoiler_html_escapes_content_and_reason() {
        let (html, _) = spoiler_html(Some("a & b"), "<secret>");
        assert!(html.contains("a &amp; b"));
        assert!(html.contains("&lt;secret&gt;"));
    }

    #[test]
    fn spoiler_renders_dimmed_with_label_in_tui() {
        let colors = TuiConfig::test_default().colors;
        let (html, _) = spoiler_html(None, "hidden");
        let lines = formatted_message_body_lines(&html, 80, 80, &colors)
            .expect("spoiler html should render");
        let flat: Vec<_> = lines.into_iter().flatten().collect();
        assert!(flat.iter().any(|s| s.content.contains("[spoiler]")));
        assert!(flat
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::DIM)));
    }

    #[test]
    fn spoiler_renders_reason_label_in_tui() {
        let colors = TuiConfig::test_default().colors;
        let (html, _) = spoiler_html(Some("CW: ending"), "he dies");
        let lines = formatted_message_body_lines(&html, 80, 80, &colors)
            .expect("spoiler html should render");
        let flat: Vec<_> = lines.into_iter().flatten().collect();
        assert!(flat.iter().any(|s| s.content.contains("[CW: ending]")));
    }

    #[test]
    fn formatted_html_renders_supported_inline_styles() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<strong>bold</strong> <a href=\"https://example.com\">link</a> <code>code</code>",
            80,
            80,
            &colors,
        )
        .expect("formatted html should render");

        assert!(lines[0][0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(lines[0][2].style.fg, Some(colors.status));
        assert!(lines[0][2]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED));
        assert!(lines[0].iter().any(|span| {
            span.content.contains("code") && span.style.fg == Some(colors.input_hint)
        }));
    }

    #[test]
    fn formatted_html_renders_link_href_when_label_is_not_url() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<a href=\"https://example.com/path\">label</a>",
            80,
            80,
            &colors,
        )
        .expect("formatted html should render");
        let rendered = line_text(&lines[0]);

        assert_eq!(rendered, "label (https://example.com/path)");
        assert!(lines[0]
            .iter()
            .any(|span| span.content == "https://example.com/path"
                && span.style.fg == Some(colors.status)
                && span.style.add_modifier.contains(Modifier::UNDERLINED)));
    }

    #[test]
    fn formatted_html_warns_when_link_label_is_different_url() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<a href=\"https://example.com\">https://evil.example</a>",
            80,
            80,
            &colors,
        )
        .expect("formatted html should render");
        let rendered = line_text(&lines[0]);

        assert_eq!(rendered, "https://evil.example ⚠ https://example.com");
        assert!(rendered.contains("⚠"));
    }

    #[test]
    fn formatted_html_does_not_repeat_matching_link_label() {
        let colors = TuiConfig::test_default().colors;
        let lines = formatted_message_body_lines(
            "<a href=\"https://example.com\">https://example.com</a>",
            80,
            80,
            &colors,
        )
        .expect("formatted html should render");

        assert_eq!(line_text(&lines[0]), "https://example.com");
    }

    #[test]
    fn formatted_html_returns_none_when_sanitized_empty() {
        let colors = TuiConfig::test_default().colors;

        assert!(
            formatted_message_body_lines("<script>alert('x')</script>", 80, 80, &colors,).is_none()
        );
    }

    fn line_text(line: &[Span<'static>]) -> String {
        line.iter().map(|span| span.content.as_ref()).collect()
    }
}
