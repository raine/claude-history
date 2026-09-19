//! Fitting text with known match ranges into a fixed column width.
//!
//! The list view knows *where* a query matched (via `search::QueryMatcher`);
//! this module decides what to show of the text around those ranges and how
//! to style it. Nothing here re-derives matches.

use crate::search::QueryMatcher;
use crate::search::matcher::merge_overlapping;
use ratatui::prelude::*;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncate text to max_width columns, adding "…" suffix if truncated.
pub(super) fn simple_truncate(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let mut result = String::new();
    let ellipsis_width = UnicodeWidthChar::width('…').unwrap_or(1);
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + ellipsis_width > max_width {
            break;
        }
        result.push(ch);
        width += ch_width;
    }
    result.push('…');
    result
}

/// Truncate from the left, keeping the tail and prefixing "…".
fn truncate_start(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let ellipsis_width = UnicodeWidthChar::width('…').unwrap_or(1);
    let mut chars = Vec::new();
    let mut width = 0;
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + ellipsis_width > max_width {
            break;
        }
        chars.push(ch);
        width += ch_width;
    }
    chars.reverse();
    format!("…{}", chars.into_iter().collect::<String>())
}

/// Spans for `text` with every range the matcher finds in it highlighted.
pub(super) fn highlight(
    matcher: &QueryMatcher,
    text: &str,
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    highlight_ranges(text, matcher.ranges(text), base_style, highlight_style)
}

/// Spans for `text` with the given byte ranges highlighted. Ranges are merged
/// and any that overrun the text are dropped.
pub(super) fn highlight_ranges(
    text: &str,
    ranges: Vec<(usize, usize)>,
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let merged = merge_overlapping(ranges)
        .into_iter()
        .filter(|(_, end)| *end <= text.len())
        .collect::<Vec<_>>();

    if merged.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let mut spans = Vec::new();
    let mut pos = 0;

    for (start, end) in &merged {
        if *start > pos {
            spans.push(Span::styled(text[pos..*start].to_string(), base_style));
        }
        spans.push(Span::styled(
            text[*start..*end].to_string(),
            highlight_style,
        ));
        pos = *end;
    }

    if pos < text.len() {
        spans.push(Span::styled(text[pos..].to_string(), base_style));
    }

    spans
}

/// Fit already-sanitized `text` into `max_width` columns so that its match
/// `ranges` stay visible: nearby matches are clustered (up to three) and
/// shown with context, joined by "…". Falls back to simple truncation when
/// every match already fits, and to a window around the first match when
/// the clustered form is still too wide.
pub(super) fn fit_around_matches(
    text: &str,
    ranges: &[(usize, usize)],
    max_width: usize,
) -> String {
    if ranges.is_empty() || max_width == 0 {
        return simple_truncate(text, max_width);
    }

    // Convert byte ranges to char ranges for width budgeting
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    let text_char_len = char_indices.len();

    let byte_to_char = |byte_pos: usize| -> usize {
        char_indices
            .iter()
            .position(|(b, _)| *b >= byte_pos)
            .unwrap_or(text_char_len)
    };

    let char_ranges: Vec<(usize, usize)> = ranges
        .iter()
        .map(|(s, e)| (byte_to_char(*s), byte_to_char(*e)))
        .collect();

    // If all matches fit within simple truncation, use that
    let last_match_end = char_ranges.last().map(|(_, e)| *e).unwrap_or(0);
    if last_match_end <= max_width.saturating_sub(1) {
        return simple_truncate(text, max_width);
    }

    // Cluster nearby matches (gap < 20 chars)
    let merge_gap = 20;
    let mut clusters: Vec<(usize, usize)> = Vec::new();
    for &(cs, ce) in &char_ranges {
        if let Some(last) = clusters.last_mut()
            && cs <= last.1 + merge_gap
        {
            last.1 = last.1.max(ce);
            continue;
        }
        clusters.push((cs, ce));
    }
    clusters.truncate(3);

    // Budget: matched chars plus one ellipsis per gap (leading, between, trailing)
    let num_clusters = clusters.len();
    let match_chars: usize = clusters.iter().map(|(s, e)| e - s).sum();
    let max_ellipsis = num_clusters + 1;
    let available_context = max_width
        .saturating_sub(match_chars)
        .saturating_sub(max_ellipsis);
    let padding_per_side = available_context / (num_clusters * 2);

    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut last_seg_end: usize = 0;

    for (i, &(cl_start, cl_end)) in clusters.iter().enumerate() {
        let mut seg_start = cl_start.saturating_sub(padding_per_side);
        let seg_end = (cl_end + padding_per_side).min(text_char_len);

        if i > 0 {
            seg_start = seg_start.max(last_seg_end);
        }

        if (i == 0 && seg_start > 0) || (i > 0 && seg_start > last_seg_end) {
            result.push('…');
        }

        result.extend(&chars[seg_start..seg_end]);
        last_seg_end = seg_end;
    }

    let last_cluster_end = clusters.last().map(|(_, e)| *e).unwrap_or(0);
    if last_cluster_end + padding_per_side < text_char_len {
        result.push('…');
    }

    if UnicodeWidthStr::width(result.as_str()) > max_width {
        return truncate_around_match(text, ranges[0], max_width);
    }

    result
}

