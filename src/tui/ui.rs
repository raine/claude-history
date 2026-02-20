use crate::history::ProviderKind;
use crate::tui::app::{
    App, AppMode, DialogMode, LineStyle, LoadingState, RenderedLine, ViewSearchMode, ViewState,
};
use crate::tui::search::is_word_separator;
use chrono::{DateTime, Local};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use ratatui::layout::Position;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};

/// Lines per conversation item (header + preview + separator)
const LINES_PER_ITEM: usize = 3;

/// Duration before status messages auto-clear
const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(3);

/// Format model name for display (e.g., "claude-opus-4-5-20251101" → "opus-4.5")
fn format_model_name(model: &str) -> String {
    // Handle claude-opus-4-5-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-opus-4-5-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "opus-4.5".to_string();
    }

    // Handle claude-sonnet-4-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-sonnet-4-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "sonnet-4".to_string();
    }

    // Handle claude-3-5-sonnet-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-3-5-sonnet-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "sonnet-3.5".to_string();
    }

    // Handle claude-3-5-haiku-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-3-5-haiku-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "haiku-3.5".to_string();
    }

    // Handle claude-3-opus-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-3-opus-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "opus-3".to_string();
    }

    // Handle claude-3-sonnet-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-3-sonnet-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "sonnet-3".to_string();
    }

    // Handle claude-3-haiku-YYYYMMDD format
    if let Some(rest) = model.strip_prefix("claude-3-haiku-")
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return "haiku-3".to_string();
    }

    // Unknown format - truncate if too long
    if model.len() > 20 {
        format!("{}…", &model[..19])
    } else {
        model.to_string()
    }
}

/// Format token count with K/M suffix (short form, e.g., "926k")
fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// Format token count with K/M suffix and "tokens" label (long form, e.g., "926k tokens")
fn format_tokens_long(tokens: u64) -> String {
    format!("{} tokens", format_tokens(tokens))
}

/// Create a colored provider badge span
fn provider_badge(provider: ProviderKind) -> Span<'static> {
    match provider {
        ProviderKind::Claude => Span::styled(
            "[CC] ",
            Style::default().fg(Color::Rgb(78, 201, 176)), // Teal
        ),
        ProviderKind::Cursor => Span::styled(
            "[Cu] ",
            Style::default().fg(Color::Rgb(180, 130, 230)), // Purple
        ),
    }
}

/// Render the TUI
pub fn render(frame: &mut Frame, app: &App) {
    match app.app_mode() {
        AppMode::List => render_list_mode(frame, app),
        AppMode::View(state) => render_view_mode(frame, app, state),
    }
}

/// Render the list mode (conversation browser)
fn render_list_mode(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Outer border wrapping the entire app
    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(60, 60, 60)));
    let inner_area = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    // Graceful degradation for tiny terminals - skip bottom bar if too small
    if inner_area.height < 4 {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(inner_area);
        render_search_bar(frame, app, chunks[0]);
        render_list(frame, app, chunks[1]);
        return;
    }

    // Always reserve space for bottom bar (status, dialog, or hotkeys)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner_area);

    render_search_bar(frame, app, chunks[0]);
    render_list(frame, app, chunks[1]);

    // Render bottom bar: confirm dialog > status message > hotkeys
    if *app.dialog_mode() == DialogMode::ConfirmDelete {
        render_confirm_dialog(frame, chunks[2]);
    } else if let Some((msg, instant)) = app.status_message()
        && instant.elapsed() < STATUS_TTL
    {
        render_status_message(frame, msg, chunks[2]);
    } else {
        render_list_status_bar(frame, app, chunks[2]);
    }

    // Render help overlay on top of everything if active
    if *app.dialog_mode() == DialogMode::Help {
        render_help_overlay(frame, false, false);
    }
}

fn render_status_message(frame: &mut Frame, msg: &str, area: Rect) {
    let status_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(msg, Style::default().fg(Color::Yellow)),
    ]);
    let status = Paragraph::new(status_line).style(Style::default().bg(Color::Rgb(30, 30, 35)));
    frame.render_widget(status, area);
}

fn render_list_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let is_loading = app.is_loading();

    let key_style = Style::default().fg(Color::Rgb(78, 201, 176));
    let label_style = Style::default().fg(Color::Rgb(100, 100, 100));
    // Dimmed styles for unavailable shortcuts during loading
    let dim_key_style = Style::default().fg(Color::Rgb(60, 60, 60));
    let dim_label_style = Style::default().fg(Color::Rgb(60, 60, 60));

    let (action_key, action_label) = if is_loading {
        (dim_key_style, dim_label_style)
    } else {
        (key_style, label_style)
    };

    let spans = vec![
        Span::raw("  "),
        Span::styled("Enter", action_key),
        Span::styled(" open  ", action_label),
        Span::styled("^R", action_key),
        Span::styled(" resume  ", action_label),
        Span::styled("^X", action_key),
        Span::styled(" delete  ", action_label),
        Span::styled("?", key_style),
        Span::styled("help  ", label_style),
        Span::styled("Esc", key_style),
        Span::styled(" quit", label_style),
    ];

    let status_line = Line::from(spans);
    let status = Paragraph::new(status_line).style(Style::default().bg(Color::Rgb(30, 30, 35)));
    frame.render_widget(status, area);
}

