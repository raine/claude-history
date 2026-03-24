use crate::config::KeyBindings;
use crate::tui::app::{
    App, AppMode, DialogMode, LineStyle, LoadingState, RenderedLine, ViewSearchMode, ViewState,
};
use crate::tui::search::normalize_for_search;
use crate::tui::theme::{self, Theme};
use chrono::{DateTime, Local};
use ratatui::layout::Position;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::UnicodeWidthChar;

/// Get the current theme
fn th() -> &'static Theme {
    theme::detect_theme()
}

/// Convert theme RGB tuple to ratatui Color
fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

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
        .border_style(Style::default().fg(rgb(th().border)));
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
        render_help_overlay(frame, false, false, app.keys());
    }
}

fn render_status_message(frame: &mut Frame, msg: &str, area: Rect) {
    let status_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(msg, Style::default().fg(Color::Yellow)),
    ]);
    let status = Paragraph::new(status_line).style(Style::default().bg(rgb(th().status_bar_bg)));
    frame.render_widget(status, area);
}

fn render_list_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let is_loading = app.is_loading();

    let key_style = Style::default().fg(rgb(th().accent));
    let label_style = Style::default().fg(rgb(th().text_muted));
    // Dimmed styles for unavailable shortcuts during loading
    let dim_key_style = Style::default().fg(rgb(th().dim_key));
    let dim_label_style = Style::default().fg(rgb(th().dim_label));

    let (action_key, action_label) = if is_loading {
        (dim_key_style, dim_label_style)
    } else {
        (key_style, label_style)
    };

    let keys = app.keys();
    let mut spans = vec![
        Span::raw("  "),
        Span::styled("Enter", action_key),
        Span::styled(" open  ", action_label),
        Span::styled(keys.resume.short_label(), action_key),
        Span::styled(" resume  ", action_label),
        Span::styled(keys.fork.short_label(), action_key),
        Span::styled(" fork  ", action_label),
        Span::styled(keys.resume_here.short_label(), action_key),
        Span::styled(" here  ", action_label),
        Span::styled(keys.delete.short_label(), action_key),
        Span::styled(" delete  ", action_label),
    ];

    // Scope toggle (only when project context exists)
    if app.has_project_context() {
        let scope_label = if app.workspace_filter() { "Prj" } else { "All" };
        let scope_val_style = if app.workspace_filter() {
            Style::default().fg(rgb(th().accent)).bold()
        } else {
            label_style
        };
        spans.extend([
            Span::styled("Tab", key_style),
            Span::styled("\u{b7}", label_style),
            Span::styled(scope_label, scope_val_style),
            Span::raw("  "),
        ]);
    }

    spans.extend([
        Span::styled("?", key_style),
        Span::styled("help  ", label_style),
        Span::styled("Esc", key_style),
        Span::styled(" quit", label_style),
    ]);

    let status_line = Line::from(spans);
    let status = Paragraph::new(status_line).style(Style::default().bg(rgb(th().status_bar_bg)));
    frame.render_widget(status, area);
}

/// Check if the header (with summary) fits on a single line given terminal width
fn header_fits_single_line(conv: &crate::history::Conversation, terminal_width: u16) -> bool {
    let summary = match &conv.summary {
        Some(s) => s,
        None => return true, // No summary means it's already single line
    };

    let project = conv.project_name.as_deref().unwrap_or("Unknown");

    // Calculate custom title length if present
    let custom_title_len = conv
        .custom_title
        .as_ref()
        .map(|t| t.chars().count() + 3) // + " · "
        .unwrap_or(0);

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

    // Format: "  project · custom_title · model · msg_count · duration · tokens · timestamp · summary"
    let total_len = 2
        + project.len()
        + 3
        + custom_title_len
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
        DialogMode::Help => render_help_overlay(frame, true, app.is_single_file_mode(), app.keys()),
        DialogMode::None => {}
    }
}

fn render_view_header(frame: &mut Frame, app: &App, state: &ViewState, area: Rect) {
    // Find the conversation by path (works for both list and single file mode)
    let conv = app
        .conversations()
        .iter()
        .find(|c| c.path == state.conversation_path);

    let (
        project,
        custom_title,
        model,
        msg_count,
        duration,
        tokens,
        timestamp,
        summary,
        fits_single,
    ) = if let Some(conv) = conv {
        let project = conv.project_name.as_deref().unwrap_or("Unknown");
        let custom_title = conv.custom_title.clone();
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
        let custom_title_len = custom_title
            .as_ref()
            .map(|t| t.chars().count() + 3)
            .unwrap_or(0); // + " · "
        let model_len = model.as_ref().map(|m| m.len() + 3).unwrap_or(0); // + " · "
        let duration_len = duration.as_ref().map(|d| d.len() + 3).unwrap_or(0); // + " · "
        let base_len = 2
            + project.len()
            + 3
            + custom_title_len
            + model_len
            + msg_count.len()
            + duration_len
            + 3
            + 16; // 16 = timestamp

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
            custom_title,
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
    let build_metadata_spans = |include_summary: bool| {
        let mut spans = vec![
            Span::raw("  "),
            Span::styled(
                project.clone(),
                Style::default().fg(rgb(th().accent)).bold(),
            ),
        ];

        // Add custom title if present
        if let Some(ref t) = custom_title {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                t.clone(),
                Style::default().fg(rgb(th().custom_title)), // Warm gold
            ));
        }

        // Add model if present
        if let Some(ref m) = model {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                m.clone(),
                Style::default().fg(rgb(th().model_color)),
            ));
        }

        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            msg_count.clone(),
            Style::default().fg(rgb(th().text_secondary)),
        ));

        // Add conversation duration if present
        if let Some(ref d) = duration {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                d.clone(),
                Style::default().fg(rgb(th().duration_color)),
            ));
        }

        // Add tokens if present
        if let Some(ref t) = tokens {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                t.clone(),
                Style::default().fg(rgb(th().text_secondary)),
            ));
        }

        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            timestamp.clone(),
            Style::default().fg(rgb(th().text_secondary)),
        ));

        // Add summary if requested
        if include_summary && let Some(ref s) = summary {
            spans.push(Span::raw(" · "));
            spans.push(Span::styled(
                s.clone(),
                Style::default().fg(rgb(th().header_summary)),
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
                Span::styled(summary_text, Style::default().fg(rgb(th().header_summary))),
            ]));
        }
        lines
    };

    let header = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(rgb(th().border))),
    );

    frame.render_widget(header, area);
}