/// Window `text` around one match so the match itself stays visible.
fn truncate_around_match(text: &str, (start, end): (usize, usize), max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let matched = &text[start..end];
    let matched_width = UnicodeWidthStr::width(matched);
    if matched_width >= max_width {
        return simple_truncate(matched, max_width);
    }

    let ellipsis_budget = usize::from(start > 0) + usize::from(end < text.len());
    let context_budget = max_width.saturating_sub(matched_width + ellipsis_budget);
    let left_budget = context_budget / 2;
    let right_budget = context_budget - left_budget;
    format!(
        "{}{}{}",
        truncate_start(&text[..start], left_budget + usize::from(start > 0)),
        matched,
        simple_truncate(&text[end..], right_budget + usize::from(end < text.len()))
    )
}

/// One line of context for matches the preview hides: a sanitized window
/// around each hidden range of raw `full_text`, joined by "…" and fitted to
/// `max_width`.
pub(super) fn context_snippet(
    full_text: &str,
    hidden_ranges: &[(usize, usize)],
    max_width: usize,
) -> Option<String> {
    if hidden_ranges.is_empty() || max_width == 0 {
        return None;
    }

    let num_segments = hidden_ranges.len();
    let budget_per_segment = max_width.saturating_sub(num_segments + 1) / num_segments; // reserve for ellipsis

    let mut result = String::new();
    let mut remaining_width = max_width;
    let mut prev_end_byte: usize = 0;

    for (i, &(match_start, match_end)) in hidden_ranges.iter().enumerate() {
        let match_char_len = full_text[match_start..match_end].chars().count();
        let context_chars = budget_per_segment
            .saturating_sub(match_char_len)
            .saturating_sub(2) // reserve for "…" on each side
            / 2;

        let mut start_byte = full_text[..match_start]
            .char_indices()
            .rev()
            .nth(context_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        start_byte = start_byte.max(prev_end_byte);

        let end_byte = full_text[match_end..]
            .char_indices()
            .nth(context_chars)
            .map(|(idx, _)| match_end + idx)
            .unwrap_or(full_text.len())
            .min(full_text.len());

        let sanitized = sanitize_preview(&full_text[start_byte..end_byte]);

        let has_gap = if i == 0 {
            start_byte > 0
        } else {
            start_byte > prev_end_byte
        };
        if has_gap {
            result.push('…');
            remaining_width = remaining_width.saturating_sub(1);
        }

        prev_end_byte = end_byte;

        let seg_char_count = sanitized.chars().count();
        if seg_char_count <= remaining_width {
            result.push_str(&sanitized);
            remaining_width = remaining_width.saturating_sub(seg_char_count);
        } else {
            let budget = remaining_width.saturating_sub(1);
            result.extend(sanitized.chars().take(budget));
            result.push('…');
            remaining_width = 0;
            break;
        }
    }

    if remaining_width > 0 {
        let last_end = hidden_ranges.last().map(|(_, e)| *e).unwrap_or(0);
        if last_end < full_text.len() {
            result.push('…');
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(simple_truncate(&result, max_width))
    }
}

/// Sanitize preview text by removing XML-like tags and normalizing whitespace
pub(super) fn sanitize_preview(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut last_was_space = false;

    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '\n' | '\r' | '\t' | ' ' => {
                if !last_was_space {
                    result.push(' ');
                    last_was_space = true;
                }
            }
            _ => {
                result.push(ch);
                last_was_space = false;
            }
        }
    }

    result.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn fit(text: &str, query: &str, width: usize) -> String {
        fit_around_matches(text, &QueryMatcher::from_query(query).ranges(text), width)
    }

    fn context(full_text: &str, preview: &str, query: &str, width: usize) -> Option<String> {
        let ranges = QueryMatcher::from_query(query).hidden_context(full_text, preview)?;
        context_snippet(full_text, &ranges, width)
    }

    #[test]
    fn highlight_splits_spans_at_match_boundaries() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let spans = highlight(&QueryMatcher::from_query("world"), "hello world!", base, hl);
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["hello ", "world", "!"]);
        assert_eq!(spans[1].style, hl);
    }

    #[test]
    fn highlight_without_matches_is_one_span() {
        let spans = highlight(
            &QueryMatcher::from_query("zzz"),
            "hello",
            Style::default(),
            Style::default(),
        );
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "hello");
    }

    #[test]
    fn highlight_ranges_drops_ranges_past_the_end() {
        let spans = highlight_ranges("abc", vec![(0, 10)], Style::default(), Style::default());
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn fit_without_query_truncates() {
        let text = "a fairly long line of preview text";
        assert_eq!(fit(text, "", 20), simple_truncate(text, 20));
    }

    #[test]
    fn fit_when_matches_already_fit_truncates() {
        let text = "hello world and then some much longer trailing text";
        assert_eq!(fit(text, "hello", 20), simple_truncate(text, 20));
    }

    #[test]
    fn fit_windows_distant_matches() {
        let text = format!("alpha {} omega", "x ".repeat(40));
        let fitted = fit(&text, "alpha omega", 30);
        assert!(fitted.contains("alpha"));
        assert!(fitted.contains("omega"));
        assert!(fitted.contains('…'));
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= 30);
    }

    #[test]
    fn fit_merges_close_matches() {
        let text = format!("{} alpha and omega {}", "y ".repeat(30), "z ".repeat(30));
        let fitted = fit(&text, "alpha omega", 30);
        assert!(fitted.contains("alpha and omega"));
    }

    #[test]
    fn context_none_when_all_visible() {
        assert!(context("hello world", "hello world", "hello", 40).is_none());
    }

    #[test]
    fn context_shows_hidden_match_with_ellipses() {
        let full = format!("visible preview {} hidden keyword here", "q ".repeat(60));
        let snippet = context(&full, "visible preview", "keyword", 30).unwrap();
        assert!(snippet.contains("keyword"));
        assert!(snippet.starts_with('…'));
        assert!(UnicodeWidthStr::width(snippet.as_str()) <= 30);
    }

    #[test]
    fn context_sanitizes_tags_and_whitespace() {
        let full = format!("{} the <b>hidden</b>\n\tkeyword", "r ".repeat(60));
        let snippet = context(&full, "preview", "keyword", 40).unwrap();
        assert!(!snippet.contains('<'));
        assert!(!snippet.contains('\n'));
    }

    #[test]
    fn sanitize_preview_collapses_whitespace_and_strips_tags() {
        assert_eq!(
            sanitize_preview("  a <tag attr=\"x\">b</tag>\n\n c\t d "),
            "a b c d"
        );
    }

    #[test]
    fn simple_truncate_respects_display_width() {
        assert_eq!(simple_truncate("日本語テキスト", 7), "日本語…");
        assert_eq!(simple_truncate("short", 10), "short");
        assert_eq!(simple_truncate("anything", 0), "");
    }
}