/// Check if the header (with summary) fits on a single line given terminal width
fn header_fits_single_line(conv: &crate::history::Conversation, terminal_width: u16) -> bool {
    let summary = match &conv.summary {
        Some(s) => s,
        None => return true, // No summary means it's already single line
    };

    let project = conv.project_name.as_deref().unwrap_or("Unknown");

    // Calculate model length if present
    let model_len = conv
        .model
        .as_ref()
        .map(|m| format_model_name(m).len() + 3) // + " · "
        .unwrap_or(0);

    let msg_count_len = if conv.message_count == 1 {
        "1 message".len()
    } else {
        format!("{} messages", conv.message_count).len()
    };

    // Calculate tokens length if present (use long form for single-line check)
    let tokens_len = if conv.total_tokens > 0 {
        format_tokens_long(conv.total_tokens).len() + 3 // + " · "
    } else {
        0
    };

    // timestamp is "YYYY-MM-DD HH:MM" = 16 chars
    let timestamp_len = 16;

    // Duration length (if present): " · Xm" or " · Xh Ym" etc.
    let duration_len = conv.duration_minutes.map_or(0, |m| {
        let formatted = if m >= 60 {
            format!("{}h {}m", m / 60, m % 60)
        } else {
            format!("{}m", m)
        };
        3 + formatted.len() // " · " + duration
    });

    // Format: "  project · model · msg_count · duration · tokens · timestamp · summary"
    let total_len = 2
        + project.len()
        + 3
        + model_len
        + msg_count_len
        + duration_len
        + 3
        + tokens_len
        + timestamp_len
        + 3
        + summary.len();

    total_len <= terminal_width as usize
}

/// Render the view mode (conversation viewer)
fn render_view_mode(frame: &mut Frame, app: &App, state: &ViewState) {
    let area = frame.area();

    // Determine if we need extra space for search input
    let status_height = if state.search_mode == ViewSearchMode::Typing {
        2
    } else {
        1
    };

    // Check if conversation has summary for header height
    let conv = app
        .conversations()
        .iter()
        .find(|c| c.path == state.conversation_path);
    let has_summary = conv.is_some_and(|c| c.summary.is_some());
    let fits_single_line = conv.is_some_and(|c| header_fits_single_line(c, area.width));
    let header_height = if has_summary && !fits_single_line {
        3
    } else {
        2
    };

    // Layout: header (2-3 lines) | content | status bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // Header
            Constraint::Min(1),                // Content
            Constraint::Length(status_height), // Status bar (+ search input if typing)
        ])
        .split(area);

    render_view_header(frame, app, state, chunks[0]);
    render_view_content(frame, state, chunks[1]);

    if state.search_mode == ViewSearchMode::Typing {
        render_search_input(frame, state, chunks[2]);
    } else {
        render_view_status_bar(frame, app, state, chunks[2]);
    }

    // Render dialog overlay if active
    match app.dialog_mode() {
        DialogMode::ConfirmDelete => render_confirm_dialog(frame, chunks[2]),
        DialogMode::ExportMenu { selected } => render_export_menu(frame, *selected, false),
        DialogMode::YankMenu { selected } => render_export_menu(frame, *selected, true),
        DialogMode::Help => render_help_overlay(frame, true, app.is_single_file_mode()),
        DialogMode::None => {}
    }
}

fn render_view_header(frame: &mut Frame, app: &App, state: &ViewState, area: Rect) {
    // Find the conversation by path (works for both list and single file mode)
    let conv = app
        .conversations()
        .iter()
        .find(|c| c.path == state.conversation_path);

    let (project, model, msg_count, duration, tokens, timestamp, summary, fits_single) =
        if let Some(conv) = conv {
            let project = conv.project_name.as_deref().unwrap_or("Unknown");
            let model = conv.model.as_ref().map(|m| format_model_name(m));
            let msg_count = if conv.message_count == 1 {
                "1 message".to_string()
            } else {
                format!("{} messages", conv.message_count)
            };
            // Format conversation duration
            let duration = conv.duration_minutes.map(|m| {
                if m >= 60 {
                    format!("{}h {}m", m / 60, m % 60)
                } else {
                    format!("{}m", m)
                }
            });

            // Calculate header length to determine if long token format fits
            let model_len = model.as_ref().map(|m| m.len() + 3).unwrap_or(0); // + " · "
            let duration_len = duration.as_ref().map(|d| d.len() + 3).unwrap_or(0); // + " · "
            let base_len =
                2 + project.len() + 3 + model_len + msg_count.len() + duration_len + 3 + 16; // 16 = timestamp

            let tokens = if conv.total_tokens > 0 {
                let long_form = format_tokens_long(conv.total_tokens);
                let short_form = format_tokens(conv.total_tokens);
                // Use long form if it fits (base + " · " + tokens <= width)
                if base_len + 3 + long_form.len() <= area.width as usize {
                    Some(long_form)
                } else {
                    Some(short_form)
                }
            } else {
                None
            };

            let timestamp = conv.timestamp.format("%Y-%m-%d %H:%M").to_string();
            let fits = header_fits_single_line(conv, area.width);
            (
                project.to_string(),
                model,
                msg_count,
                duration,
                tokens,
                timestamp,
                conv.summary.clone(),
                fits,
            )
        } else {
            // Fallback if parsing failed
            let project = state
                .conversation_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string();
            (
                project,
                None,
                "".to_string(),
                None,
                None,
                "".to_string(),
                None,
                true,
            )
        };

    // Build header spans for metadata line
    let conv_provider = conv.map(|c| c.provider.clone());
    let build_metadata_spans = |include_summary: bool| {
        let mut spans = vec![Span::raw("  ")];
        if let Some(ref provider) = conv_provider {
            spans.push(provider_badge(provider.clone()));
        }
        spans.push(Span::styled(
            project.clone(),
            Style::default().fg(Color::Rgb(78, 201, 176)).bold(),
        ));

        // Add model if present
        if let Some(ref m) = model {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                m.clone(),
                Style::default().fg(Color::Rgb(180, 140, 200)),
            ));
        }

        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            msg_count.clone(),
            Style::default().fg(Color::Rgb(140, 140, 140)),
        ));

        // Add conversation duration if present
        if let Some(ref d) = duration {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                d.clone(),
                Style::default().fg(Color::Rgb(100, 140, 130)),
            ));
        }

        // Add tokens if present
        if let Some(ref t) = tokens {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                t.clone(),
                Style::default().fg(Color::Rgb(140, 140, 140)),
            ));
        }

        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            timestamp.clone(),
            Style::default().fg(Color::Rgb(140, 140, 140)),
        ));

        // Add summary if requested
        if include_summary && let Some(ref s) = summary {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                s.clone(),
                Style::default().fg(Color::Rgb(180, 180, 180)),
            ));
        }

        spans
    };

    // Build header lines
    let lines = if fits_single && summary.is_some() {
        // Single line with summary
        vec![Line::from(build_metadata_spans(true))]
    } else {
        // Two lines (or single line without summary)
        let mut lines = vec![Line::from(build_metadata_spans(false))];

        // Add summary on second line if available
        if let Some(summary_text) = summary {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(summary_text, Style::default().fg(Color::Rgb(180, 180, 180))),
            ]));
        }
        lines
    };

    let header = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 60))),
    );

    frame.render_widget(header, area);
}

