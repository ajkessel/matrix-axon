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
                    push_rich_span(
                        lines,
                        RichSpan::new("> ", Style::default().fg(colors.input_hint)),
                    );
                    render_children_with_style(
                        node,
                        lines,
                        HtmlContext {
                            style: context.style.fg(colors.input_hint),
                            ..context
                        },
                        colors,
                    );
                    push_rich_line_break_if_needed(lines);
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
                _ => render_children_with_style(node, lines, context, colors),
            }
        }
        _ => {}
    }
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
    use ratatui::style::Modifier;

    use super::*;
    use crate::config::TuiConfig;

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
