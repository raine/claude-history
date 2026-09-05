use crate::config::KeyBindings;
use crate::search::evidence::select_hidden_context_ranges;
use crate::search::literal::{Literal, match_literal_ranges};
use crate::search::normalize_for_search;
use crate::search::query::ParsedQuery;
use crate::tui::app::{
    App, AppMode, DialogMode, ListSearchMode, LoadingState, SemanticResultMetadata, ViewSearchMode,
    ViewState, list_lines_per_item,
};
use crate::tui::theme::{self, Theme};
use crate::tui::viewer::{LineStyle, RenderedLine};
use chrono::{DateTime, Local};
use ratatui::layout::Position;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Get the current theme
fn th() -> &'static Theme {
    theme::detect_theme()
}

/// Convert theme RGB tuple to ratatui Color
fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

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

    match app.dialog_mode() {
        DialogMode::Help { scroll } => render_help_overlay(
            frame,
            false,
            false,
            app.semantic_toggle_available(),
            app.keys(),
            *scroll,
        ),
        DialogMode::SemanticDebug => render_semantic_debug_popup(frame, app),
        DialogMode::Rename { input, cursor } => render_rename_dialog(frame, input, *cursor),
        _ => {}
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

fn render_activity_status(frame: &mut Frame, msg: &str, area: Rect) {
    let status_line = Line::from(vec![
        Span::raw("  "),
        Span::styled(msg, Style::default().fg(rgb(th().accent)).bold()),
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

    if let Some(status) = app.semantic_activity_status_text() {
        render_activity_status(frame, &status, area);
        return;
    }

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
        Span::styled(keys.rename.short_label(), action_key),
        Span::styled(" rename  ", action_label),
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

    if app.semantic_toggle_available() {
        let mode_style = if app.list_search_mode() == ListSearchMode::Semantic {
            Style::default().fg(rgb(th().accent)).bold()
        } else {
            label_style
        };
        spans.extend([
            Span::styled("Ctrl+T", key_style),
            Span::styled(" semantic·", label_style),
            Span::styled(app.list_search_mode().label(), mode_style),
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

fn semantic_rationale_label(metadata: &SemanticResultMetadata) -> &'static str {
    match metadata.explanation.rationale_kind {
        crate::semantic::types::SemanticRationaleKind::SemanticOnly => "semantic",
        crate::semantic::types::SemanticRationaleKind::LexicalBoosted => "lex boost",
        crate::semantic::types::SemanticRationaleKind::WeakMatch => "weak",
    }
}

fn semantic_row_metadata(metadata: &SemanticResultMetadata) -> String {
    format!("{:.2}", metadata.score_breakdown.hybrid)
}

fn render_semantic_debug_popup(frame: &mut Frame, app: &App) {
    let Some(metadata) = app.semantic_result_metadata_for_selection() else {
        return;
    };
    let area = frame.area();
    let popup = centered_modal_area(area, 68, 10);
    frame.render_widget(Clear, popup);
    let background = Block::default().style(Style::default().bg(rgb(th().overlay_bg)));
    frame.render_widget(background, popup);
    let block = Block::default()
        .title(" Semantic result ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(th().accent)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.is_empty() {
        return;
    }

    let score = &metadata.score_breakdown;
    let explanation = &metadata.explanation;
    let lines = vec![
        Line::from(vec![
            Span::styled(" hybrid ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                format!("{:.2}", score.hybrid),
                Style::default().fg(rgb(th().accent)).bold(),
            ),
            Span::styled("  semantic ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                format!("{:.2}", score.semantic),
                Style::default().fg(rgb(th().text_primary)),
            ),
            Span::styled("  lexical ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                format!("{:.2}", score.lexical),
                Style::default().fg(rgb(th().text_primary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" rationale ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                semantic_rationale_label(metadata),
                Style::default().fg(rgb(th().text_primary)),
            ),
            Span::styled("  quality ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                explanation.quality_label,
                Style::default().fg(rgb(th().text_primary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" chunk ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                format!(
                    "{} #{}",
                    explanation.chunk.session, explanation.chunk.chunk_index
                ),
                Style::default().fg(rgb(th().text_primary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" terms ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                if explanation.matched_terms.is_empty() {
                    "(none)".to_string()
                } else {
                    explanation.matched_terms.join(", ")
                },
                Style::default().fg(rgb(th().text_primary)),
            ),
        ]),
        Line::from(vec![
            Span::styled(" preview ", Style::default().fg(rgb(th().text_muted))),
            Span::styled(
                simple_truncate(
                    &sanitize_preview(&explanation.evidence_preview),
                    inner.width.saturating_sub(10) as usize,
                ),
                Style::default().fg(rgb(th().preview)),
            ),
        ]),
        Line::from(""),
        Line::styled(" Esc close", Style::default().fg(rgb(th().text_muted))),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
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

#[derive(Clone, Copy, Debug)]
pub struct ViewLayoutRects {
    pub header: Rect,
    pub content: Rect,
    pub status: Rect,
}

pub fn view_layout_rects(area: Rect, app: &App, state: &ViewState) -> ViewLayoutRects {
    let status_height = if state.search_mode == ViewSearchMode::Typing {
        2
    } else {
        1
    };
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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Min(1),
            Constraint::Length(status_height),
        ])
        .split(area);

    ViewLayoutRects {
        header: chunks[0],
        content: chunks[1],
        status: chunks[2],
    }
}

/// Render the view mode (conversation viewer)
fn render_view_mode(frame: &mut Frame, app: &App, state: &ViewState) {
    let layout = view_layout_rects(frame.area(), app, state);

    render_view_header(frame, app, state, layout.header);
    render_view_content(frame, state, layout.content);

    if state.search_mode == ViewSearchMode::Typing {
        render_search_input(frame, state, layout.status);
    } else {
        render_view_status_bar(frame, app, state, layout.status);
    }

    // Render dialog overlay if active
    match app.dialog_mode() {
        DialogMode::ConfirmDelete => render_confirm_dialog(frame, layout.status),
        DialogMode::ExportMenu { selected } => render_export_menu(frame, *selected, false),
        DialogMode::YankMenu { selected } => render_export_menu(frame, *selected, true),
        DialogMode::Help { scroll } => {
            render_help_overlay(
                frame,
                true,
                app.is_single_file_mode(),
                false,
                app.keys(),
                *scroll,
            );
        }
        DialogMode::SemanticDebug => render_semantic_debug_popup(frame, app),
        DialogMode::Rename { input, cursor } => render_rename_dialog(frame, input, *cursor),
        DialogMode::Annotate {
            input,
            cursor,
            line,
            replacing,
        } => {
            let verb = if replacing.is_some() { "Edit" } else { "Note" };
            let title = match line {
                Some(line) => format!(" {verb} on line {line} "),
                None => format!(" {verb} on session "),
            };
            render_text_prompt(frame, input, *cursor, &title);
        }
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

    // Determine the focused line range. A selected note takes the range first,
    // so the gutter marks the selection the d and e keys act on; message nav
    // supplies the range the rest of the time.
    let focused_range = if let Some(id) = state.focused_annotation.as_deref() {
        state
            .annotation_ranges
            .iter()
            .find(|range| range.id == id)
            .map(|range| range.start_line..range.end_line)
    } else if state.message_nav_active {
        state
            .focused_message
            .and_then(|idx| state.message_ranges.get(idx))
            .map(|m| m.start_line..m.end_line)
    } else {
        None
    };
    // The gutter column is drawn while either selection is live. Without the
    // note case the marker stays hidden, because clicking a note clears
    // message_nav_active.
    let gutter_active = state.message_nav_active || state.focused_annotation.is_some();

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

            // Gutter indicator (shown for a nav-focused message or a selected note)
            let gutter = if gutter_active {
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

            let is_hovered = rendered
                .tool_output_id
                .as_ref()
                .is_some_and(|id| state.hovered_tool_output.as_ref() == Some(id));
            if is_hovered {
                let used_width: usize = spans
                    .iter()
                    .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                    .sum();
                let padding = (area.width as usize).saturating_sub(used_width);
                if padding > 0 {
                    spans.push(Span::styled(
                        " ".repeat(padding),
                        Style::default().bg(rgb(th().selection_bg)),
                    ));
                }
            }

            let mut line = Line::from(spans);
            if is_hovered {
                line = line.style(Style::default().bg(rgb(th().selection_bg)));
            }

            line
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
    let notes_status = if state.show_annotations { "on " } else { "off" };

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(scroll_pos, Style::default().fg(rgb(th().text_secondary))),
        Span::raw("  "),
        Span::styled("t", key_style),
        Span::styled(format!("ools·{} ", tools_status), label_style),
        Span::styled("T", key_style),
        Span::styled(format!("hink·{} ", thinking_status), label_style),
        Span::styled("i", key_style),
        Span::styled(format!("nfo·{} ", timing_status), label_style),
        Span::styled("A", key_style),
        Span::styled(format!("notes·{}", notes_status), label_style),
        Span::raw("  "),
        Span::styled("│", label_style),
        Span::raw("  "),
    ];

    // A selected note takes d, e, y and Esc ahead of every other binding, so
    // the row names the note's keys instead of export, delete and yank. Without
    // the swap the footer prints bindings that do not fire.
    if state.focused_annotation.is_some() {
        spans.extend([
            Span::styled("e", key_style),
            Span::styled("dit note  ", label_style),
            Span::styled("d", key_style),
            Span::styled("elete note  ", label_style),
            Span::styled("y", key_style),
            Span::styled("ank note  ", label_style),
            Span::styled("Esc", key_style),
            Span::styled(" deselect", label_style),
        ]);
    } else if state.search_mode == ViewSearchMode::Active && !state.search_matches.is_empty() {
        spans.extend([
            Span::styled("n", key_style),
            Span::styled("ext  ", label_style),
            Span::styled("N", key_style),
            Span::styled("prev  ", label_style),
            Span::styled(
                format!(
                    "{}/{}  ",
                    state.current_match + 1,
                    state.search_matches.len()
                ),
                Style::default().fg(rgb(th().text_secondary)),
            ),
            Span::styled("Esc", key_style),
            Span::styled(" clear", label_style),
        ]);
    } else {
        spans.extend([
            Span::styled("a", key_style),
            Span::styled("note  ", label_style),
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
    let count_text = match app.loading_state() {
        LoadingState::Loading { loaded } => format!("Loading... {}", loaded),
        LoadingState::Ready => match app.selected() {
            Some(selected) => format!("{}/{}", selected + 1, app.filtered().len()),
            None => format!("0/{}", app.filtered().len()),
        },
    };
    let status_text = if app.semantic_search_available() {
        app.semantic_status_text()
            .map(|status| {
                format!(
                    "{} {} {}",
                    app.list_search_mode().label(),
                    count_text,
                    status
                )
            })
            .unwrap_or_else(|| format!("{} {}", app.list_search_mode().label(), count_text))
    } else {
        count_text
    };

    let prompt_style = Style::default().fg(rgb(th().accent));
    let (prompt_spans, prefix_width) = if app.workspace_filter() {
        (
            vec![
                Span::raw(" "),
                Span::styled("Project", Style::default().fg(rgb(th().text_muted))),
                Span::raw(" "),
                Span::styled("\u{276F} ", prompt_style),
            ],
            11,
        )
    } else {
        (
            vec![Span::raw(" "), Span::styled("\u{276F} ", prompt_style)],
            3,
        )
    };

    let status_style = if app.is_loading() {
        Style::default().fg(rgb(th().accent))
    } else {
        Style::default().fg(rgb(th().text_muted))
    };
    let available = area.width as usize;
    let min_gap = usize::from(available > prefix_width);
    let right_budget = available.saturating_sub(prefix_width + min_gap);
    let rendered_status = simple_truncate(&status_text, right_budget);
    let right_width =
        UnicodeWidthStr::width(rendered_status.as_str()) + usize::from(!rendered_status.is_empty());
    let query_budget = available.saturating_sub(prefix_width + right_width + min_gap);
    let rendered_query = simple_truncate(app.query(), query_budget);
    let query_width = UnicodeWidthStr::width(rendered_query.as_str());
    let padding = available.saturating_sub(prefix_width + query_width + right_width);

    let mut spans = prompt_spans;
    spans.extend([
        Span::raw(rendered_query),
        Span::raw(" ".repeat(padding)),
        Span::styled(rendered_status, status_style),
        Span::raw(" "),
    ]);
    let search_line = Line::from(spans);

    let input = Paragraph::new(search_line).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(rgb(th().border))),
    );

    frame.render_widget(input, area);

    if area.width > prefix_width as u16 {
        let cursor_offset: u16 = app
            .query()
            .chars()
            .take(app.cursor_pos())
            .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
            .sum::<usize>()
            .min(query_budget)
            .min(u16::MAX as usize) as u16;
        let max_x = area
            .x
            .saturating_add(prefix_width as u16)
            .saturating_add(query_budget.min(u16::MAX as usize) as u16);
        let cursor_x = (area.x + prefix_width as u16)
            .saturating_add(cursor_offset)
            .min(max_x)
            .min(area.x + area.width.saturating_sub(1));
        frame.set_cursor_position(Position::new(cursor_x, area.y));
    }
}

fn centered_modal_area(area: Rect, preferred_width: u16, preferred_height: u16) -> Rect {
    let width = preferred_width.min(area.width);
    let height = preferred_height.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
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

fn render_rename_dialog(frame: &mut Frame, input: &str, cursor: usize) {
    render_text_prompt(frame, input, cursor, " Rename session ")
}

fn render_text_prompt(frame: &mut Frame, input: &str, cursor: usize, title: &str) {
    let area = frame.area();
    let menu_width = area.width.saturating_sub(4).clamp(30, 70);
    let menu_height = 4;
    let menu_area = Rect {
        x: (area.width.saturating_sub(menu_width)) / 2,
        y: (area.height.saturating_sub(menu_height)) / 2,
        width: menu_width,
        height: menu_height,
    };

    frame.render_widget(Clear, menu_area);
    let background = Block::default().style(Style::default().bg(rgb(th().overlay_bg)));
    frame.render_widget(background, menu_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(rgb(th().accent)));
    let inner = block.inner(menu_area);
    frame.render_widget(block, menu_area);

    let input_width = inner.width.saturating_sub(2) as usize;
    let display = simple_truncate(input, input_width);
    let lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(display, Style::default().fg(rgb(th().text_primary))),
        ]),
        Line::styled(
            " Enter save · Esc cancel",
            Style::default().fg(rgb(th().text_muted)),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), inner);

    let cursor_offset: u16 = input
        .chars()
        .take(cursor)
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum::<usize>()
        .min(input_width) as u16;
    frame.set_cursor_position(Position::new(
        inner.x.saturating_add(1).saturating_add(cursor_offset),
        inner.y,
    ));
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

    let menu_area = centered_modal_area(area, menu_width, menu_height);

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

    if inner.is_empty() {
        return;
    }

    let menu_content = Paragraph::new(lines);
    frame.render_widget(menu_content, inner);
}

fn render_help_overlay(
    frame: &mut Frame,
    is_view_mode: bool,
    is_single_file_mode: bool,
    semantic_available: bool,
    keys: &KeyBindings,
    scroll: usize,
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
            (keys.delete.help_label(), "Delete"),
            ("q / Esc".into(), exit_text),
        ]
    } else {
        let mut shortcuts = vec![
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
            (keys.rename.help_label(), "Rename"),
            (keys.delete.help_label(), "Delete"),
            ("Esc".into(), "Quit"),
        ];
        if semantic_available {
            shortcuts.insert(9, ("Ctrl+T".into(), "Toggle semantic search"));
            shortcuts.insert(10, ("Ctrl+S".into(), "Semantic details"));
        }
        shortcuts
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

    let menu_area = centered_modal_area(area, menu_width, menu_height);

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

    if inner.is_empty() {
        return;
    }

    let content_height = inner.height as usize;
    let indicator_needed = shortcuts.len() > content_height;
    let shortcut_rows = if indicator_needed {
        content_height.saturating_sub(1)
    } else {
        content_height
    };
    let max_scroll = shortcuts.len().saturating_sub(shortcut_rows);
    let scroll = scroll.min(max_scroll);

    let mut lines = Vec::new();
    if !indicator_needed {
        lines.extend(
            (0..content_height.saturating_sub(shortcuts.len()) / 2).map(|_| Line::from("")),
        );
    }
    for (key, action) in shortcuts.iter().skip(scroll).take(shortcut_rows) {
        let key_padding = max_key_len - key.chars().count();
        lines.push(Line::from(vec![
            Span::raw("  "),
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

    if indicator_needed && content_height > 0 {
        let start = scroll + 1;
        let end = (scroll + shortcut_rows).min(shortcuts.len());
        let indicator = match (scroll > 0, scroll < max_scroll) {
            (true, true) => format!("  ↑↓ more  {start}-{end}/{}", shortcuts.len()),
            (true, false) => format!("  ↑ more  {start}-{end}/{}", shortcuts.len()),
            (false, true) => format!("  ↓ more  {start}-{end}/{}", shortcuts.len()),
            (false, false) => format!("  {start}-{end}/{}", shortcuts.len()),
        };
        lines.push(Line::styled(
            indicator,
            Style::default().fg(rgb(th().text_muted)),
        ));
    }

    let content = Paragraph::new(lines);
    frame.render_widget(content, inner);
}

fn render_list(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let highlight_query = HighlightQuery::parse(app.query());
    let query_normalized = highlight_query.context_text();

    let semantic_mode = app.list_search_mode() == ListSearchMode::Semantic;
    let lines_per_item = list_lines_per_item(app.list_search_mode(), app.query());
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

            // Note count, stated only where notes exist, so a row without them
            // keeps its width for the summary.
            let note_count = match app.annotation_count(&conv.path) {
                0 => None,
                1 => Some("1 note".to_string()),
                n => Some(format!("{n} notes")),
            };

            // Selection indicator: vertical bar for all rows (with left padding)
            let indicator = " ▌ ";
            let indicator_style = if is_selected {
                Style::default().fg(rgb(th().accent))
            } else {
                Style::default().fg(rgb(th().border))
            };

            let semantic_metadata = app.semantic_result_metadata(conv_idx);
            let semantic_meta_part = (semantic_mode && width >= 70)
                .then(|| semantic_metadata.map(semantic_row_metadata))
                .flatten();
            let semantic_meta_len = semantic_meta_part
                .as_ref()
                .map(|s| UnicodeWidthStr::width(s.as_str()) + 3)
                .unwrap_or(0);

            let duration_len = duration
                .as_ref()
                .map(|d| UnicodeWidthStr::width(d.as_str()) + 3)
                .unwrap_or(0);
            let note_count_len = note_count
                .as_ref()
                .map(|n| UnicodeWidthStr::width(n.as_str()) + 3)
                .unwrap_or(0);
            let right_len = UnicodeWidthStr::width(msg_count.as_str())
                + note_count_len
                + duration_len
                + semantic_meta_len
                + 3
                + UnicodeWidthStr::width(timestamp.as_str());
            let indicator_len = UnicodeWidthStr::width(indicator);
            let min_padding = 3;
            let left_budget = width.saturating_sub(indicator_len + right_len + min_padding);

            // Build left part: indicator + project + optional custom title + optional summary
            let raw_project_part = conv
                .project_name
                .as_ref()
                .map(|name| {
                    if app.has_multiple_sources() {
                        format!("{:<3} · {name}", conv.source.list_label())
                    } else {
                        name.to_string()
                    }
                })
                .unwrap_or_default();
            let has_title_or_summary = conv.custom_title.as_ref().is_some_and(|s| !s.is_empty())
                || conv.summary.as_ref().is_some_and(|s| !s.is_empty());
            let raw_project_width = UnicodeWidthStr::width(raw_project_part.as_str());
            let reserved_left_detail = if width < 90 && has_title_or_summary {
                (left_budget / 3).clamp(10, 24)
            } else {
                0
            };
            let project_budget =
                raw_project_width.min(left_budget.saturating_sub(reserved_left_detail));
            let project_part = simple_truncate(&raw_project_part, project_budget);
            let project_len = UnicodeWidthStr::width(project_part.as_str());

            let title_budget = left_budget.saturating_sub(project_len + 3);
            let custom_title_part = conv
                .custom_title
                .as_ref()
                .filter(|s| !s.is_empty() && title_budget > 4)
                .map(|s| format!(" · {}", simple_truncate(s, title_budget)));
            let custom_title_len = custom_title_part
                .as_ref()
                .map(|s| UnicodeWidthStr::width(s.as_str()))
                .unwrap_or(0);

            let available_for_summary = width.saturating_sub(
                indicator_len + project_len + custom_title_len + right_len + min_padding + 4,
            );

            // Build summary part (dimmer, dynamically truncated based on available space)
            let summary_part = conv
                .summary
                .as_ref()
                .filter(|s| !s.is_empty() && available_for_summary > 5)
                .map(|s| {
                    if UnicodeWidthStr::width(s.as_str()) > available_for_summary {
                        format!(" · {}", simple_truncate(s, available_for_summary))
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
                    .map(|s| UnicodeWidthStr::width(s.as_str()))
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
            header_spans.extend(highlight_query.highlight(
                &project_part,
                project_style,
                highlight_style,
            ));

            // Add custom title if present (with search highlighting)
            if let Some(ref title) = custom_title_part {
                header_spans.extend(highlight_query.highlight(
                    title,
                    custom_title_style,
                    custom_title_highlight_style,
                ));
            }

            // Add summary if present (with search highlighting)
            if let Some(ref summary) = summary_part {
                header_spans.extend(highlight_query.highlight(
                    summary,
                    summary_style,
                    summary_highlight_style,
                ));
            }

            header_spans.push(Span::raw(" ".repeat(padding)));
            header_spans.push(Span::styled(
                msg_count,
                Style::default().fg(rgb(th().msg_count)),
            ));
            if let Some(ref notes) = note_count {
                header_spans.push(Span::styled(
                    " · ",
                    Style::default().fg(rgb(th().dot_separator)),
                ));
                header_spans.push(Span::styled(
                    notes.clone(),
                    Style::default().fg(rgb(th().annotation)),
                ));
            }
            if let Some(ref metadata_text) = semantic_meta_part {
                header_spans.push(Span::styled(
                    " · ",
                    Style::default().fg(rgb(th().dot_separator)),
                ));
                header_spans.push(Span::styled(
                    metadata_text.clone(),
                    Style::default().fg(rgb(th().accent)),
                ));
            }
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

            let max_preview_len = width.saturating_sub(4);
            let lexical_evidence = (!semantic_mode || semantic_metadata.is_none())
                .then(|| app.lexical_evidence(conv_idx))
                .flatten();
            let lexical_context = lexical_evidence.and_then(|evidence| {
                build_context_segments_from_ranges(
                    &conv.full_text,
                    &evidence.context_ranges,
                    max_preview_len,
                )
            });
            let semantic_preview = semantic_metadata
                .filter(|_| semantic_mode && !query_normalized.is_empty())
                .map(|metadata| sanitize_preview(&metadata.explanation.evidence_preview));
            let preview_text = if let Some(preview) = semantic_preview {
                preview
            } else if let Some(context) = lexical_context.as_ref() {
                context.clone()
            } else {
                sanitize_preview(&conv.preview)
            };
            let truncated_preview = if query_normalized.is_empty() {
                simple_truncate(&preview_text, max_preview_len)
            } else if semantic_mode && highlight_query.has_match(&preview_text) {
                build_match_segments_for_query(&preview_text, &highlight_query, max_preview_len)
            } else if semantic_mode || lexical_context.is_some() {
                simple_truncate(&preview_text, max_preview_len)
            } else {
                build_match_segments(&preview_text, &query_normalized, max_preview_len)
            };

            // Build preview with highlighted matches
            let preview_style = Style::default().fg(rgb(th().preview));
            let mut preview_spans = vec![Span::styled(indicator, indicator_style)];
            preview_spans.extend(highlight_query.highlight(
                &truncated_preview,
                preview_style,
                highlight_style,
            ));

            let preview = Line::from(preview_spans).style(selection_bg);

            let allow_literal_context = lexical_context.is_none();
            // Check for hidden literal matches and build context line if needed
            let context_line = if allow_literal_context
                && highlight_query.needs_literal_context(&truncated_preview)
            {
                let context_width = width.saturating_sub(4);
                build_literal_context_segments(
                    &conv.full_text,
                    &truncated_preview,
                    &highlight_query,
                    context_width,
                )
                .map(|context_text| {
                    let context_base_style = Style::default().fg(rgb(th().context_base));
                    let context_highlight_style = Style::default().fg(rgb(th().context_highlight));

                    let mut context_spans = vec![Span::styled(indicator, indicator_style)];
                    context_spans.extend(highlight_query.highlight(
                        &context_text,
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

/// Build a display string showing context around each match, joined by "…".
/// Operates on already-sanitized text (e.g. preview). Falls back to simple
/// truncation when all matches already fit within max_width.
fn build_match_segments_for_query(text: &str, query: &HighlightQuery, max_width: usize) -> String {
    build_match_segments_with_ranges(text, query.match_ranges(text), max_width, || {
        simple_truncate(text, max_width)
    })
}

fn build_match_segments(text: &str, query: &str, max_width: usize) -> String {
    if query.is_empty() || max_width == 0 {
        return simple_truncate(text, max_width);
    }

    let ranges = find_normalized_match_ranges(text, query);
    build_match_segments_with_ranges(text, ranges, max_width, || {
        truncate_around_match(text, query, max_width)
    })
}

fn build_match_segments_with_ranges(
    text: &str,
    ranges: Vec<(usize, usize)>,
    max_width: usize,
    fallback: impl Fn() -> String,
) -> String {
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

    if UnicodeWidthStr::width(result.as_str()) > max_width {
        return fallback();
    }

    result
}

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

fn truncate_around_match(text: &str, query: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let ranges = find_normalized_match_ranges(text, query);
    let Some((start, end)) = ranges.first().copied() else {
        return simple_truncate(text, max_width);
    };
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

/// Build a context string showing snippets around hidden matches in full_text.
///
/// Selection is cluster-based: collect every term hit in `full_text`, group
/// nearby hits into clusters, then rank clusters by:
///
/// 1. how many *missing* (not-in-preview) terms they cover,
/// 2. how many adjacent term pairs they contain (e.g. literal phrase match),
/// 3. total unique-term coverage,
/// 4. tighter span,
/// 5. earlier position.
///
/// This makes the literal phrase `audio generation` win over a far-apart pair
/// of `audio` + `generation` occurrences in unrelated boilerplate.
/// Operates on raw full_text and sanitizes each extracted slice independently.
fn build_literal_context_segments(
    full_text: &str,
    preview: &str,
    query: &HighlightQuery,
    max_width: usize,
) -> Option<String> {
    build_context_segments_with_specs(
        full_text,
        preview,
        &query.literal_context_specs(),
        max_width,
    )
}

#[cfg(test)]
fn build_context_segments_for_query(
    full_text: &str,
    preview: &str,
    query: &HighlightQuery,
    max_width: usize,
) -> Option<String> {
    build_context_segments_with_specs(full_text, preview, &query.context_specs(), max_width)
}

#[cfg(test)]
fn build_context_segments(
    full_text: &str,
    preview: &str,
    query: &str,
    max_width: usize,
) -> Option<String> {
    build_context_segments_with_specs(
        full_text,
        preview,
        &HighlightQuery::normalized_context_specs(query),
        max_width,
    )
}

fn build_context_segments_from_ranges(
    full_text: &str,
    hidden_matches: &[(usize, usize)],
    max_width: usize,
) -> Option<String> {
    if hidden_matches.is_empty() || max_width == 0 {
        return None;
    }

    build_context_segments_from_ranges_unchecked(full_text, hidden_matches, max_width)
}

fn build_context_segments_with_specs(
    full_text: &str,
    preview: &str,
    specs: &[ContextMatchSpec],
    max_width: usize,
) -> Option<String> {
    if specs.is_empty() || max_width == 0 {
        return None;
    }

    let specs = dedupe_context_specs(specs);
    if specs.is_empty() {
        return None;
    }

    let has_normalized_specs = specs
        .iter()
        .any(|spec| matches!(spec, ContextMatchSpec::Normalized(_)));
    let preview_normalized = has_normalized_specs.then(|| NormalizedText::new(preview));
    let full_text_normalized = has_normalized_specs.then(|| NormalizedText::new(full_text));

    let mut missing_mask: u64 = 0;
    let mut missing_count = 0u32;
    for (i, spec) in specs.iter().enumerate() {
        if spec
            .find_ranges(preview, preview_normalized.as_ref())
            .is_empty()
        {
            missing_mask |= 1 << i;
            missing_count += 1;
        }
    }

    let all_hits = find_all_spec_hits(full_text, &specs, full_text_normalized.as_ref());
    if all_hits.is_empty() {
        return None;
    }

    // Fallback: if every term is already visible in the preview, only emit a
    // context line when full_text contains *more* hits than the preview does.
    // We don't try to skip positionally — `preview` is sanitized/truncated and
    // `full_text` is raw, so positional alignment between the two hit streams
    // is unreliable. Instead we let the cluster ranker pick the most
    // informative cluster across the whole document. Worst case it picks one
    // that overlaps preview content, which is still the best snippet we have.
    if missing_count == 0 {
        let preview_hit_count =
            find_all_spec_hits(preview, &specs, preview_normalized.as_ref()).len();
        if all_hits.len() <= preview_hit_count {
            return None;
        }
    }

    let hit_spans: Vec<(usize, usize, usize)> = all_hits
        .iter()
        .map(|hit| (hit.start, hit.end, hit.term_idx))
        .collect();
    let hidden_matches =
        select_hidden_context_ranges(full_text, &hit_spans, missing_mask, missing_count)?;

    build_context_segments_from_ranges_unchecked(full_text, &hidden_matches, max_width)
}

fn build_context_segments_from_ranges_unchecked(
    full_text: &str,
    hidden_matches: &[(usize, usize)],
    max_width: usize,
) -> Option<String> {
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

/// One match used to compute term coverage and phrase density per cluster.
#[derive(Clone, Copy, Debug)]
struct TermHit {
    start: usize,
    end: usize,
    term_idx: usize,
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

#[derive(Debug, Default)]
struct HighlightQuery {
    unquoted: String,
    literals: Vec<Literal>,
}

impl HighlightQuery {
    fn parse(query: &str) -> Self {
        let parsed = ParsedQuery::parse(query);
        let unquoted_terms = parsed.unquoted().split_whitespace().collect::<Vec<_>>();
        let identifier_literals = unquoted_terms
            .iter()
            .copied()
            .filter(|term| term.contains('_'))
            .map(|term| Literal::new(term.to_string()))
            .collect::<Vec<_>>();
        let unquoted = unquoted_terms
            .into_iter()
            .filter(|term| !term.contains('_'))
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            unquoted: normalize_highlight_query(&unquoted),
            literals: parsed
                .literals()
                .iter()
                .cloned()
                .chain(identifier_literals)
                .collect(),
        }
    }

    fn context_text(&self) -> String {
        normalize_highlight_query(
            [self.unquoted.as_str()]
                .into_iter()
                .chain(self.literals.iter().map(Literal::text))
                .collect::<Vec<_>>()
                .join(" ")
                .as_str(),
        )
    }

    fn highlight(
        &self,
        text: &str,
        base_style: Style,
        highlight_style: Style,
    ) -> Vec<Span<'static>> {
        let mut ranges = find_normalized_match_ranges(text, &self.unquoted);
        ranges.extend(match_literal_ranges(text, &self.literals));
        highlight_ranges(text, ranges, base_style, highlight_style)
    }

    #[cfg(test)]
    fn context_specs(&self) -> Vec<ContextMatchSpec> {
        Self::normalized_context_specs(&self.unquoted)
            .into_iter()
            .chain(self.literals.iter().cloned().map(ContextMatchSpec::Literal))
            .collect()
    }

    fn literal_context_specs(&self) -> Vec<ContextMatchSpec> {
        self.literals
            .iter()
            .cloned()
            .map(ContextMatchSpec::Literal)
            .collect()
    }

    fn has_match(&self, text: &str) -> bool {
        !find_normalized_match_ranges(text, &self.unquoted).is_empty()
            || self.has_literal_match(text)
    }

    fn has_literal_match(&self, text: &str) -> bool {
        self.literals.iter().any(|literal| literal.matches(text))
    }

    fn needs_literal_context(&self, preview: &str) -> bool {
        self.literals
            .iter()
            .any(|literal| !literal.matches(preview))
    }

    fn match_ranges(&self, text: &str) -> Vec<(usize, usize)> {
        let mut ranges = find_normalized_match_ranges(text, &self.unquoted);
        ranges.extend(match_literal_ranges(text, &self.literals));
        merge_match_ranges(ranges)
    }

    #[cfg(test)]
    fn normalized_context_specs(query: &str) -> Vec<ContextMatchSpec> {
        query
            .split_whitespace()
            .map(|term| ContextMatchSpec::Normalized(term.to_string()))
            .collect()
    }
}

#[allow(dead_code)]
#[derive(Clone)]
enum ContextMatchSpec {
    Normalized(String),
    Literal(Literal),
}

impl ContextMatchSpec {
    fn key(&self) -> (&str, bool) {
        match self {
            Self::Normalized(term) => (term.as_str(), false),
            Self::Literal(literal) => (literal.text(), true),
        }
    }

    fn find_ranges(&self, text: &str, normalized: Option<&NormalizedText>) -> Vec<(usize, usize)> {
        match self {
            Self::Normalized(term) => normalized
                .map(|normalized| normalized.find_all_ranges(term))
                .unwrap_or_else(|| find_normalized_match_ranges(text, term)),
            Self::Literal(literal) => literal.match_ranges(text),
        }
    }
}

fn dedupe_context_specs(specs: &[ContextMatchSpec]) -> Vec<ContextMatchSpec> {
    let mut deduped = Vec::new();
    for spec in specs {
        let (text, is_literal) = spec.key();
        if !deduped.iter().any(|existing: &ContextMatchSpec| {
            let (existing_text, existing_is_literal) = existing.key();
            existing_is_literal == is_literal && existing_text.eq_ignore_ascii_case(text)
        }) {
            deduped.push(spec.clone());
            if deduped.len() == 64 {
                break;
            }
        }
    }
    deduped
}

fn find_all_spec_hits(
    text: &str,
    specs: &[ContextMatchSpec],
    normalized: Option<&NormalizedText>,
) -> Vec<TermHit> {
    let mut all_hits = Vec::new();
    for (term_idx, spec) in specs.iter().enumerate() {
        all_hits.extend(
            spec.find_ranges(text, normalized)
                .into_iter()
                .map(|(start, end)| TermHit {
                    start,
                    end,
                    term_idx,
                }),
        );
    }
    all_hits.sort_unstable_by_key(|hit| hit.start);
    all_hits
}

fn normalize_highlight_query(query: &str) -> String {
    normalize_for_search(query)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
fn highlight_text(
    text: &str,
    query: &str,
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    highlight_ranges(
        text,
        find_normalized_match_ranges(text, query),
        base_style,
        highlight_style,
    )
}

fn merge_match_ranges(mut ranges: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    ranges.sort_unstable_by_key(|range| range.0);
    let mut merged = Vec::<(usize, usize)>::new();
    for (start, end) in ranges {
        if start >= end {
            continue;
        }
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

fn highlight_ranges(
    text: &str,
    ranges: Vec<(usize, usize)>,
    base_style: Style,
    highlight_style: Style,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    let merged = merge_match_ranges(ranges)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Conversation;
    use crate::semantic::types::{
        SemanticChunkIdentity, SemanticExplanation, SemanticQuality, SemanticRationaleKind,
        SemanticScoreBreakdown,
    };
    use crate::tui::app::{SemanticProgress, SemanticResultMetadata, TuiSearchOptions};
    use crate::tui::semantic_worker::{SemanticSearchMessage, SemanticSearchResponse};
    use crate::tui::viewer::ToolDisplayMode;
    use chrono::TimeZone;
    use ratatui::Terminal;
    use ratatui::backend::{Backend, TestBackend};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::mpsc;

    #[test]
    fn view_help_overlay_handles_tiny_terminal() {
        for (width, height) in [(20, 8), (10, 3), (2, 2), (1, 1)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render_help_overlay(frame, true, false, false, &KeyBindings::default(), 0)
                })
                .unwrap();
        }
    }

    #[test]
    fn list_help_overlay_handles_tiny_terminal() {
        for (width, height) in [(20, 8), (10, 3), (2, 2), (1, 1)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render_help_overlay(frame, false, false, false, &KeyBindings::default(), 0)
                })
                .unwrap();
        }
    }

    fn terminal_contents(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect()
    }

    fn assert_cursor_inside(terminal: &mut Terminal<TestBackend>, width: u16) {
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert_eq!(cursor.y, 0);
        assert!(cursor.x < width, "cursor {cursor:?} outside width {width}");
    }

    fn test_conversation() -> Conversation {
        Conversation {
            source: crate::history::Source::Claude,
            session_id: "session".to_owned(),
            path: PathBuf::from("/tmp/session.jsonl"),
            index: 0,
            timestamp: Local.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
            preview: "lexical preview sentinel".to_string(),
            preview_first: "lexical preview sentinel".to_string(),
            preview_last: "lexical preview sentinel".to_string(),
            full_text: "tool output sentinel summary sentinel cwd sentinel".to_string(),
            agent_search_text: String::new(),
            semantic_route_text: String::new(),
            semantic_turns: vec!["semantic visible text".to_string()],
            semantic_turn_ranges: vec![crate::agent::refs::MessageRange::single(1)],
            search_text_lower: "lexical preview sentinel".to_string(),
            project_name: Some("project sentinel".to_string()),
            project_path: None,
            cwd: Some(PathBuf::from("/cwd/sentinel")),
            message_count: 1,
            parse_errors: Vec::new(),
            summary: Some("summary sentinel".to_string()),
            custom_title: Some("title sentinel".to_string()),
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    fn semantic_app() -> App {
        App::new_with_options(
            vec![test_conversation()],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
            TuiSearchOptions {
                default_mode: ListSearchMode::Semantic,
            },
        )
    }

    fn app_with_project_name(project_name: &str) -> App {
        let mut conversation = test_conversation();
        conversation.project_name = Some(project_name.to_string());
        conversation.custom_title = Some("semantic status title".to_string());
        conversation.summary = Some("semantic status summary".to_string());
        App::new(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        )
    }

    fn semantic_searching_app(query: &str, progress: SemanticProgress) -> App {
        let mut app = semantic_app();
        let (response_tx, response_rx) = mpsc::channel();
        app.set_query_for_test(query);
        app.set_semantic_receiver_for_test(7, response_rx);
        app.set_semantic_prewarm_generation_for_test(7);
        response_tx
            .send(SemanticSearchMessage::Progress {
                generation: 7,
                progress,
            })
            .unwrap();
        app.receive_search_results();
        app
    }

    fn test_semantic_metadata(evidence_preview: &str) -> SemanticResultMetadata {
        test_semantic_metadata_with_scores(
            evidence_preview,
            SemanticScoreBreakdown {
                hybrid: 1.0,
                semantic: 1.0,
                lexical: 0.0,
            },
            SemanticRationaleKind::SemanticOnly,
        )
    }

    fn test_semantic_metadata_with_scores(
        evidence_preview: &str,
        score_breakdown: SemanticScoreBreakdown,
        rationale_kind: SemanticRationaleKind,
    ) -> SemanticResultMetadata {
        SemanticResultMetadata {
            score_breakdown,
            explanation: SemanticExplanation {
                quality: SemanticQuality::Strong,
                quality_label: "strong",
                matched_terms: Vec::new(),
                evidence_preview: evidence_preview.to_string(),
                rationale_kind,
                chunk: SemanticChunkIdentity {
                    conversation_index: 0,
                    source: crate::semantic::types::SemanticChunkSource::VisibleDialogue,
                    session: "test-session".to_string(),
                    chunk_index: 0,
                    message_range: crate::agent::refs::MessageRange::single(1),
                },
            },
        }
    }

    #[test]
    fn search_bar_hides_transient_semantic_status_at_narrow_width() {
        let app = semantic_searching_app("你好世界widequery", SemanticProgress::Ranking);
        let width = 24;
        let backend = TestBackend::new(width, 4);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_search_bar(frame, &app, frame.area()))
            .unwrap();

        let line = row_text(&terminal, 0);
        assert_eq!(line.chars().count(), width as usize);
        assert!(!line.contains("sem ranking"), "{line:?}");
        assert!(!line.contains("sem model"), "{line:?}");
        assert!(!line.contains("sem cache"), "{line:?}");
        assert!(line.contains("1/1"), "{line:?}");
        assert_cursor_inside(&mut terminal, width);
    }

    #[test]
    fn lexical_search_bar_omits_semantic_status_at_normal_width() {
        let mut app = App::new(
            vec![test_conversation()],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );
        app.set_query_for_test("lexical query");
        let width = 80;
        let backend = TestBackend::new(width, 4);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_search_bar(frame, &app, frame.area()))
            .unwrap();

        let line = row_text(&terminal, 0);
        assert_eq!(line.chars().count(), width as usize);
        assert!(line.contains("lexical query"), "{line:?}");
        assert!(line.contains("1/1"), "{line:?}");
        assert!(!line.contains("semantic"), "{line:?}");
        assert!(!line.contains("sem "), "{line:?}");
        assert_cursor_inside(&mut terminal, width);
    }

    #[test]
    fn semantic_search_bar_keeps_query_mode_count_status_and_cursor_at_normal_width() {
        let app = semantic_searching_app(
            "vector query with enough words",
            SemanticProgress::Embedding {
                completed: 21,
                total: 42,
            },
        );
        let width = 80;
        let backend = TestBackend::new(width, 4);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_search_bar(frame, &app, frame.area()))
            .unwrap();

        let line = row_text(&terminal, 0);
        assert_eq!(line.chars().count(), width as usize);
        assert!(line.contains("vector query with enough words"), "{line:?}");
        assert!(line.contains("sem 1/1"), "{line:?}");
        assert!(line.contains("1/1"), "{line:?}");
        assert!(!line.contains("sem embedding"), "{line:?}");
        assert_cursor_inside(&mut terminal, width);
    }

    fn complete_semantic_search(app: &mut App, metadata: SemanticResultMetadata) {
        let (response_tx, response_rx) = mpsc::channel();
        app.set_semantic_receiver_for_test(7, response_rx);
        response_tx
            .send(SemanticSearchMessage::Complete(SemanticSearchResponse {
                generation: 7,
                filtered: vec![0],
                metadata: HashMap::from([(0, metadata)]),
                error: None,
                progress: SemanticProgress::Complete,
                prewarm: false,
            }))
            .unwrap();
        app.receive_search_results();
    }

    fn render_semantic_list_contents(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        terminal_contents(&terminal)
    }

    #[test]
    fn list_truncates_long_project_names_on_narrow_rows() {
        let app = app_with_project_name("claude-history/drop-semantic-feature-gate");
        let backend = TestBackend::new(70, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let first_row = row_text(&terminal, 0);
        assert!(
            first_row.contains("claude-history/drop-semantic…"),
            "{first_row:?}"
        );
        assert!(
            !first_row.contains("claude-history/drop-semantic-feature-gate"),
            "{first_row:?}"
        );
    }

    #[test]
    fn list_uses_available_width_for_custom_titles() {
        let mut conversation = test_conversation();
        conversation.project_name = Some("aven".to_string());
        conversation.custom_title = Some(
            "fork lineage alpha beta gamma delta epsilon zeta eta theta iota kappa lambda"
                .to_string(),
        );
        conversation.summary = Some("generated summary remains visible".to_string());
        let app = App::new(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );
        let backend = TestBackend::new(160, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let first_row = row_text(&terminal, 0);
        assert!(
            first_row.contains(
                "fork lineage alpha beta gamma delta epsilon zeta eta theta iota kappa lambda"
            ),
            "{first_row:?}"
        );
        assert!(
            first_row.contains("generated summary remains visible"),
            "{first_row:?}"
        );
        assert!(first_row.contains("1 msg · Jan 01, 00:00"), "{first_row:?}");
    }

    #[test]
    fn list_truncates_custom_titles_to_preserve_metadata() {
        let mut conversation = test_conversation();
        conversation.project_name = Some("aven".to_string());
        conversation.custom_title = Some(
            "fork lineage alpha beta gamma delta epsilon zeta eta theta iota kappa lambda"
                .to_string(),
        );
        conversation.summary = None;
        let app = App::new(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );
        let backend = TestBackend::new(72, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let first_row = row_text(&terminal, 0);
        assert!(
            first_row.contains("fork lineage alpha beta gamma delta e…"),
            "{first_row:?}"
        );
        assert!(first_row.contains("1 msg · Jan 01, 00:00"), "{first_row:?}");
        assert_eq!(
            UnicodeWidthStr::width(first_row.as_str()),
            72,
            "{first_row:?}"
        );
    }

    #[test]
    fn semantic_list_uses_conversation_preview_without_query() {
        let app = semantic_app();
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(
            contents.contains("lexical preview sentinel"),
            "{contents:?}"
        );
        assert!(!contents.contains("semantic visible text"), "{contents:?}");
    }

    #[test]
    fn semantic_list_uses_conversation_preview_while_query_has_no_metadata() {
        let mut app = semantic_app();
        app.set_query_for_test("sentinel");
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(
            contents.contains("lexical preview sentinel"),
            "{contents:?}"
        );
    }

    #[test]
    fn semantic_list_shows_compact_score_metadata_on_wide_rows() {
        let mut app = semantic_app();
        app.set_query_for_test("sentinel");
        complete_semantic_search(
            &mut app,
            test_semantic_metadata_with_scores(
                "semantic evidence only",
                SemanticScoreBreakdown {
                    hybrid: 1.23,
                    semantic: 1.0,
                    lexical: 0.23,
                },
                SemanticRationaleKind::LexicalBoosted,
            ),
        );
        let backend = TestBackend::new(70, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("1.23"), "{contents:?}");
        assert!(!contents.contains("strong"), "{contents:?}");
        assert!(!contents.contains("good"), "{contents:?}");
    }

    #[test]
    fn semantic_list_hides_score_metadata_on_narrow_rows() {
        let mut app = semantic_app();
        app.set_query_for_test("sentinel");
        complete_semantic_search(
            &mut app,
            test_semantic_metadata_with_scores(
                "semantic evidence only",
                SemanticScoreBreakdown {
                    hybrid: 1.23,
                    semantic: 1.0,
                    lexical: 0.23,
                },
                SemanticRationaleKind::LexicalBoosted,
            ),
        );
        let backend = TestBackend::new(69, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(!contents.contains("1.23"), "{contents:?}");
        assert!(!contents.contains("strong"), "{contents:?}");
        assert!(!contents.contains("good"), "{contents:?}");
    }

    #[test]
    fn semantic_status_bar_keeps_hotkeys_when_result_metadata_exists() {
        let mut app = App::new_with_options(
            vec![test_conversation(), test_conversation()],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
            TuiSearchOptions {
                default_mode: ListSearchMode::Semantic,
            },
        );
        app.set_query_for_test("sentinel");
        let (response_tx, response_rx) = mpsc::channel();
        app.set_semantic_receiver_for_test(7, response_rx);
        response_tx
            .send(SemanticSearchMessage::Complete(SemanticSearchResponse {
                generation: 7,
                filtered: vec![1, 0],
                metadata: HashMap::from([(
                    1,
                    test_semantic_metadata_with_scores(
                        "semantic evidence only",
                        SemanticScoreBreakdown {
                            hybrid: 1.23,
                            semantic: 0.98,
                            lexical: 0.25,
                        },
                        SemanticRationaleKind::LexicalBoosted,
                    ),
                )]),
                error: None,
                progress: SemanticProgress::Complete,
                prewarm: false,
            }))
            .unwrap();
        app.receive_search_results();
        let backend = TestBackend::new(80, 2);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list_status_bar(frame, &app, frame.area()))
            .unwrap();

        let line = row_text(&terminal, 0);
        assert!(line.contains("Enter"), "{line:?}");
        assert!(line.contains("semantic·sem"), "{line:?}");
        assert!(!line.contains("sem 0.98"), "{line:?}");
        assert!(!line.contains("lex 0.25"), "{line:?}");
        assert!(!line.contains("lex boost"), "{line:?}");
    }

    #[test]
    fn semantic_status_bar_shows_embedding_progress_before_results() {
        let app = semantic_searching_app(
            "sentinel",
            SemanticProgress::Embedding {
                completed: 21,
                total: 42,
            },
        );
        let backend = TestBackend::new(80, 2);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list_status_bar(frame, &app, frame.area()))
            .unwrap();

        let line = row_text(&terminal, 0);
        assert!(line.contains("sem embedding 50%"), "{line:?}");
        assert!(line.contains("21/42 chunks"), "{line:?}");
    }

    #[test]
    fn semantic_debug_popup_renders_score_details() {
        let mut app = semantic_app();
        app.set_query_for_test("sentinel");
        complete_semantic_search(
            &mut app,
            test_semantic_metadata_with_scores(
                "semantic evidence only",
                SemanticScoreBreakdown {
                    hybrid: 1.23,
                    semantic: 0.9,
                    lexical: 0.1,
                },
                SemanticRationaleKind::LexicalBoosted,
            ),
        );
        app.set_dialog_mode_for_test(DialogMode::SemanticDebug);
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list_mode(frame, &app))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("Semantic result"), "{contents:?}");
        assert!(contents.contains("1.23"), "{contents:?}");
        assert!(contents.contains("0.90"), "{contents:?}");
        assert!(contents.contains("lex boost"), "{contents:?}");
        assert!(contents.contains("semantic evidence only"), "{contents:?}");
    }

    #[test]
    fn highlight_query_uses_unquoted_terms_and_literals_for_context() {
        assert_eq!(
            HighlightQuery::parse("alpha \"DEPLOYMENT_TOKEN\" beta").context_text(),
            "alpha beta deployment token"
        );
    }

    #[test]
    fn highlight_query_preserves_unquoted_identifier_punctuation() {
        let highlight_style = Style::default().fg(Color::Yellow);
        let query = HighlightQuery::parse("deployment_token");
        let spans = query.highlight(
            "prefix deployment token suffix DEPLOYMENT_TOKEN",
            Style::default(),
            highlight_style,
        );
        let highlighted: Vec<_> = span_info(&spans, highlight_style)
            .into_iter()
            .filter(|(_, highlighted)| *highlighted)
            .collect();

        assert_eq!(highlighted, vec![("DEPLOYMENT_TOKEN", true)]);
    }

    #[test]
    fn quoted_list_highlighting_matches_literal_text() {
        let highlight_style = Style::default().fg(Color::Yellow);
        let query = HighlightQuery::parse("\"DEPLOYMENT_TOKEN\"");
        let spans = query.highlight(
            "prefix DEPLOYMENT_TOKEN suffix",
            Style::default(),
            highlight_style,
        );
        let highlighted: Vec<_> = span_info(&spans, highlight_style)
            .into_iter()
            .filter(|(_, highlighted)| *highlighted)
            .collect();

        assert_eq!(highlighted, vec![("DEPLOYMENT_TOKEN", true)]);
    }

    #[test]
    fn quoted_list_highlighting_matches_multiword_literal_phrase() {
        let highlight_style = Style::default().fg(Color::Yellow);
        let query = HighlightQuery::parse("alpha \"beta gamma\"");
        let spans = query.highlight(
            "alpha prefix beta gamma suffix beta-only",
            Style::default(),
            highlight_style,
        );
        let highlighted: Vec<_> = span_info(&spans, highlight_style)
            .into_iter()
            .filter(|(_, highlighted)| *highlighted)
            .collect();

        assert_eq!(highlighted, vec![("alpha", true), ("beta gamma", true)]);
    }

    #[test]
    fn quoted_list_highlighting_respects_smart_case() {
        let highlight_style = Style::default().fg(Color::Yellow);
        let query = HighlightQuery::parse("\"Beta Gamma\"");
        let spans = query.highlight(
            "beta gamma then Beta Gamma",
            Style::default(),
            highlight_style,
        );
        let highlighted: Vec<_> = span_info(&spans, highlight_style)
            .into_iter()
            .filter(|(_, highlighted)| *highlighted)
            .collect();

        assert_eq!(highlighted, vec![("Beta Gamma", true)]);
    }

    #[test]
    fn semantic_evidence_preview_highlights_query_terms() {
        let metadata = test_semantic_metadata(
            "prefix text before the important semantic needle appears near the end",
        );
        let spans = highlight_text(
            &build_match_segments(&metadata.explanation.evidence_preview, "needle", 40),
            "needle",
            Style::default(),
            Style::default().fg(Color::Yellow),
        );
        let highlighted: Vec<_> = span_info(&spans, Style::default().fg(Color::Yellow))
            .into_iter()
            .filter(|(_, highlighted)| *highlighted)
            .collect();
        assert_eq!(highlighted.len(), 1);
        assert_eq!(highlighted[0].0, "needle");
    }

    #[test]
    fn semantic_list_truncates_cleanly_at_narrow_width() {
        let mut app = semantic_app();
        app.set_query_for_test("needle");
        let evidence_preview = format!("{} needle{}", "宽字符前缀".repeat(8), "x".repeat(120));
        complete_semantic_search(
            &mut app,
            test_semantic_metadata_with_scores(
                &evidence_preview,
                SemanticScoreBreakdown {
                    hybrid: 123.45,
                    semantic: 67.89,
                    lexical: 55.56,
                },
                SemanticRationaleKind::WeakMatch,
            ),
        );
        let width = 28;
        let height = 8;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list_mode(frame, &app))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("needle"), "{contents:?}");
        let truncated = build_match_segments(
            &evidence_preview,
            "needle",
            width.saturating_sub(4) as usize,
        );
        assert!(truncated.contains("needle"), "{truncated:?}");
        assert!(
            UnicodeWidthStr::width(truncated.as_str()) <= width.saturating_sub(4) as usize,
            "{truncated:?}"
        );
        for y in 0..height {
            let line = row_text(&terminal, y);
            assert_eq!(line.chars().count(), width as usize, "{line:?}");
        }
    }

    #[test]
    fn lexical_unquoted_render_shows_cached_hidden_full_text_context() {
        let mut conversation = test_conversation();
        conversation.preview = "visible lexical preview".to_string();
        conversation.full_text =
            format!("visible lexical preview {} hiddenneedle", "x ".repeat(200));
        let evidence = crate::search::build_lexical_evidence(
            &conversation,
            &ParsedQuery::parse("hiddenneedle"),
        )
        .unwrap();
        let mut app = App::new(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );
        app.set_query_for_test("hiddenneedle");
        app.set_lexical_evidence_for_test(0, evidence);
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("hiddenneedle"), "{contents:?}");
    }

    #[test]
    fn lexical_quoted_render_shows_hidden_literal_context() {
        let mut conversation = test_conversation();
        conversation.preview = "visible lexical preview".to_string();
        conversation.full_text =
            format!("visible lexical preview {} hidden_literal", "x ".repeat(80));
        let mut app = App::new(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );
        app.set_query_for_test("\"hidden_literal\"");
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("hidden_literal"), "{contents:?}");
    }

    #[test]
    fn lexical_mixed_query_cached_context_uses_unquoted_and_literals() {
        let mut conversation = test_conversation();
        conversation.preview = "visible preview".to_string();
        conversation.full_text = format!("hidden_unquoted {} exact_literal", "x ".repeat(120));
        let evidence = crate::search::build_lexical_evidence(
            &conversation,
            &ParsedQuery::parse("hidden_unquoted \"exact_literal\""),
        )
        .unwrap();
        let ctx = build_context_segments_from_ranges(
            &conversation.full_text,
            &evidence.context_ranges,
            120,
        )
        .unwrap();

        assert!(ctx.contains("exact_literal"), "{ctx:?}");
        assert!(ctx.contains("hidden_unquoted"), "{ctx:?}");
    }

    #[test]
    fn semantic_list_uses_semantic_evidence_preview_without_full_text_context() {
        let mut app = semantic_app();
        app.set_query_for_test("sentinel");
        complete_semantic_search(&mut app, test_semantic_metadata("semantic evidence only"));
        let contents = render_semantic_list_contents(&mut app, 80, 8);
        assert!(contents.contains("semantic evidence only"), "{contents:?}");
        assert!(
            !contents.contains("lexical preview sentinel"),
            "{contents:?}"
        );
        assert!(!contents.contains("tool output sentinel"), "{contents:?}");
    }

    #[test]
    fn semantic_list_shows_literal_context_when_evidence_lacks_literal() {
        let mut conversation = test_conversation();
        conversation.full_text =
            "tool output sentinel includes audio_generation literal".to_string();
        let mut app = App::new_with_options(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
            TuiSearchOptions {
                default_mode: ListSearchMode::Semantic,
            },
        );
        app.set_query_for_test("semantic \"audio_generation\"");
        let (response_tx, response_rx) = mpsc::channel();
        app.set_semantic_receiver_for_test(7, response_rx);
        response_tx
            .send(SemanticSearchMessage::Complete(SemanticSearchResponse {
                generation: 7,
                filtered: vec![0],
                metadata: HashMap::new(),
                error: None,
                progress: SemanticProgress::Complete,
                prewarm: false,
            }))
            .unwrap();
        app.receive_search_results();
        let contents = render_semantic_list_contents(&mut app, 80, 8);
        assert!(
            contents.contains("lexical preview sentinel"),
            "{contents:?}"
        );
        assert!(contents.contains("audio_generation"), "{contents:?}");
    }

    #[test]
    fn semantic_literal_preview_uses_literal_ranges() {
        let mut app = semantic_app();
        app.set_query_for_test("\"audio_generation\"");
        complete_semantic_search(
            &mut app,
            test_semantic_metadata(
                "normalized audio generation appears early before exact audio_generation literal",
            ),
        );
        let backend = TestBackend::new(54, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("audio_generation"), "{contents:?}");
        assert!(!contents.contains("audio generation"), "{contents:?}");
    }

    #[test]
    fn semantic_literal_preview_merges_overlapping_ranges() {
        let mut app = semantic_app();
        app.set_query_for_test("audio \"audio_generation\"");
        complete_semantic_search(
            &mut app,
            test_semantic_metadata("prefix audio_generation literal near the front"),
        );
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("audio_generation"), "{contents:?}");
    }

    #[test]
    fn semantic_literal_context_requires_all_literals_visible() {
        let mut conversation = test_conversation();
        conversation.full_text = "alpha_exact near preview. beta_exact hidden deeper.".to_string();
        let mut app = App::new_with_options(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
            TuiSearchOptions {
                default_mode: ListSearchMode::Semantic,
            },
        );
        app.set_query_for_test("semantic \"alpha_exact\" \"beta_exact\"");
        complete_semantic_search(
            &mut app,
            test_semantic_metadata("semantic alpha_exact only"),
        );
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("alpha_exact"), "{contents:?}");
        assert!(contents.contains("beta_exact"), "{contents:?}");
    }

    #[test]
    fn semantic_shortcut_appears_only_when_available() {
        let backend = TestBackend::new(70, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_help_overlay(frame, false, false, false, &KeyBindings::default(), 0)
            })
            .unwrap();
        let unavailable = terminal_contents(&terminal);

        let backend = TestBackend::new(70, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_help_overlay(frame, false, false, true, &KeyBindings::default(), 0)
            })
            .unwrap();
        let available = terminal_contents(&terminal);

        assert!(
            !unavailable.contains("Toggle semantic search"),
            "{unavailable:?}"
        );
        assert!(
            available.contains("Toggle semantic search"),
            "{available:?}"
        );
    }

    #[test]
    fn help_overlay_indicates_hidden_rows() {
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_help_overlay(frame, true, false, false, &KeyBindings::default(), 0)
            })
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("↓ more"), "{contents:?}");
        assert!(contents.contains("1-"), "{contents:?}");
    }

    #[test]
    fn help_overlay_scrolls_to_later_rows() {
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_help_overlay(frame, true, false, false, &KeyBindings::default(), 10)
            })
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(
            contents.contains("↑↓ more") || contents.contains("↑ more"),
            "{contents:?}"
        );
        assert!(contents.contains("11-"), "{contents:?}");
    }

    #[test]
    fn export_menus_handle_tiny_terminal() {
        for is_yank in [false, true] {
            for (width, height) in [(20, 8), (10, 3), (2, 2), (1, 1)] {
                let backend = TestBackend::new(width, height);
                let mut terminal = Terminal::new(backend).unwrap();
                terminal
                    .draw(|frame| render_export_menu(frame, 0, is_yank))
                    .unwrap();
            }
        }
    }

    #[test]
    fn centered_modal_area_preserves_fitting_size() {
        let area = centered_modal_area(Rect::new(0, 0, 80, 24), 35, 8);
        assert_eq!(area, Rect::new(22, 8, 35, 8));
    }

    #[test]
    fn centered_modal_area_clamps_to_frame() {
        assert_eq!(
            centered_modal_area(Rect::new(0, 0, 20, 24), 35, 8),
            Rect::new(0, 8, 20, 8)
        );
        assert_eq!(
            centered_modal_area(Rect::new(0, 0, 80, 3), 35, 8),
            Rect::new(22, 0, 35, 3)
        );
        assert_eq!(
            centered_modal_area(Rect::new(0, 0, 10, 3), 35, 8),
            Rect::new(0, 0, 10, 3)
        );
    }

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

    #[test]
    fn context_segments_prefers_adjacent_phrase_over_distant_terms() {
        // "audio" and "generation" each appear early in unrelated boilerplate,
        // and the literal phrase "audio generation" appears much later. The
        // snippet must surface the phrase, not the early independent hits.
        let mut full_text = String::new();
        full_text.push_str("Card generation is supported. -field:Audio is a filter. ");
        full_text.push_str(&"junk ".repeat(20));
        full_text
            .push_str("First-class audio generation (OpenAI TTS) and image support is missing. ");
        full_text.push_str(&"junk ".repeat(20));
        let preview = "Some unrelated preview line about deck workflow";
        let result = build_context_segments(&full_text, preview, "audio generation", 120);
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(
            ctx.contains("audio generation"),
            "context should contain the literal phrase 'audio generation', got: {ctx}"
        );
    }

    #[test]
    fn context_segments_phrase_inside_markdown_bold() {
        // The phrase appears inside `**...**` (markdown bold). Adjacency must
        // still be detected — markdown punctuation is a word boundary.
        let mut full_text = String::new();
        full_text.push_str("audio is mentioned. generation is mentioned separately. ");
        full_text.push_str(&"x ".repeat(30));
        full_text.push_str("**Audio generation** is the actual recommendation here.");
        let preview = "boring preview text";
        let result = build_context_segments(&full_text, preview, "audio generation", 120);
        assert!(result.is_some());
        let ctx = result.unwrap();
        let lower = ctx.to_lowercase();
        assert!(
            lower.contains("audio generation"),
            "context should contain the phrase 'audio generation', got: {ctx}"
        );
    }

    #[test]
    fn context_segments_skips_clusters_only_covering_visible_terms() {
        // "audio" is in the preview already. A standalone "audio" cluster
        // should not be selected — the snippet should surface "generation"
        // (the missing term), preferably alongside "audio" if a phrase exists.
        let full_text = "audio first. then later generation alone. and finally audio generation together at the end.";
        let preview = "audio first. then later";
        let result = build_context_segments(full_text, preview, "audio generation", 120);
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(
            ctx.contains("generation"),
            "context should contain 'generation', got: {ctx}"
        );
    }

    #[test]
    fn context_segments_distant_terms_dont_count_as_adjacent() {
        // Two clusters with the same unique_count, but only one has the
        // terms actually adjacent. The adjacent one must win.
        let mut full_text = String::new();
        // Cluster A: alpha and beta with junk between (within merge_gap=50
        // bytes but not adjacent — `aaaa` is alphanumeric in the gap).
        full_text.push_str("alpha aaaa bbbb cccc dddd beta ");
        full_text.push_str(&"x ".repeat(40));
        // Cluster B: literal phrase
        full_text.push_str("alpha beta together here");
        let preview = "boring preview line";
        let result = build_context_segments(&full_text, preview, "alpha beta", 100);
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(
            ctx.contains("alpha beta together")
                || ctx.ends_with("here")
                || ctx.contains("beta together"),
            "expected the literal phrase cluster to be selected, got: {ctx}"
        );
    }

    #[test]
    fn context_segments_dedupes_query_terms() {
        // Repeated query terms must not inflate uniqueness/adjacency math.
        let full_text = "alpha alpha and later beta the end";
        let preview = "preview only";
        let r1 = build_context_segments(full_text, preview, "alpha alpha beta", 80);
        let r2 = build_context_segments(full_text, preview, "alpha beta", 80);
        // Should produce the same context — duplicates are folded away.
        assert_eq!(r1, r2);
    }

    #[test]
    fn context_segments_underscore_phrase_still_detected() {
        // Underscores normalize to spaces, so `audio_generation` should be
        // detected as the adjacent phrase `audio generation`.
        let mut full_text = String::new();
        full_text.push_str("audio early. generation early. ");
        full_text.push_str(&"x ".repeat(30));
        full_text.push_str("the relevant audio_generation pipeline lives here.");
        let preview = "preview only";
        let result = build_context_segments(&full_text, preview, "audio generation", 120);
        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(
            ctx.contains("audio_generation") || ctx.contains("audio generation"),
            "context should contain the adjacent occurrence, got: {ctx}"
        );
    }

    #[test]
    fn context_query_quoted_literal_preserves_punctuation_contract() {
        let query = HighlightQuery::parse("\"audio_generation\"");
        let mut full_text = String::new();
        full_text.push_str("punctuation-stripped audio generation appears early. ");
        full_text.push_str(&"x ".repeat(30));
        full_text.push_str("the exact audio_generation literal appears here.");
        let preview = "preview only";

        let result = build_context_segments_for_query(&full_text, preview, &query, 120);

        assert!(result.is_some());
        let ctx = result.unwrap();
        assert!(ctx.contains("audio_generation"), "got: {ctx}");
        assert!(
            !ctx.contains("audio generation appears early"),
            "got: {ctx}"
        );
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

    /// The transcript and app used by the note-selection chrome tests: one
    /// user message, viewed as a single file.
    fn app_viewing_one_message(dir: &std::path::Path) -> App {
        let transcript = dir.join("abc.jsonl");
        std::fs::write(
            &transcript,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}"#,
                "\n",
            ),
        )
        .unwrap();
        let mut app = App::new_single_file(
            transcript,
            ToolDisplayMode::Hidden,
            false,
            KeyBindings::default(),
        );
        // The single-file view opens at width 0 and renders no lines until a
        // frame reports its size, so the gutter column would have nothing to
        // sit beside.
        app.check_view_resize(60, 4);
        app
    }

    #[test]
    fn a_selected_note_swaps_the_status_row_for_its_own_keys() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app_viewing_one_message(root.path());
        app.select_annotation_for_test("note-1", 0, 1);

        let AppMode::View(state) = app.app_mode() else {
            panic!("still in view mode");
        };
        let backend = TestBackend::new(100, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_view_status_bar(frame, &app, state, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("edit note"), "{contents:?}");
        assert!(contents.contains("elete note"), "{contents:?}");
        assert!(contents.contains("ank note"), "{contents:?}");
        // e is bound to export until a selection takes it: naming export here
        // would print a binding that does not fire.
        assert!(!contents.contains("xport"), "{contents:?}");
    }

    #[test]
    fn a_selected_note_draws_the_focus_gutter() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app_viewing_one_message(root.path());
        app.select_annotation_for_test("note-1", 0, 1);

        let AppMode::View(state) = app.app_mode() else {
            panic!("still in view mode");
        };
        // message_nav_active stays false, the state a note click leaves behind.
        assert!(!state.message_nav_active);

        let backend = TestBackend::new(60, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_view_content(frame, state, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains('\u{258c}'), "{contents:?}");
    }

    #[test]
    fn a_list_row_states_its_note_count_beside_the_other_metadata() {
        let conversation = test_conversation();
        let path = conversation.path.clone();
        let mut app = App::new(
            vec![conversation],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );
        app.set_annotation_count_for_test(&path, 2);

        let backend = TestBackend::new(120, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(contents.contains("2 notes"), "{contents:?}");
        // The count joins the existing metadata rather than displacing it.
        assert!(contents.contains("1 msg"), "{contents:?}");
    }

    #[test]
    fn a_list_row_without_notes_states_no_count() {
        let app = App::new(
            vec![test_conversation()],
            ToolDisplayMode::Truncated,
            false,
            KeyBindings::default(),
            vec![],
        );

        let backend = TestBackend::new(120, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_list(frame, &app, frame.area()))
            .unwrap();

        let contents = terminal_contents(&terminal);
        assert!(!contents.contains("note"), "{contents:?}");
    }
}