fn render_view_content(frame: &mut Frame, state: &ViewState, area: Rect) {
    let visible_height = area.height as usize;
    let query_lower = state.search_query.to_lowercase();

    let visible_lines: Vec<Line> = state
        .rendered_lines
        .iter()
        .enumerate()
        .skip(state.scroll_offset)
        .take(visible_height)
        .map(|(line_idx, rendered)| {
            let is_current_match = state.search_matches.get(state.current_match) == Some(&line_idx);
            let has_match = !query_lower.is_empty() && state.search_matches.contains(&line_idx);

            let spans: Vec<Span> = if has_match && !query_lower.is_empty() {
                highlight_line_matches(rendered, &query_lower, is_current_match)
            } else {
                rendered
                    .spans
                    .iter()
                    .map(|(text, style)| styled_span(text, style))
                    .collect()
            };

            Line::from(spans)
        })
        .collect();

    let content = Paragraph::new(visible_lines);
    frame.render_widget(content, area);
}

fn render_view_status_bar(frame: &mut Frame, app: &App, state: &ViewState, area: Rect) {
    // Check for status message first
    if let Some((msg, instant)) = app.status_message()
        && instant.elapsed() < STATUS_TTL
    {
        let status_line = Line::from(vec![
            Span::raw("  "),
            Span::styled(msg, Style::default().fg(Color::Green)),
        ]);
        let status = Paragraph::new(status_line).style(Style::default().bg(Color::Rgb(30, 30, 35)));
        frame.render_widget(status, area);
        return;
    }

    // Fixed-width scroll position to prevent bar from jumping
    // Use minimum width of 4 for both numbers to handle most conversations
    let total = state.total_lines.max(1);
    let width = total.to_string().len().max(4);
    let scroll_pos = format!("[{:>width$}/{:<width$}]", state.scroll_offset + 1, total);

    let key_style = Style::default().fg(Color::Rgb(78, 201, 176));
    let label_style = Style::default().fg(Color::Rgb(100, 100, 100));

    // Fixed-width status labels to prevent jumping when toggling
    let tools_status = state.tool_display.status_label();
    let thinking_status = if state.show_thinking { "on " } else { "off" };
    let timing_status = if state.show_timing { "on " } else { "off" };

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(scroll_pos, Style::default().fg(Color::Rgb(140, 140, 140))),
        Span::raw("  "),
        Span::styled("t", key_style),
        Span::styled(format!("ools·{} ", tools_status), label_style),
        Span::styled("T", key_style),
        Span::styled(format!("hink·{} ", thinking_status), label_style),
        Span::styled("i", key_style),
        Span::styled(format!("nfo·{}", timing_status), label_style),
        Span::raw("  "),
        Span::styled("│", label_style),
        Span::raw("  "),
    ];

    if state.search_mode == ViewSearchMode::Active {
        spans.extend([
            Span::styled("n", key_style),
            Span::styled("ext  ", label_style),
            Span::styled("N", key_style),
            Span::styled("prev  ", label_style),
            Span::styled("Esc", key_style),
            Span::styled(" clear", label_style),
        ]);
    } else {
        spans.extend([
            Span::styled("?", key_style),
            Span::styled("help  ", label_style),
            Span::styled("/", key_style),
            Span::styled("search  ", label_style),
            Span::styled("e", key_style),
            Span::styled("xport  ", label_style),
            Span::styled("y", key_style),
            Span::styled("ank  ", label_style),
            Span::styled("^R", key_style),
            Span::styled(" resume  ", label_style),
            Span::styled("^X", key_style),
            Span::styled(" del  ", label_style),
            Span::styled("q", key_style),
            Span::styled("uit", label_style),
        ]);
    }

    let status_line = Line::from(spans);
    let status = Paragraph::new(status_line).style(Style::default().bg(Color::Rgb(30, 30, 35)));
    frame.render_widget(status, area);
}

fn render_search_input(frame: &mut Frame, state: &ViewState, area: Rect) {
    let match_info = if state.search_matches.is_empty() {
        if state.search_query.is_empty() {
            String::new()
        } else {
            " (no matches)".to_string()
        }
    } else {
        format!(
            " ({}/{})",
            state.current_match + 1,
            state.search_matches.len()
        )
    };

    let input_line = Line::from(vec![
        Span::raw("  /"),
        Span::styled(&state.search_query, Style::default().fg(Color::White)),
        Span::styled(match_info, Style::default().fg(Color::Rgb(140, 140, 140))),
    ]);

    let input = Paragraph::new(input_line).style(Style::default().bg(Color::Rgb(30, 30, 35)));
    frame.render_widget(input, area);

    // Position cursor
    let cursor_x = area.x + 3 + state.search_query.chars().count() as u16;
    frame.set_cursor_position(Position::new(cursor_x, area.y));
}