fn render_view_content(frame: &mut Frame, state: &ViewState, area: Rect) {
    let visible_height = area.height as usize;
    let query_lower = state.search_query.to_lowercase();

    // Determine focused message line range (only when nav mode active)
    let focused_range = if state.message_nav_active {
        state
            .focused_message
            .and_then(|idx| state.message_ranges.get(idx))
            .map(|m| m.start_line..m.end_line)
    } else {
        None
    };

    let visible_lines: Vec<Line> = state
        .rendered_lines
        .iter()
        .enumerate()
        .skip(state.scroll_offset)
        .take(visible_height)
        .map(|(line_idx, rendered)| {
            let is_current_match = state.search_matches.get(state.current_match) == Some(&line_idx);
            let has_match = !query_lower.is_empty() && state.search_matches.contains(&line_idx);

            let is_focused = focused_range
                .as_ref()
                .is_some_and(|r| r.contains(&line_idx));

            // Gutter indicator (only shown in message nav mode)
            let gutter = if state.message_nav_active {
                if is_focused {
                    Span::styled("▌ ", Style::default().fg(rgb(th().accent)))
                } else {
                    Span::raw("  ")
                }
            } else {
                Span::raw("")
            };

            let mut spans: Vec<Span> = vec![gutter];

            if has_match && !query_lower.is_empty() {
                spans.extend(highlight_line_matches(
                    rendered,
                    &query_lower,
                    is_current_match,
                ));
            } else {
                spans.extend(
                    rendered
                        .spans
                        .iter()
                        .map(|(text, style)| styled_span(text, style)),
                );
            }

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
        let status =
            Paragraph::new(status_line).style(Style::default().bg(rgb(th().status_bar_bg)));
        frame.render_widget(status, area);
        return;
    }

    // Fixed-width scroll position to prevent bar from jumping
    // Use minimum width of 4 for both numbers to handle most conversations
    let total = state.total_lines.max(1);
    let width = total.to_string().len().max(4);
    let scroll_pos = format!("[{:>width$}/{:<width$}]", state.scroll_offset + 1, total);

    let key_style = Style::default().fg(rgb(th().accent));
    let label_style = Style::default().fg(rgb(th().text_muted));

    // Fixed-width status labels to prevent jumping when toggling
    let tools_status = state.tool_display.status_label();
    let thinking_status = if state.show_thinking { "on " } else { "off" };
    let timing_status = if state.show_timing { "on " } else { "off" };

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(scroll_pos, Style::default().fg(rgb(th().text_secondary))),
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
            Span::styled(app.keys().resume.short_label(), key_style),
            Span::styled(" resume  ", label_style),
            Span::styled(app.keys().fork.short_label(), key_style),
            Span::styled(" fork  ", label_style),
            Span::styled(app.keys().resume_here.short_label(), key_style),
            Span::styled(" here  ", label_style),
            Span::styled(app.keys().delete.short_label(), key_style),
            Span::styled(" del  ", label_style),
            Span::styled("q", key_style),
            Span::styled("uit", label_style),
        ]);
    }

    let status_line = Line::from(spans);
    let status = Paragraph::new(status_line).style(Style::default().bg(rgb(th().status_bar_bg)));
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
        Span::styled(
            &state.search_query,
            Style::default().fg(rgb(th().text_primary)),
        ),
        Span::styled(match_info, Style::default().fg(rgb(th().text_secondary))),
    ]);

    let input = Paragraph::new(input_line).style(Style::default().bg(rgb(th().status_bar_bg)));
    frame.render_widget(input, area);

    // Position cursor (account for "  /" prefix = 3 columns)
    let query_width: usize = state
        .search_query
        .chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum();
    let max_x = area.x + area.width.saturating_sub(1);
    let cursor_x = (area.x + 3 + query_width.min(u16::MAX as usize) as u16).min(max_x);
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
            .bg(rgb(th().search_match_bg))
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
        s = s.fg(rgb(th().text_muted));
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
            format!("{}/{}", app.filtered().len(), app.scoped_count())
        }
    };

    // Build search line with optional "Project" prefix when workspace filter is active
    let query = app.query();
    let prompt_style = Style::default().fg(rgb(th().accent));

    // " Project ❯ " when filtering, " ❯ " otherwise
    let (prompt_spans, prefix_width) = if app.workspace_filter() {
        (
            vec![
                Span::raw(" "),
                Span::styled("Project", Style::default().fg(rgb(th().text_muted))),
                Span::raw(" "),
                Span::styled("\u{276F} ", prompt_style),
            ],
            11, // " Project ❯ " = 11 columns
        )
    } else {
        (
            vec![Span::raw(" "), Span::styled("\u{276F} ", prompt_style)],
            3, // " ❯ " = 3 columns
        )
    };

    let left_width = prefix_width
        + query
            .chars()
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum::<usize>();
    let right_len = status_text.chars().count() + 1; // status + trailing space
    let padding = (area.width as usize).saturating_sub(left_width + right_len + 1);

    let status_style = if app.is_loading() {
        Style::default().fg(rgb(th().accent))
    } else {
        Style::default().fg(rgb(th().text_muted))
    };

    let mut spans = prompt_spans;
    spans.extend([
        Span::raw(query.to_string()),
        Span::raw(" ".repeat(padding)),
        Span::styled(status_text, status_style),
        Span::raw(" "),
    ]);
    let search_line = Line::from(spans);

    let input = Paragraph::new(search_line).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(rgb(th().border))),
    );

    frame.render_widget(input, area);

    // Position cursor at cursor_pos (account for " ❯ " prefix = 3 columns)
    if area.width > prefix_width as u16 {
        let cursor_offset: u16 = app
            .query()
            .chars()
            .take(app.cursor_pos())
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum::<usize>()
            .min(u16::MAX as usize) as u16;
        let max_x = area.x + area.width.saturating_sub(2);
        let cursor_x = (area.x + prefix_width as u16)
            .saturating_add(cursor_offset)
            .min(max_x);
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
        Span::styled("(y/n)", Style::default().fg(rgb(th().text_secondary))),
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
    let background = Block::default().style(Style::default().bg(rgb(th().overlay_bg)));
    frame.render_widget(background, menu_area);

    // Render border
    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(th().accent)));

    let inner = block.inner(menu_area);
    frame.render_widget(block, menu_area);

    // Render options
    let mut lines = Vec::new();
    for (i, opt) in options.iter().enumerate() {
        let style = if i == selected {
            Style::default().fg(rgb(th().accent)).bold()
        } else {
            Style::default().fg(rgb(th().text_primary))
        };
        let prefix = if i == selected { "▶ " } else { "  " };
        lines.push(Line::styled(format!("{}{}", prefix, opt), style));
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "  [Esc] Cancel",
        Style::default().fg(rgb(th().text_muted)),
    ));

    let menu_content = Paragraph::new(lines);
    frame.render_widget(menu_content, inner);
}

