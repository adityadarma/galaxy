//! Markdown preview: CommonMark (plus GFM tables, strikethrough, task lists
//! and footnotes) rendered to an HTML fragment.
//!
//! Raw HTML in the source is escaped, not passed through, so a preview of an
//! untrusted file cannot inject markup or script into the web view.

use pulldown_cmark::{Event, Options, Parser, html};

/// Renders `source` to an HTML fragment (no `<html>` or `<body>`).
///
/// Every top-level block is preceded by an empty
/// `<span class="src" data-line="N">` (N is the 0-based source line), and one
/// more marks the end of the text (N = number of lines). The preview uses them to keep its scroll
/// position in step with the editor.
pub fn to_html(source: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS;

    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let line_of = |offset: usize| line_starts.partition_point(|&s| s <= offset) - 1;
    let anchor = |line: usize| Event::Html(format!("<span class=\"src\" data-line=\"{line}\"></span>").into());

    let mut events: Vec<Event> = Vec::new();
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        if depth == 0 && !matches!(event, Event::End(_)) {
            events.push(anchor(line_of(range.start)));
        }
        match &event {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
        events.push(match event {
            Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
            other => other,
        });
    }
    events.push(anchor(line_starts.len()));

    let mut out = String::with_capacity(source.len() * 3 / 2);
    html::push_html(&mut out, events.into_iter());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_and_emphasis() {
        let html = to_html("# Title\n\nsome *text*\n");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<em>text</em>"));
    }

    #[test]
    fn renders_tables_and_task_lists() {
        let html = to_html("|a|b|\n|-|-|\n|1|2|\n\n- [x] done\n");
        assert!(html.contains("<table>"));
        assert!(html.contains("checkbox"));
    }

    #[test]
    fn raw_html_is_escaped() {
        let html = to_html("<script>alert(1)</script>\n\nhi <b>x</b>\n");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<b>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn empty_source_has_only_the_end_marker() {
        assert_eq!(to_html(""), "<span class=\"src\" data-line=\"1\"></span>");
    }

    #[test]
    fn top_level_blocks_carry_their_source_line() {
        let html = to_html("# A\n\npara\nmore\n\n- x\n- y\n\n```\ncode\n```\n");
        for line in [0, 2, 5, 8] {
            assert!(html.contains(&format!("data-line=\"{line}\"")), "line {line}: {html}");
        }
        // Nested list items get no marker of their own; the end marker is the last line.
        assert_eq!(html.matches("class=\"src\"").count(), 5);
        assert!(html.ends_with("data-line=\"12\"></span>"));
    }
}