/// Highlight search matches across the full line text, handling matches that span
/// across multiple styled spans. Works by finding match positions in the concatenated
/// line text, then rebuilding spans with highlights applied at the correct positions.
fn highlight_line_matches(
    rendered: &RenderedLine,
    query: &str,
    is_current_match: bool,
) -> Vec<Span<'static>> {
    // Concatenate all span texts to get the full line
    let full_text: String = rendered
        .spans
        .iter()
        .map(|(text, _)| text.as_str())
        .collect();
    let full_lower = full_text.to_lowercase();

    // Find match positions using char indices to safely handle Unicode
    // (lowercasing can change byte lengths for some characters)
    let orig_chars: Vec<(usize, char)> = full_text.char_indices().collect();
    let lower_chars: Vec<char> = full_lower.chars().collect();
    let query_chars: Vec<char> = query.chars().collect();

    let mut match_byte_ranges: Vec<(usize, usize)> = Vec::new();
    if !query_chars.is_empty() {
        let mut i = 0;
        while i + query_chars.len() <= lower_chars.len() {
            if lower_chars[i..i + query_chars.len()] == query_chars[..] {
                // Guard against Unicode casing expansion (e.g. ß → ss) where
                // lower_chars may be longer than orig_chars
                if i >= orig_chars.len() {
                    break;
                }
                let start_byte = orig_chars[i].0;
                let end_byte = if i + query_chars.len() < orig_chars.len() {
                    orig_chars[i + query_chars.len()].0
                } else {
                    full_text.len()
                };
                match_byte_ranges.push((start_byte, end_byte));
                i += query_chars.len();
            } else {
                i += 1;
            }
        }
    }

    if match_byte_ranges.is_empty() {
        return rendered
            .spans
            .iter()
            .map(|(t, s)| styled_span(t, s))
            .collect();
    }

    let match_style = if is_current_match {
        Style::default().bg(Color::Yellow).fg(Color::Black)
    } else {
        Style::default()
            .bg(Color::Rgb(78, 201, 176))
            .fg(Color::Black)
    };

    // Build output spans by walking through original spans and splitting at match boundaries
    let mut result: Vec<Span<'static>> = Vec::new();
    let mut match_idx = 0;
    let mut global_offset: usize = 0;

    for (text, style) in &rendered.spans {
        let span_start = global_offset;
        let span_end = global_offset + text.len();
        let base_style = build_style(style);
        let mut pos = span_start;

        while pos < span_end {
            // Skip past matches that are entirely before our position
            while match_idx < match_byte_ranges.len() && match_byte_ranges[match_idx].1 <= pos {
                match_idx += 1;
            }

            if match_idx < match_byte_ranges.len() {
                let (ms, me) = match_byte_ranges[match_idx];
                if pos >= ms && pos < me {
                    // Inside a match
                    let end = me.min(span_end);
                    result.push(Span::styled(full_text[pos..end].to_string(), match_style));
                    pos = end;
                } else if ms < span_end {
                    // There's a match starting within this span, emit text before it
                    let end = ms.min(span_end);
                    if end > pos {
                        result.push(Span::styled(full_text[pos..end].to_string(), base_style));
                    }
                    pos = end;
                } else {
                    // No more matches in this span
                    result.push(Span::styled(
                        full_text[pos..span_end].to_string(),
                        base_style,
                    ));
                    pos = span_end;
                }
            } else {
                // No more matches at all
                result.push(Span::styled(
                    full_text[pos..span_end].to_string(),
                    base_style,
                ));
                pos = span_end;
            }
        }

        global_offset = span_end;
    }

    result
}

fn build_style(style: &LineStyle) -> Style {
    let mut s = Style::default();
    if let Some((r, g, b)) = style.fg {
        s = s.fg(Color::Rgb(r, g, b));
    }
    if style.bold {
        s = s.bold();
    }
    if style.italic {
        s = s.italic();
    }
    if style.dimmed {
        s = s.fg(Color::Rgb(100, 100, 100));
    }
    s
}

fn styled_span(text: &str, style: &LineStyle) -> Span<'static> {
    Span::styled(text.to_string(), build_style(style))
}

fn render_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    let status_text = match app.loading_state() {
        LoadingState::Loading { loaded } => {
            format!("Loading... {}", loaded)
        }
        LoadingState::Ready => {
            format!("{}/{}", app.filtered().len(), app.conversations().len())
        }
    };

    // Build search line: " ❯ query" on left, "status " on right
    let prompt = " ❯ ";
    let query = app.query();
    let left_len = prompt.chars().count() + query.chars().count();
    let count_len = status_text.chars().count() + 1; // +1 for trailing space
    let padding = (area.width as usize).saturating_sub(left_len + count_len + 1);

    // Prompt is always active - user can type during loading
    let prompt_style = Style::default().fg(Color::Rgb(78, 201, 176));

    let status_style = if app.is_loading() {
        Style::default().fg(Color::Rgb(78, 201, 176)) // Highlight loading status
    } else {
        Style::default().fg(Color::Rgb(100, 100, 100))
    };

    let search_line = Line::from(vec![
        Span::raw(" "),
        Span::styled("❯ ", prompt_style),
        Span::raw(query.to_string()),
        Span::raw(" ".repeat(padding)),
        Span::styled(status_text, status_style),
        Span::raw(" "),
    ]);

    let input = Paragraph::new(search_line).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(60, 60, 60))),
    );

    frame.render_widget(input, area);

    // Position cursor at cursor_pos (account for " ❯ " prefix)
    if area.width > 3 {
        let cursor_offset = app.cursor_pos() as u16;
        let max_x = area.x + area.width.saturating_sub(2);
        let cursor_x = (area.x + 3).saturating_add(cursor_offset).min(max_x);
        frame.set_cursor_position(Position::new(cursor_x, area.y));
    }
}

fn render_confirm_dialog(frame: &mut Frame, area: Rect) {
    let prompt = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Delete this conversation? ",
            Style::default().fg(Color::Yellow),
        ),
        Span::styled("(y/n)", Style::default().fg(Color::Rgb(140, 140, 140))),
    ]);
    let paragraph = Paragraph::new(prompt);
    frame.render_widget(paragraph, area);
}