fn render_help_overlay(
    frame: &mut Frame,
    is_view_mode: bool,
    is_single_file_mode: bool,
    keys: &KeyBindings,
) {
    let exit_text = if is_single_file_mode {
        "Quit"
    } else {
        "Back to list"
    };

    let shortcuts: Vec<(String, &str)> = if is_view_mode {
        vec![
            ("j / ↓".into(), "Scroll down"),
            ("k / ↑".into(), "Scroll up"),
            ("J / ]".into(), "Next message"),
            ("K / [".into(), "Previous message"),
            ("d / Ctrl+D".into(), "Half page down"),
            ("u / Ctrl+U".into(), "Half page up"),
            ("g / Home".into(), "Jump to top"),
            ("G / End".into(), "Jump to bottom"),
            ("/".into(), "Search"),
            ("n / N".into(), "Next / prev match"),
            ("t".into(), "Cycle tools: off/trunc/full"),
            ("T".into(), "Toggle thinking"),
            ("i".into(), "Toggle timing"),
            ("e".into(), "Export to file"),
            ("y".into(), "Copy to clipboard / message"),
            ("p".into(), "Show file path"),
            ("Y".into(), "Copy path"),
            ("I".into(), "Copy session ID"),
            (keys.resume.help_label(), "Resume"),
            (keys.fork.help_label(), "Fork resume"),
            (keys.resume_here.help_label(), "Resume here (CWD)"),
            (keys.delete.help_label(), "Delete"),
            ("q / Esc".into(), exit_text),
        ]
    } else {
        vec![
            ("↑ / ↓".into(), "Move selection"),
            ("← / →".into(), "Move cursor"),
            ("Ctrl+P / N".into(), "Move selection"),
            ("Ctrl+D".into(), "Half page down"),
            ("Ctrl+U".into(), "Kill to start of line"),
            ("Ctrl+K".into(), "Kill to end of line"),
            ("PgUp / PgDn".into(), "Jump by page"),
            ("Home / End".into(), "Jump to first/last"),
            ("Tab".into(), "Toggle scope (All/Project)"),
            ("Enter".into(), "Open viewer"),
            ("Ctrl+O".into(), "Select and exit"),
            ("Ctrl+W".into(), "Delete word"),
            (keys.resume.help_label(), "Resume"),
            (keys.fork.help_label(), "Fork resume"),
            (keys.resume_here.help_label(), "Resume here (CWD)"),
            (keys.delete.help_label(), "Delete"),
            ("Esc".into(), "Quit"),
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
    let background = Block::default().style(Style::default().bg(rgb(th().overlay_bg)));
    frame.render_widget(background, menu_area);

    // Render border
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(th().accent)));

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
                Style::default().fg(rgb(th().accent)),
            ),
            Span::styled(" │ ", Style::default().fg(rgb(th().border))),
            Span::styled(
                action.to_string(),
                Style::default().fg(rgb(th().text_primary)),
            ),
        ]));
    }

    let content = Paragraph::new(lines);
    frame.render_widget(content, inner);
}