fn render_export_menu(frame: &mut Frame, selected: usize, is_yank: bool) {
    let title = if is_yank {
        "Copy to clipboard"
    } else {
        "Export to file"
    };
    let options = [
        "[1] Ledger (formatted)",
        "[2] Plain text",
        "[3] Markdown",
        "[4] JSONL (raw)",
    ];

    let area = frame.area();
    let menu_width = 35;
    let menu_height = options.len() as u16 + 4; // options + title + border + cancel hint

    // Center the menu
    let menu_area = Rect {
        x: (area.width.saturating_sub(menu_width)) / 2,
        y: (area.height.saturating_sub(menu_height)) / 2,
        width: menu_width,
        height: menu_height,
    };

    // Clear the area behind the modal first
    frame.render_widget(Clear, menu_area);

    // Render background
    let background = Block::default().style(Style::default().bg(Color::Rgb(25, 25, 30)));
    frame.render_widget(background, menu_area);

    // Render border
    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(78, 201, 176)));

    let inner = block.inner(menu_area);
    frame.render_widget(block, menu_area);

    // Render options
    let mut lines = Vec::new();
    for (i, opt) in options.iter().enumerate() {
        let style = if i == selected {
            Style::default().fg(Color::Rgb(78, 201, 176)).bold()
        } else {
            Style::default().fg(Color::White)
        };
        let prefix = if i == selected { "▶ " } else { "  " };
        lines.push(Line::styled(format!("{}{}", prefix, opt), style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "  [Esc] Cancel",
        Style::default().fg(Color::Rgb(100, 100, 100)),
    ));

    let menu_content = Paragraph::new(lines);
    frame.render_widget(menu_content, inner);
}

fn render_help_overlay(frame: &mut Frame, is_view_mode: bool, is_single_file_mode: bool) {
    let exit_text = if is_single_file_mode {
        "Quit"
    } else {
        "Back to list"
    };

    let shortcuts: Vec<(&str, &str)> = if is_view_mode {
        vec![
            ("j / ↓", "Scroll down"),
            ("k / ↑", "Scroll up"),
            ("d / Ctrl+D", "Half page down"),
            ("u / Ctrl+U", "Half page up"),
            ("g / Home", "Jump to top"),
            ("G / End", "Jump to bottom"),
            ("/", "Search"),
            ("n / N", "Next / prev match"),
            ("t", "Cycle tools: off/trunc/full"),
            ("T", "Toggle thinking"),
            ("i", "Toggle timing"),
            ("e", "Export to file"),
            ("y", "Copy to clipboard"),
            ("p", "Show file path"),
            ("Y", "Copy path"),
            ("I", "Copy session ID"),
            ("Ctrl+R", "Resume"),
            ("Ctrl+X", "Delete"),
            ("q / Esc", exit_text),
        ]
    } else {
        vec![
            ("↑ / ↓", "Move selection"),
            ("← / →", "Move cursor"),
            ("Ctrl+P / N", "Move selection"),
            ("Ctrl+D / U", "Half page down/up"),
            ("PgUp / PgDn", "Jump by page"),
            ("Home / End", "Jump to first/last"),
            ("Enter", "Open viewer"),
            ("Ctrl+O", "Select and exit"),
            ("Ctrl+W", "Delete word"),
            ("Ctrl+R", "Resume"),
            ("Ctrl+X", "Delete"),
            ("Esc", "Quit"),
        ]
    };

    let title = " Shortcuts ";

    let area = frame.area();
    // Calculate dimensions based on content (use chars().count() for Unicode)
    let max_key_len = shortcuts
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    let max_action_len = shortcuts
        .iter()
        .map(|(_, a)| a.chars().count())
        .max()
        .unwrap_or(0);
    // Padding: 2 chars left + key + " │ " (3) + action + 2 chars right
    let menu_width = (max_key_len + max_action_len + 11) as u16;
    // Height: 1 top padding + shortcuts + 1 bottom padding + 2 border
    let menu_height = shortcuts.len() as u16 + 4;

    // Center the menu
    let menu_area = Rect {
        x: (area.width.saturating_sub(menu_width)) / 2,
        y: (area.height.saturating_sub(menu_height)) / 2,
        width: menu_width,
        height: menu_height,
    };

    // Clear the area behind the modal
    frame.render_widget(Clear, menu_area);

    // Render background
    let background = Block::default().style(Style::default().bg(Color::Rgb(25, 25, 30)));
    frame.render_widget(background, menu_area);

    // Render border
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(78, 201, 176)));

    let inner = block.inner(menu_area);
    frame.render_widget(block, menu_area);

    // Build shortcut lines with padding
    let mut lines = Vec::new();
    lines.push(Line::from("")); // Top padding
    for (key, action) in &shortcuts {
        let key_padding = max_key_len - key.chars().count();
        lines.push(Line::from(vec![
            Span::raw("  "), // Left padding
            Span::styled(
                format!("{}{}", key, " ".repeat(key_padding)),
                Style::default().fg(Color::Rgb(78, 201, 176)),
            ),
            Span::styled(" │ ", Style::default().fg(Color::Rgb(60, 60, 60))),
            Span::styled(*action, Style::default().fg(Color::White)),
        ]));
    }

    let content = Paragraph::new(lines);
    frame.render_widget(content, inner);
}

fn render_list(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    // Use cached query words instead of reparsing
    let query_words: Vec<&str> = app.query_words().iter().map(|s| s.as_str()).collect();

    // Calculate visible range FIRST (before building any items)
    // When searching, items may have 4 lines (with context), so use 4 lines per item
    // to ensure the offset calculation matches the actual rendered heights
    let lines_per_item = if query_words.is_empty() {
        LINES_PER_ITEM // 3 lines: header, preview, separator
    } else {
        4 // 4 lines: header, preview, context (optional but reserve space), separator
    };
    let items_per_page = (area.height as usize) / lines_per_item;
    let offset = match (app.selected(), items_per_page) {
        (Some(sel), n) if n > 0 => (sel / n) * n,
        _ => 0,
    };
    let visible_count = items_per_page.max(1);

    // Cache separator string (same for all items in this frame)
    let separator_str = "─".repeat(width);

    // Only build ListItems for the visible range
    let visible_items: Vec<ListItem> = app
        .filtered()
        .iter()
        .skip(offset)
        .take(visible_count)
        .enumerate()
        .map(|(relative_idx, &conv_idx)| {
            let list_idx = offset + relative_idx;
            let conv = &app.conversations()[conv_idx];
            let is_selected = app.selected() == Some(list_idx);

            // Format timestamp
            let timestamp = if app.use_relative_time() {
                format_relative_time(conv.timestamp)
            } else {
                conv.timestamp.format("%b %d, %H:%M").to_string()
            };

            // Format message count
            let msg_count = if conv.message_count == 1 {
                "1 msg".to_string()
            } else {
                format!("{} msgs", conv.message_count)
            };

            // Format conversation duration (only if > 0 minutes)
            let duration = conv.duration_minutes.map(|m| {
                if m >= 60 {
                    format!("{}h {}m", m / 60, m % 60)
                } else {
                    format!("{}m", m)
                }
            });

            // Selection indicator: vertical bar for all rows (with left padding)
            let indicator = " ▌ ";
            let indicator_style = if is_selected {
                Style::default().fg(Color::Rgb(78, 201, 176))
            } else {
                Style::default().fg(Color::Rgb(60, 60, 60))
            };

            // Build left part: indicator + project + optional summary
            let project_part = conv
                .project_name
                .as_ref()
                .map(|name| name.to_string())
                .unwrap_or_default();

            // Calculate right-side length first to determine available space for summary
            let duration_len = duration
                .as_ref()
                .map(|d| d.chars().count() + 3)
                .unwrap_or(0); // 3 for " · "
            let right_len =
                msg_count.chars().count() + duration_len + 3 + timestamp.chars().count(); // 3 for " · "
            let indicator_len = indicator.chars().count();
            let project_len = project_part.chars().count();
            let min_padding = 2; // Minimum padding between content and timestamp

            // Calculate available width for summary (filter empty summaries)
            let available_for_summary =
                width.saturating_sub(indicator_len + project_len + right_len + min_padding + 4); // 4 for " · " prefix and ellipsis

            // Build summary part (dimmer, dynamically truncated based on available space)
            let summary_part = conv
                .summary
                .as_ref()
                .filter(|s| !s.is_empty() && available_for_summary > 5)
                .map(|s| {
                    let summary_chars = s.chars().count();
                    if summary_chars > available_for_summary {
                        format!(
                            " · {}…",
                            s.chars()
                                .take(available_for_summary.saturating_sub(1))
                                .collect::<String>()
                        )
                    } else {
                        format!(" · {}", s)
                    }
                });

            // Calculate padding for right-aligned timestamp + message count
            let left_len = indicator_len
                + project_len
                + summary_part
                    .as_ref()
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
            let padding = width.saturating_sub(left_len + right_len + 1);

            // Header line: ▌ project-name · summary                    timestamp
            let project_style = if is_selected {
                Style::default().fg(Color::White).bold()
            } else {
                Style::default().fg(Color::White)
            };

            let summary_style = Style::default().fg(Color::Rgb(140, 155, 175)); // Soft slate blue
            let summary_highlight_style = Style::default().fg(Color::Rgb(180, 195, 215)); // Lighter slate blue for highlights

            // Highlight style: cyan with bold for selected row
            let highlight_style = if is_selected {
                Style::default().fg(Color::Rgb(78, 201, 176)).bold()
            } else {
                Style::default().fg(Color::Rgb(78, 201, 176))
            };

            let selection_bg = if is_selected {
                Style::default().bg(Color::Rgb(45, 45, 55))
            } else {
                Style::default()
            };

            // Build header with provider badge and highlighted project name
            let mut header_spans = vec![Span::styled(indicator, indicator_style)];
            header_spans.push(provider_badge(conv.provider.clone()));
            header_spans.extend(highlight_text(
                &project_part,
                &query_words,
                project_style,
                highlight_style,
            ));

            // Add summary if present (with search highlighting)
            if let Some(ref summary) = summary_part {
                header_spans.extend(highlight_text(
                    summary,
                    &query_words,
                    summary_style,
                    summary_highlight_style,
                ));
            }

            header_spans.push(Span::raw(" ".repeat(padding)));
            header_spans.push(Span::styled(
                msg_count,
                Style::default().fg(Color::Rgb(110, 110, 110)),
            ));
            // Add conversation duration if present
            if let Some(ref d) = duration {
                header_spans.push(Span::styled(
                    " · ",
                    Style::default().fg(Color::Rgb(70, 70, 70)),
                ));
                header_spans.push(Span::styled(
                    d.clone(),
                    Style::default().fg(Color::Rgb(100, 140, 130)),
                ));
            }
            header_spans.push(Span::styled(
                " · ",
                Style::default().fg(Color::Rgb(70, 70, 70)),
            ));
            header_spans.push(Span::styled(
                timestamp,
                Style::default().fg(Color::Rgb(140, 140, 140)),
            ));

            let header = Line::from(header_spans).style(selection_bg);

            // Preview line: sanitized and truncated
            let preview_text = sanitize_preview(&conv.preview);
            let max_preview_len = width.saturating_sub(4);
            let truncated_preview = if preview_text.chars().count() > max_preview_len {
                let truncated: String = preview_text
                    .chars()
                    .take(max_preview_len.saturating_sub(1))
                    .collect();
                format!("{}…", truncated)
            } else {
                preview_text
            };

            // Build preview with highlighted matches
            let preview_style = Style::default().fg(Color::Rgb(130, 130, 130));
            let mut preview_spans = vec![Span::styled(indicator, indicator_style)];
            preview_spans.extend(highlight_text(
                &truncated_preview,
                &query_words,
                preview_style,
                highlight_style,
            ));

            let preview = Line::from(preview_spans).style(selection_bg);

            // Check for hidden matches and build context line if needed
            let context_line = if !query_words.is_empty() {
                if let Some((match_pos, match_char_len)) =
                    find_hidden_match(&conv.full_text, &truncated_preview, &query_words)
                {
                    let context_width = width.saturating_sub(4); // Account for indicator
                    let context_text = extract_match_context(
                        &conv.full_text,
                        match_pos,
                        match_char_len,
                        context_width,
                    );

                    // Truncate context if still too long
                    let truncated_context = if context_text.chars().count() > context_width {
                        let truncated: String = context_text
                            .chars()
                            .take(context_width.saturating_sub(1))
                            .collect();
                        format!("{}…", truncated)
                    } else {
                        context_text
                    };

                    // Build context line with highlighting (dimmer style)
                    let context_base_style = Style::default().fg(Color::Rgb(100, 100, 100));
                    let context_highlight_style = Style::default().fg(Color::Rgb(60, 160, 140)); // Dimmer cyan

                    let mut context_spans = vec![Span::styled(indicator, indicator_style)];
                    context_spans.extend(highlight_text(
                        &truncated_context,
                        &query_words,
                        context_base_style,
                        context_highlight_style,
                    ));

                    Some(Line::from(context_spans).style(selection_bg))
                } else {
                    None
                }
            } else {
                None
            };

            // Separator line: dim horizontal rule (full width)
            let separator = Line::from(Span::styled(
                separator_str.as_str(),
                Style::default().fg(Color::Rgb(50, 50, 50)),
            ));

            // Combine into item (3 or 4 lines depending on context)
            let lines = if let Some(ctx) = context_line {
                vec![header, preview, ctx, separator]
            } else {
                vec![header, preview, separator]
            };

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(visible_items);
    frame.render_widget(list, area);
}

fn format_relative_time(timestamp: DateTime<Local>) -> String {
    let delta = timestamp.signed_duration_since(Local::now());
    HumanTime::from(delta).to_text_en(Accuracy::Rough, Tense::Present)
}

/// Sanitize preview text by removing XML-like tags and normalizing whitespace
fn sanitize_preview(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut last_was_space = false;

    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            '\n' | '\r' | '\t' => {
                if !last_was_space {
                    result.push(' ');
                    last_was_space = true;
                }
            }
            ' ' => {
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

/// Split text into spans with matched portions highlighted (case-insensitive)
fn highlight_text(
    text: &str,
    query_words: &[&str],
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    if query_words.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    // Collect chars for iteration (original text only)
    let chars: Vec<char> = text.chars().collect();

    // Build char-to-byte mapping
    let char_to_byte: Vec<usize> = text
        .char_indices()
        .map(|(byte_idx, _)| byte_idx)
        .chain(std::iter::once(text.len()))
        .collect();

    let mut spans = Vec::new();
    let mut last_end = 0;
    let mut char_idx = 0;

    while char_idx < chars.len() {
        // Check if we're at a word start
        let at_word_start = if char_idx == 0 {
            !is_word_separator(chars[char_idx])
        } else {
            is_word_separator(chars[char_idx - 1]) && !is_word_separator(chars[char_idx])
        };

        if at_word_start {
            // Find word end
            let word_start = char_idx;
            let mut word_end = char_idx;
            while word_end < chars.len() && !is_word_separator(chars[word_end]) {
                word_end += 1;
            }

            // Extract original word and lowercase it for comparison
            // This avoids Unicode index mismatch issues from to_lowercase changing char count
            let start_byte = char_to_byte[word_start];
            let end_byte = char_to_byte[word_end];
            let original_word = &text[start_byte..end_byte];
            let word_lower = original_word.to_lowercase();

            // Check if any query word is a prefix of this word
            let matched_query = query_words.iter().find(|&&qw| word_lower.starts_with(qw));

            if let Some(qw) = matched_query {
                // Add non-highlighted text before this word
                if word_start > last_end {
                    let prev_start_byte = char_to_byte[last_end];
                    spans.push(Span::styled(
                        text[prev_start_byte..start_byte].to_string(),
                        base_style,
                    ));
                }

                // Highlight the matched prefix portion
                // Use char count from original word to handle Unicode correctly
                let prefix_len = qw.chars().count();
                let highlight_end = (word_start + prefix_len).min(word_end);
                let highlight_end_byte = char_to_byte[highlight_end];
                spans.push(Span::styled(
                    text[start_byte..highlight_end_byte].to_string(),
                    highlight_style,
                ));

                // Add the rest of the word (unhighlighted)
                if highlight_end < word_end {
                    spans.push(Span::styled(
                        text[highlight_end_byte..end_byte].to_string(),
                        base_style,
                    ));
                }

                last_end = word_end;
            }
            char_idx = word_end;
        } else {
            char_idx += 1;
        }
    }

    // Add remaining text
    if last_end < chars.len() {
        let start_byte = char_to_byte[last_end];
        spans.push(Span::styled(text[start_byte..].to_string(), base_style));
    }

    if spans.is_empty() {
        vec![Span::styled(text.to_string(), base_style)]
    } else {
        spans
    }
}

/// Find the first match in full_text that is NOT visible in the preview.
/// Returns (byte_offset, matched_word_char_len) or None if all matches are visible.
fn find_hidden_match(
    full_text: &str,
    preview: &str,
    query_words: &[&str],
) -> Option<(usize, usize)> {
    if query_words.is_empty() {
        return None;
    }

    // Count word prefix matches in text using single-pass iteration
    // Uses original text chars and lowercases words individually to avoid Unicode index issues
    let count_word_matches = |text: &str| -> usize {
        let chars: Vec<char> = text.chars().collect();
        let char_to_byte: Vec<usize> = text
            .char_indices()
            .map(|(byte_idx, _)| byte_idx)
            .chain(std::iter::once(text.len()))
            .collect();

        let mut count = 0;
        let mut prev_sep = true;

        for (i, &c) in chars.iter().enumerate() {
            let is_sep = is_word_separator(c);
            if !is_sep && prev_sep {
                // Word start - find word end
                let word_end = chars[i..]
                    .iter()
                    .position(|&c| is_word_separator(c))
                    .map(|p| i + p)
                    .unwrap_or(chars.len());

                // Extract and lowercase word
                let start_byte = char_to_byte[i];
                let end_byte = char_to_byte[word_end];
                let word = &text[start_byte..end_byte];
                let word_lower = word.to_lowercase();

                if query_words.iter().any(|&qw| word_lower.starts_with(qw)) {
                    count += 1;
                }
            }
            prev_sep = is_sep;
        }
        count
    };

    let preview_matches = count_word_matches(preview);
    let full_matches = count_word_matches(full_text);

    if full_matches <= preview_matches {
        return None;
    }

    // Find the (preview_matches + 1)th match in full_text using single-pass
    // Use original text chars to avoid Unicode index mismatch
    let chars: Vec<char> = full_text.chars().collect();

    // Build char-to-byte mapping from original text
    let char_to_byte: Vec<usize> = full_text
        .char_indices()
        .map(|(byte_idx, _)| byte_idx)
        .chain(std::iter::once(full_text.len()))
        .collect();

    let mut match_count = 0;
    let mut prev_sep = true;

    for (i, &c) in chars.iter().enumerate() {
        let is_sep = is_word_separator(c);
        if !is_sep && prev_sep {
            // Word start
            let word_end = chars[i..]
                .iter()
                .position(|&c| is_word_separator(c))
                .map(|p| i + p)
                .unwrap_or(chars.len());

            // Extract and lowercase word
            let start_byte = char_to_byte[i];
            let end_byte = char_to_byte[word_end];
            let word = &full_text[start_byte..end_byte];
            let word_lower = word.to_lowercase();

            if let Some(&qw) = query_words.iter().find(|&&qw| word_lower.starts_with(qw)) {
                match_count += 1;
                if match_count > preview_matches {
                    // Return byte offset and the matched prefix length
                    return Some((start_byte, qw.chars().count()));
                }
            }
        }
        prev_sep = is_sep;
    }

    None
}

/// Extract a context snippet around a match position in full_text.
/// Returns a sanitized string with ellipsis prefix, suitable for display.
fn extract_match_context(
    full_text: &str,
    match_pos: usize,
    query_char_len: usize,
    max_width: usize,
) -> String {
    // Calculate how much context to show around the match
    // Reserve space for ellipsis prefix/suffix (2 chars each)
    let context_chars = max_width.saturating_sub(query_char_len).saturating_sub(4) / 2;

    // Find char boundaries around match_pos efficiently without collecting all chars
    // Go back ~context_chars characters from match_pos
    let start_byte = full_text[..match_pos.min(full_text.len())]
        .char_indices()
        .rev()
        .nth(context_chars)
        .map(|(i, _)| i)
        .unwrap_or(0);

    // Go forward ~(query_char_len + context_chars) characters from match_pos
    let end_byte = full_text[match_pos.min(full_text.len())..]
        .char_indices()
        .nth(query_char_len + context_chars)
        .map(|(i, _)| match_pos + i)
        .unwrap_or(full_text.len());

    let snippet = &full_text[start_byte..end_byte.min(full_text.len())];

    // Sanitize the snippet (remove XML tags, normalize whitespace)
    let sanitized = sanitize_preview(snippet);

    // Add ellipsis prefix/suffix to indicate this is from elsewhere
    let prefix = if start_byte > 0 { "…" } else { "" };
    let suffix = if end_byte < full_text.len() {
        "…"
    } else {
        ""
    };

    format!("{}{}{}", prefix, sanitized, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_model_name_opus_45() {
        assert_eq!(format_model_name("claude-opus-4-5-20251101"), "opus-4.5");
    }

    #[test]
    fn test_format_model_name_sonnet_4() {
        assert_eq!(format_model_name("claude-sonnet-4-20250514"), "sonnet-4");
    }

    #[test]
    fn test_format_model_name_sonnet_35() {
        assert_eq!(
            format_model_name("claude-3-5-sonnet-20241022"),
            "sonnet-3.5"
        );
    }

    #[test]
    fn test_format_model_name_haiku_35() {
        assert_eq!(format_model_name("claude-3-5-haiku-20241022"), "haiku-3.5");
    }

    #[test]
    fn test_format_model_name_opus_3() {
        assert_eq!(format_model_name("claude-3-opus-20240229"), "opus-3");
    }

    #[test]
    fn test_format_model_name_unknown() {
        assert_eq!(format_model_name("custom-model"), "custom-model");
    }

    #[test]
    fn test_format_model_name_truncates_long() {
        let long_name = "very-long-unknown-model-name-that-exceeds-limit";
        let formatted = format_model_name(long_name);
        // 19 chars + ellipsis (3 bytes in UTF-8)
        assert!(formatted.chars().count() <= 20);
        assert!(formatted.ends_with('…'));
    }

    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(1000), "1k");
        assert_eq!(format_tokens(417000), "417k");
        assert_eq!(format_tokens(999999), "999k");
    }

    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(1_500_000), "1.5M");
        assert_eq!(format_tokens(12_345_678), "12.3M");
    }

    #[test]
    fn test_format_tokens_long() {
        assert_eq!(format_tokens_long(500), "500 tokens");
        assert_eq!(format_tokens_long(1000), "1k tokens");
        assert_eq!(format_tokens_long(926000), "926k tokens");
        assert_eq!(format_tokens_long(1_500_000), "1.5M tokens");
    }
}