fn render_list(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let query_normalized: String = normalize_for_search(app.query().trim())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // Calculate visible range FIRST (before building any items)
    // When searching, items may have 4 lines (with context), so use 4 lines per item
    // to ensure the offset calculation matches the actual rendered heights
    let lines_per_item = if query_normalized.is_empty() {
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

    // Compute now once for consistent relative timestamps across all visible items
    let now = Local::now();

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

            // Format timestamp (hybrid: relative for recent, absolute for older)
            let (timestamp, recency) = format_timestamp(conv.timestamp, now);

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
                Style::default().fg(rgb(th().accent))
            } else {
                Style::default().fg(rgb(th().border))
            };

            // Build left part: indicator + project + optional custom title + optional summary
            let project_part = conv
                .project_name
                .as_ref()
                .map(|name| name.to_string())
                .unwrap_or_default();

            // Build custom title part (truncated if very long)
            let custom_title_part = conv
                .custom_title
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| {
                    let max_title = 40;
                    if s.chars().count() > max_title {
                        format!(" · {}…", s.chars().take(max_title - 1).collect::<String>())
                    } else {
                        format!(" · {}", s)
                    }
                });

            // Calculate right-side length first to determine available space for summary
            let duration_len = duration
                .as_ref()
                .map(|d| d.chars().count() + 3)
                .unwrap_or(0); // 3 for " · "
            let right_len =
                msg_count.chars().count() + duration_len + 3 + timestamp.chars().count(); // 3 for " · "
            let indicator_len = indicator.chars().count();
            let project_len = project_part.chars().count();
            let custom_title_len = custom_title_part
                .as_ref()
                .map(|s| s.chars().count())
                .unwrap_or(0);
            let min_padding = 2; // Minimum padding between content and timestamp

            // Calculate available width for summary (filter empty summaries)
            let available_for_summary = width.saturating_sub(
                indicator_len + project_len + custom_title_len + right_len + min_padding + 4,
            ); // 4 for " · " prefix and ellipsis

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
                + custom_title_len
                + summary_part
                    .as_ref()
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
            let padding = width.saturating_sub(left_len + right_len + 1);

            // Header line: ▌ project-name · summary                    timestamp
            let project_style = if is_selected {
                Style::default().fg(rgb(th().text_primary)).bold()
            } else {
                Style::default().fg(rgb(th().text_primary))
            };

            let summary_style = Style::default().fg(rgb(th().summary)); // Soft slate blue
            let summary_highlight_style = Style::default().fg(rgb(th().summary_highlight)); // Lighter slate blue for highlights

            // Highlight style: cyan with bold for selected row
            let highlight_style = if is_selected {
                Style::default().fg(rgb(th().accent)).bold()
            } else {
                Style::default().fg(rgb(th().accent))
            };

            let selection_bg = if is_selected {
                Style::default().bg(rgb(th().selection_bg))
            } else {
                Style::default()
            };

            let custom_title_style = Style::default().fg(rgb(th().custom_title)); // Warm gold
            let custom_title_highlight_style =
                Style::default().fg(rgb(th().custom_title_highlight)); // Lighter gold for highlights

            // Build header with highlighted project name
            let mut header_spans = vec![Span::styled(indicator, indicator_style)];
            header_spans.extend(highlight_text(
                &project_part,
                &query_normalized,
                project_style,
                highlight_style,
            ));

            // Add custom title if present (with search highlighting)
            if let Some(ref title) = custom_title_part {
                header_spans.extend(highlight_text(
                    title,
                    &query_normalized,
                    custom_title_style,
                    custom_title_highlight_style,
                ));
            }

            // Add summary if present (with search highlighting)
            if let Some(ref summary) = summary_part {
                header_spans.extend(highlight_text(
                    summary,
                    &query_normalized,
                    summary_style,
                    summary_highlight_style,
                ));
            }

            header_spans.push(Span::raw(" ".repeat(padding)));
            header_spans.push(Span::styled(
                msg_count,
                Style::default().fg(rgb(th().msg_count)),
            ));
            // Add conversation duration if present
            if let Some(ref d) = duration {
                header_spans.push(Span::styled(
                    " · ",
                    Style::default().fg(rgb(th().dot_separator)),
                ));
                header_spans.push(Span::styled(
                    d.clone(),
                    Style::default().fg(rgb(th().duration_color)),
                ));
            }
            header_spans.push(Span::styled(
                " · ",
                Style::default().fg(rgb(th().dot_separator)),
            ));
            let timestamp_color = match recency {
                Recency::Now => th().timestamp_now,
                Recency::Minutes => th().timestamp_minutes,
                Recency::Hours => th().timestamp_hours,
                Recency::Days => th().timestamp_days,
                Recency::Old => th().text_secondary,
            };
            header_spans.push(Span::styled(
                timestamp,
                Style::default().fg(rgb(timestamp_color)),
            ));

            let header = Line::from(header_spans).style(selection_bg);

            // Preview line: sanitized, with multi-segment match display when searching
            let preview_text = sanitize_preview(&conv.preview);
            let max_preview_len = width.saturating_sub(4);
            let truncated_preview = if query_normalized.is_empty() {
                simple_truncate(&preview_text, max_preview_len)
            } else {
                build_match_segments(&preview_text, &query_normalized, max_preview_len)
            };

            // Build preview with highlighted matches
            let preview_style = Style::default().fg(rgb(th().preview));
            let mut preview_spans = vec![Span::styled(indicator, indicator_style)];
            preview_spans.extend(highlight_text(
                &truncated_preview,
                &query_normalized,
                preview_style,
                highlight_style,
            ));

            let preview = Line::from(preview_spans).style(selection_bg);

            // Check for hidden matches and build context line if needed
            let context_line = if !query_normalized.is_empty() {
                let context_width = width.saturating_sub(4);
                build_context_segments(
                    &conv.full_text,
                    &truncated_preview,
                    &query_normalized,
                    context_width,
                )
                .map(|context_text| {
                    let context_base_style = Style::default().fg(rgb(th().context_base));
                    let context_highlight_style = Style::default().fg(rgb(th().context_highlight));

                    let mut context_spans = vec![Span::styled(indicator, indicator_style)];
                    context_spans.extend(highlight_text(
                        &context_text,
                        &query_normalized,
                        context_base_style,
                        context_highlight_style,
                    ));

                    Line::from(context_spans).style(selection_bg)
                })
            } else {
                None
            };

            // Separator line: dim horizontal rule (full width)
            let separator = Line::from(Span::styled(
                separator_str.as_str(),
                Style::default().fg(rgb(th().separator)),
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

/// Recency level for timestamp color grading
enum Recency {
    Now,
    Minutes,
    Hours,
    Days,
    Old,
}

/// Format a timestamp as relative time for recent entries, absolute for older ones.
/// Returns (formatted_string, recency) for color grading.
fn format_timestamp(timestamp: DateTime<Local>, now: DateTime<Local>) -> (String, Recency) {
    let age = now.signed_duration_since(timestamp);

    // Future timestamps (clock skew): show absolute
    if age.num_seconds() < 0 {
        return (timestamp.format("%b %d, %H:%M").to_string(), Recency::Old);
    }

    let seconds = age.num_seconds();
    let minutes = age.num_minutes();
    let hours = age.num_hours();

    if seconds < 60 {
        return ("just now".to_string(), Recency::Now);
    }
    if minutes < 60 {
        return (format!("{minutes} min ago"), Recency::Minutes);
    }
    if hours < 24 {
        return (
            format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" }),
            Recency::Hours,
        );
    }

    // Use calendar day difference for "yesterday" accuracy
    let day_diff = now
        .date_naive()
        .signed_duration_since(timestamp.date_naive())
        .num_days();
    if day_diff == 1 {
        return ("yesterday".to_string(), Recency::Days);
    }
    if day_diff < 7 {
        return (format!("{day_diff} days ago"), Recency::Days);
    }

    (timestamp.format("%b %d, %H:%M").to_string(), Recency::Old)
}

/// Truncate text to max_width chars, adding "…" suffix if truncated.
fn simple_truncate(text: &str, max_width: usize) -> String {
    if text.chars().count() > max_width {
        let truncated: String = text.chars().take(max_width.saturating_sub(1)).collect();
        format!("{}…", truncated)
    } else {
        text.to_string()
    }
}

/// Build a display string showing context around each match, joined by "…".
/// Operates on already-sanitized text (e.g. preview). Falls back to simple
/// truncation when all matches already fit within max_width.
fn build_match_segments(text: &str, query: &str, max_width: usize) -> String {
    if query.is_empty() || max_width == 0 {
        return simple_truncate(text, max_width);
    }

    let ranges = find_normalized_match_ranges(text, query);
    if ranges.is_empty() {
        return simple_truncate(text, max_width);
    }

    // Convert byte ranges to char ranges for width budgeting
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();
    let text_char_len = char_indices.len();

    // Map byte offset → char index
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
    let mut clusters: Vec<(usize, usize)> = Vec::new(); // (char_start, char_end) of cluster
    for &(cs, ce) in &char_ranges {
        if let Some(last) = clusters.last_mut()
            && cs <= last.1 + merge_gap
        {
            last.1 = last.1.max(ce);
            continue;
        }
        clusters.push((cs, ce));
    }

    // Cap at 3 clusters
    clusters.truncate(3);

    // Calculate how many ellipsis chars we need
    let num_clusters = clusters.len();
    // Ellipsis between clusters + possibly leading + possibly trailing
    let match_chars: usize = clusters.iter().map(|(s, e)| e - s).sum();
    // We need at least 1 ellipsis between each pair + leading if first doesn't start at 0
    // + trailing (assume we always need trailing since text was too long)
    let max_ellipsis = num_clusters + 1; // worst case: leading + between each + trailing
    let available_context = max_width
        .saturating_sub(match_chars)
        .saturating_sub(max_ellipsis);
    let padding_per_side = if num_clusters > 0 {
        available_context / (num_clusters * 2)
    } else {
        0
    };

    // Build segments, tracking last position to prevent overlap
    let mut result = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut last_seg_end: usize = 0;

    for (i, &(cl_start, cl_end)) in clusters.iter().enumerate() {
        let mut seg_start = cl_start.saturating_sub(padding_per_side);
        let seg_end = (cl_end + padding_per_side).min(text_char_len);

        // Prevent overlapping with previous segment
        if i > 0 {
            seg_start = seg_start.max(last_seg_end);
        }

        if (i == 0 && seg_start > 0) || (i > 0 && seg_start > last_seg_end) {
            result.push('…');
        }

        let segment: String = chars[seg_start..seg_end].iter().collect();
        result.push_str(&segment);
        last_seg_end = seg_end;
    }

    // Add trailing ellipsis if we didn't reach the end
    let last_cluster_end = clusters.last().map(|(_, e)| *e).unwrap_or(0);
    if last_cluster_end + padding_per_side < text_char_len {
        result.push('…');
    }

    // Final safety truncation
    if result.chars().count() > max_width {
        let truncated: String = result.chars().take(max_width.saturating_sub(1)).collect();
        return format!("{}…", truncated);
    }

    result
}

/// Build a context string showing snippets around hidden matches in full_text.
/// Hidden matches are those beyond what's visible in the preview.
/// Operates on raw full_text and sanitizes each extracted slice independently.
fn build_context_segments(
    full_text: &str,
    preview: &str,
    query: &str,
    max_width: usize,
) -> Option<String> {
    if query.is_empty() || max_width == 0 {
        return None;
    }

    // Use lazy scanner to avoid allocating NormalizedText over large full_text strings.
    // Prioritize showing terms NOT already visible in the preview.
    // For each term, check if it has matches in the preview; if not,
    // find its first match in full_text using the lazy scanner.
    let terms: Vec<&str> = query.split_whitespace().collect();
    let mut missing_term_matches: Vec<(usize, usize)> = Vec::new();

    for term in &terms {
        if find_first_normalized_match(preview, term).is_some() {
            continue;
        }
        // Term not in preview — find first match in full_text lazily
        if let Some(first) = find_first_normalized_match(full_text, term) {
            missing_term_matches.push(first);
        }
    }

    // If all terms are already in preview, fall back to showing additional
    // matches beyond what the preview covers (original behavior).
    // Use consistent counting (both lazy scanner) to avoid merge mismatches.
    let raw_hidden = if missing_term_matches.is_empty() {
        let preview_match_count = find_all_normalized_matches(preview, &terms).len();
        let all_matches = find_all_normalized_matches(full_text, &terms);
        if all_matches.len() <= preview_match_count {
            return None;
        }
        all_matches.into_iter().skip(preview_match_count).collect()
    } else {
        missing_term_matches
    };

    if raw_hidden.is_empty() {
        return None;
    }

    // Sort by position, cluster nearby matches (gap < 50 bytes)
    let mut sorted = raw_hidden;
    sorted.sort_unstable_by_key(|m| m.0);
    let merge_gap = 50;
    let mut hidden_matches: Vec<(usize, usize)> = Vec::new();
    for m in sorted {
        if let Some(last) = hidden_matches.last_mut()
            && m.0 <= last.1 + merge_gap
        {
            last.1 = last.1.max(m.1);
            continue;
        }
        hidden_matches.push(m);
    }
    hidden_matches.truncate(3); // cap at 3 clusters

    // For each hidden match cluster, extract a context window from raw full_text,
    // then sanitize just that slice
    let num_segments = hidden_matches.len();
    let budget_per_segment = max_width.saturating_sub(num_segments + 1) / num_segments; // reserve for ellipsis

    let mut result = String::new();
    let mut remaining_width = max_width;
    let mut prev_end_byte: usize = 0;

    for (i, &(match_start, match_end)) in hidden_matches.iter().enumerate() {
        let match_char_len = full_text[match_start..match_end].chars().count();
        let context_chars = budget_per_segment
            .saturating_sub(match_char_len)
            .saturating_sub(2) // reserve for "…" on each side
            / 2;

        // Find char boundaries for the context window in raw full_text
        let mut start_byte = full_text[..match_start]
            .char_indices()
            .rev()
            .nth(context_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(0);

        // Prevent overlapping with previous segment
        start_byte = start_byte.max(prev_end_byte);

        let end_byte = full_text[match_end..]
            .char_indices()
            .nth(context_chars)
            .map(|(idx, _)| match_end + idx)
            .unwrap_or(full_text.len())
            .min(full_text.len());

        let snippet = &full_text[start_byte..end_byte];
        let sanitized = sanitize_preview(snippet);

        // Add ellipsis if there's a gap before this segment
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

        // Append segment, truncating if needed
        let seg_char_count = sanitized.chars().count();
        if seg_char_count <= remaining_width {
            result.push_str(&sanitized);
            remaining_width = remaining_width.saturating_sub(seg_char_count);
        } else {
            // Truncate this segment to fit
            let budget = remaining_width.saturating_sub(1);
            let trunc: String = sanitized.chars().take(budget).collect();
            result.push_str(&trunc);
            result.push('…');
            remaining_width = 0;
            break;
        }
    }

    // Add trailing ellipsis if last match didn't reach end of full_text
    if remaining_width > 0 {
        let last_end = hidden_matches.last().map(|(_, e)| *e).unwrap_or(0);
        if last_end < full_text.len() {
            result.push('…');
        }
    }

    if result.is_empty() {
        None
    } else {
        // Final safety truncation
        Some(simple_truncate(&result, max_width))
    }
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

/// Lazy, zero-allocation normalized search over a string.
/// Scans char-by-char without building intermediate Vecs, short-circuits on first match.
/// Used for large strings (full_text) where NormalizedText would allocate too much.
fn find_first_normalized_match(text: &str, term: &str) -> Option<(usize, usize)> {
    let term_chars: Vec<char> = term.chars().collect();
    if term_chars.is_empty() {
        return None;
    }
    let query_starts_alnum = term_chars[0].is_alphanumeric();
    let mut prev_is_alnum = false;
    let mut iter = text.char_indices().peekable();

    while let Some(&(byte_start, ch)) = iter.peek() {
        let norm_ch = if ch == '_' || ch == '-' || ch == '/' {
            ' '
        } else {
            ch.to_lowercase().next().unwrap_or(ch)
        };
        let is_alnum = ch.is_alphanumeric();
        let valid_start = !query_starts_alnum || !prev_is_alnum;

        if valid_start && norm_ch == term_chars[0] {
            // Try to match the full term from here
            let mut lookahead = iter.clone();
            lookahead.next(); // skip current char
            let mut matched = true;
            let mut end_byte = byte_start + ch.len_utf8();

            for &q_char in term_chars.iter().skip(1) {
                if let Some(&(_, next_ch)) = lookahead.peek() {
                    let next_norm = if next_ch == '_' || next_ch == '-' || next_ch == '/' {
                        ' '
                    } else {
                        next_ch.to_lowercase().next().unwrap_or(next_ch)
                    };
                    end_byte += next_ch.len_utf8();
                    lookahead.next();
                    if next_norm != q_char {
                        matched = false;
                        break;
                    }
                } else {
                    matched = false;
                    break;
                }
            }

            if matched {
                return Some((byte_start, end_byte));
            }
        }

        prev_is_alnum = is_alnum;
        iter.next();
    }
    None
}

/// Find all normalized matches for multiple terms in text, sorted by position.
/// Uses lazy scanning — avoids building NormalizedText.
fn find_all_normalized_matches(text: &str, terms: &[&str]) -> Vec<(usize, usize)> {
    let mut all_matches = Vec::new();
    for term in terms {
        let term_chars: Vec<char> = term.chars().collect();
        if term_chars.is_empty() {
            continue;
        }
        let query_starts_alnum = term_chars[0].is_alphanumeric();
        let mut prev_is_alnum = false;
        let mut iter = text.char_indices().peekable();

        while let Some(&(byte_start, ch)) = iter.peek() {
            let norm_ch = if ch == '_' || ch == '-' || ch == '/' {
                ' '
            } else {
                ch.to_lowercase().next().unwrap_or(ch)
            };
            let is_alnum = ch.is_alphanumeric();
            let valid_start = !query_starts_alnum || !prev_is_alnum;

            if valid_start && norm_ch == term_chars[0] {
                let mut lookahead = iter.clone();
                lookahead.next();
                let mut matched = true;
                let mut end_byte = byte_start + ch.len_utf8();

                for &q_char in term_chars.iter().skip(1) {
                    if let Some(&(_, next_ch)) = lookahead.peek() {
                        let next_norm = if next_ch == '_' || next_ch == '-' || next_ch == '/' {
                            ' '
                        } else {
                            next_ch.to_lowercase().next().unwrap_or(next_ch)
                        };
                        end_byte += next_ch.len_utf8();
                        lookahead.next();
                        if next_norm != q_char {
                            matched = false;
                            break;
                        }
                    } else {
                        matched = false;
                        break;
                    }
                }

                if matched {
                    all_matches.push((byte_start, end_byte));
                    // Skip past this match
                    for _ in 0..term_chars.len().saturating_sub(1) {
                        iter.next();
                    }
                    prev_is_alnum = true;
                    iter.next();
                    continue;
                }
            }

            prev_is_alnum = is_alnum;
            iter.next();
        }
    }
    all_matches.sort_unstable_by_key(|m| m.0);
    all_matches
}

/// Pre-normalized text with char-to-byte mapping for efficient repeated searches.
struct NormalizedText {
    norm_chars: Vec<char>,
    char_map: Vec<(usize, usize)>,
}

impl NormalizedText {
    fn new(text: &str) -> Self {
        let mut norm_chars: Vec<char> = Vec::new();
        let mut char_map: Vec<(usize, usize)> = Vec::new();

        let mut iter = text.char_indices().peekable();
        while let Some((byte_start, ch)) = iter.next() {
            let byte_end = iter.peek().map_or(text.len(), |(i, _)| *i);
            if ch == '_' {
                norm_chars.push(' ');
                char_map.push((byte_start, byte_end));
            } else {
                for lc in ch.to_lowercase() {
                    norm_chars.push(lc);
                    char_map.push((byte_start, byte_end));
                }
            }
        }

        Self {
            norm_chars,
            char_map,
        }
    }

    /// Find all non-overlapping matches of a single term, with left word boundary.
    fn find_term_ranges(&self, term: &str) -> Vec<(usize, usize)> {
        let query_chars: Vec<char> = term.chars().collect();
        if query_chars.is_empty() {
            return Vec::new();
        }

        let query_starts_alnum = query_chars.first().is_some_and(|c| c.is_alphanumeric());
        let mut matches = Vec::new();

        let mut i = 0;
        while i + query_chars.len() <= self.norm_chars.len() {
            if self.norm_chars[i..i + query_chars.len()] == query_chars[..] {
                let prev_is_alnum = i > 0 && self.norm_chars[i - 1].is_alphanumeric();
                let valid_start = !query_starts_alnum || !prev_is_alnum;

                if valid_start {
                    let start_byte = self.char_map[i].0;
                    let end_byte = self.char_map[i + query_chars.len() - 1].1;
                    matches.push((start_byte, end_byte));
                    i += query_chars.len();
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }

        matches
    }

    /// Find all matches for a multi-word query, sorted and merged.
    fn find_all_ranges(&self, query_normalized: &str) -> Vec<(usize, usize)> {
        let terms: Vec<&str> = query_normalized.split_whitespace().collect();
        if terms.is_empty() {
            return Vec::new();
        }

        let mut all_matches = Vec::new();
        for term in &terms {
            all_matches.extend(self.find_term_ranges(term));
        }

        // Sort and merge overlapping or separator-adjacent ranges.
        // This ensures "run_with_loader" highlights as one span including the underscores
        // when searching "run with loader" (underscores normalized to spaces).
        all_matches.sort_unstable_by_key(|m| m.0);
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(all_matches.len());
        for m in all_matches {
            if let Some(last) = merged.last_mut() {
                if m.0 <= last.1 {
                    // Overlapping — merge
                    last.1 = last.1.max(m.1);
                    continue;
                }
                // Check if the gap between ranges is only separators (_, -, /)
                let gap = &self.norm_chars[..];
                let gap_start = self.byte_to_char_index(last.1);
                let gap_end = self.byte_to_char_index(m.0);
                if gap_start < gap_end
                    && gap[gap_start..gap_end]
                        .iter()
                        .all(|c| *c == ' ' || *c == '_' || *c == '-' || *c == '/')
                {
                    last.1 = m.1;
                    continue;
                }
            }
            merged.push(m);
        }

        merged
    }

    /// Convert a byte offset in the original text to a char index in norm_chars
    fn byte_to_char_index(&self, byte_offset: usize) -> usize {
        self.char_map
            .iter()
            .position(|(start, _)| *start >= byte_offset)
            .unwrap_or(self.char_map.len())
    }
}

/// Find all non-overlapping matches of `query_normalized` in `text` after normalizing `text`.
/// Returns byte ranges in the original `text` for each match.
fn find_normalized_match_ranges(text: &str, query_normalized: &str) -> Vec<(usize, usize)> {
    NormalizedText::new(text).find_all_ranges(query_normalized)
}

fn highlight_text(
    text: &str,
    query: &str,
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    if query.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let ranges = find_normalized_match_ranges(text, query);
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let mut spans = Vec::new();
    let mut pos = 0;

    for (start, end) in &ranges {
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

    // --- highlight_text / find_normalized_match_ranges tests ---

    /// Helper: extract (text, is_highlighted) from spans
    fn span_info<'a>(spans: &'a [Span<'a>], highlight_style: Style) -> Vec<(&'a str, bool)> {
        spans
            .iter()
            .map(|s| (s.content.as_ref(), s.style == highlight_style))
            .collect()
    }

    #[test]
    fn highlight_word_boundary_prefix() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        // "red" matches at start of "redaction" (prefix), but not mid-word
        let spans = highlight_text("Extend log redaction to cover", "red team", base, hl);
        let info = span_info(&spans, hl);
        let highlighted: Vec<_> = info.iter().filter(|(_, h)| *h).collect();
        assert_eq!(highlighted.len(), 1);
        assert_eq!(highlighted[0].0, "red");
    }

    #[test]
    fn highlight_phrase_exact_match() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let spans = highlight_text(
            "You are being tested as a security red team exercise.",
            "red team",
            base,
            hl,
        );
        let info = span_info(&spans, hl);
        let highlighted: Vec<_> = info.iter().filter(|(_, h)| *h).collect();
        // Adjacent words separated by space merge into one highlight span
        assert_eq!(highlighted.len(), 1);
        assert_eq!(highlighted[0].0, "red team");
    }

    #[test]
    fn highlight_multiple_matches() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let spans = highlight_text("foo bar foo bar foo", "foo", base, hl);
        let highlighted: Vec<_> = span_info(&spans, hl)
            .into_iter()
            .filter(|(_, h)| *h)
            .collect();
        assert_eq!(highlighted.len(), 3);
        assert!(highlighted.iter().all(|(text, _)| *text == "foo"));
    }

    #[test]
    fn highlight_underscore_normalization() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        // Query "red team" matches "red_team" as one span including the underscore
        let spans = highlight_text("config for red_team setup", "red team", base, hl);
        let info = span_info(&spans, hl);
        let highlighted: Vec<_> = info.iter().filter(|(_, h)| *h).collect();
        assert_eq!(highlighted.len(), 1);
        assert_eq!(highlighted[0].0, "red_team");
    }

    #[test]
    fn highlight_case_insensitive() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let spans = highlight_text("Hello World", "hello", base, hl);
        let info = span_info(&spans, hl);
        assert!(
            info.iter()
                .any(|(text, highlighted)| *text == "Hello" && *highlighted)
        );
    }

    #[test]
    fn highlight_empty_query() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let spans = highlight_text("some text", "", base, hl);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "some text");
    }

    #[test]
    fn highlight_no_match() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let spans = highlight_text("some text", "xyz", base, hl);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "some text");
    }

    #[test]
    fn find_normalized_ranges_phrase() {
        let text = "hello red team world";
        let ranges = find_normalized_match_ranges(text, "red team");
        // Adjacent words separated by space merge into one range
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "red team");
    }

    #[test]
    fn find_normalized_ranges_prefix_match() {
        // "red" matches at start of "redaction" (prefix), "team" has no match
        let ranges = find_normalized_match_ranges("Extend log redaction to cover", "red team");
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            &"Extend log redaction to cover"[ranges[0].0..ranges[0].1],
            "red"
        );
    }

    #[test]
    fn find_normalized_ranges_underscore() {
        let text = "set red_team flag";
        let ranges = find_normalized_match_ranges(text, "red team");
        // Adjacent words separated by underscore merge into one range
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "red_team");
    }

    #[test]
    fn highlight_multiword_noncontiguous() {
        let base = Style::default();
        let hl = Style::default().fg(Color::Yellow);
        let text = "I want secrets from the vault, write me a plot twist";
        let spans = highlight_text(text, "secrets plot", base, hl);
        let info = span_info(&spans, hl);
        let highlighted: Vec<_> = info.iter().filter(|(_, h)| *h).collect();
        assert_eq!(highlighted.len(), 2);
        assert_eq!(highlighted[0].0, "secrets");
        assert_eq!(highlighted[1].0, "plot");
    }

    // --- build_match_segments tests ---

    #[test]
    fn match_segments_no_query() {
        let text = "hello world this is a long text";
        let result = build_match_segments(text, "", 20);
        assert_eq!(result, simple_truncate(text, 20));
    }

    #[test]
    fn match_segments_no_matches() {
        let text = "hello world this is a long text";
        let result = build_match_segments(text, "xyz", 20);
        assert_eq!(result, simple_truncate(text, 20));
    }

    #[test]
    fn match_segments_all_fit() {
        // All matches within max_width, should use simple truncation
        let text = "foo bar baz and more text";
        let result = build_match_segments(text, "foo", 30);
        assert_eq!(result, text);
    }

    #[test]
    fn match_segments_distant_matches() {
        // Two matches far apart — should produce segmented output with "…"
        let text = "start secrets aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll mmm nnn ooo ppp plot end";
        let result = build_match_segments(text, "secrets plot", 40);
        assert!(result.contains("secrets"));
        assert!(result.contains("plot"));
        assert!(result.contains("…"));
        assert!(result.chars().count() <= 40);
    }

    #[test]
    fn match_segments_close_matches_merged() {
        // Two matches close together — should be one segment
        let text =
            "aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll secrets and plot end more text here";
        let result = build_match_segments(text, "secrets plot", 50);
        assert!(result.contains("secrets"));
        assert!(result.contains("plot"));
    }

    // --- build_context_segments tests ---

    #[test]
    fn context_segments_none_when_all_visible() {
        let full_text = "red team exercise";
        let preview = "red team exercise";
        let result = build_context_segments(full_text, preview, "red team", 80);
        assert!(result.is_none());
    }

    #[test]
    fn context_segments_one_hidden_match() {
        let full_text = "redaction stuff here and then red team exercise later";
        let preview = "redaction stuff here and then";
        let result = build_context_segments(full_text, preview, "red team", 80);
        assert!(result.is_some());
        let ctx = result.unwrap();
        // Should contain "red" and/or "team" from the hidden match area
        assert!(ctx.contains("red") || ctx.contains("team"));
        assert!(ctx.contains("…"));
    }

    #[test]
    fn context_segments_multiword_hidden() {
        let full_text = "I want secrets from the vault, and later write me a plot twist";
        let preview = "I want secrets from the";
        // Preview has "secrets", hidden has "plot" — context should prioritize "plot"
        let result = build_context_segments(full_text, preview, "secrets plot", 80);
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("plot"));
    }

    #[test]
    fn context_segments_prioritizes_missing_terms() {
        // "secrets" appears many times but "plot" only once deep in text.
        // Preview shows "secrets" — context should show "plot", not more "secrets".
        let full_text = "secrets here and secrets there and secrets everywhere and finally a plot twist at the end";
        let preview = "secrets here and secrets there";
        let result = build_context_segments(full_text, preview, "secrets plot", 80);
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(
            ctx.contains("plot"),
            "context should contain 'plot' but was: {ctx}"
        );
    }

    #[test]
    fn context_segments_empty_query() {
        let result = build_context_segments("some text", "some", "", 80);
        assert!(result.is_none());
    }

    // --- word boundary tests ---

    #[test]
    fn word_boundary_rejects_mid_word() {
        // "red" should not match inside "fired" (not at word start)
        let ranges = find_normalized_match_ranges("fired and tired", "red");
        assert_eq!(ranges.len(), 0);
    }

    #[test]
    fn word_boundary_allows_prefix() {
        // "red" matches at start of "redaction" (prefix matching)
        let ranges = find_normalized_match_ranges("redaction plan", "red");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&"redaction plan"[ranges[0].0..ranges[0].1], "red");
    }

    #[test]
    fn word_boundary_accepts_whole_word() {
        let ranges = find_normalized_match_ranges("the red fox", "red");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&"the red fox"[ranges[0].0..ranges[0].1], "red");
    }

    #[test]
    fn word_boundary_accepts_punctuation_adjacent() {
        // "red" after punctuation should match
        let ranges = find_normalized_match_ranges("it was (red) not blue", "red");
        assert_eq!(ranges.len(), 1);
    }

    #[test]
    fn word_boundary_start_end_of_string() {
        let ranges = find_normalized_match_ranges("red", "red");
        assert_eq!(ranges.len(), 1);
        let ranges = find_normalized_match_ranges("red fox", "red");
        assert_eq!(ranges.len(), 1);
        let ranges = find_normalized_match_ranges("the red", "red");
        assert_eq!(ranges.len(), 1);
    }
}
