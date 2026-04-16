//! Conversation viewer rendering for TUI display.
//!
//! This module renders conversation JSONL files to `Vec<RenderedLine>` for display
//! in the TUI viewer. It produces styled spans that ratatui can render directly,
//! without using ANSI escape codes.

use crate::claude::{self, AssistantMessage, ContentBlock, LogEntry, UserContent};
use crate::tool_format;
use crate::tui::app::{LineStyle, RenderedLine};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use unicode_width::UnicodeWidthStr;

use crate::tui::theme::{self, Theme};

/// Width of the focus gutter indicator (▌ + space)
pub const GUTTER_WIDTH: usize = 2;

const NAME_WIDTH: usize = 9;
/// Width of timestamp prefix when timing is enabled (space + HH:MM + space)
const TIMESTAMP_WIDTH: usize = 7;

/// Get the current theme (cached after first detection)
fn th() -> &'static Theme {
    theme::detect_theme()
}

/// Maximum body lines shown in truncated tool call mode
const TRUNCATED_BODY_LINES: usize = 3;
/// Maximum result lines shown in truncated tool result mode
const TRUNCATED_RESULT_LINES: usize = 4;

/// Controls how tool calls and results are displayed
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolDisplayMode {
    Hidden,
    #[default]
    Truncated,
    Full,
}

impl ToolDisplayMode {
    /// Cycle to the next mode: Hidden → Truncated → Full → Hidden
    pub fn next(self) -> Self {
        match self {
            Self::Hidden => Self::Truncated,
            Self::Truncated => Self::Full,
            Self::Full => Self::Hidden,
        }
    }

    /// Whether tools should be rendered at all
    pub fn is_visible(self) -> bool {
        !matches!(self, Self::Hidden)
    }

    /// Fixed-width label for the status bar (3 chars each)
    pub fn status_label(self) -> &'static str {
        match self {
            Self::Hidden => "off",
            Self::Truncated => "trn",
            Self::Full => "all",
        }
    }
}

/// Options for rendering a conversation
pub struct RenderOptions {
    pub tool_display: ToolDisplayMode,
    pub show_thinking: bool,
    pub show_timing: bool,
    pub content_width: usize,
}

/// Tracks the line range of a single message (User or Assistant entry) in the rendered output
#[derive(Clone, Debug)]
pub struct MessageRange {
    /// Index of the JSONL entry (line number in the file, 0-based, counting only parsed entries)
    pub entry_index: usize,
    /// Start line in rendered output (inclusive)
    pub start_line: usize,
    /// End line in rendered output (exclusive, excludes trailing blank)
    pub end_line: usize,
}

/// Result of rendering a conversation
pub struct RenderedConversation {
    pub lines: Vec<RenderedLine>,
    pub messages: Vec<MessageRange>,
}

/// Format an ISO 8601 timestamp to HH:MM local time
fn format_timestamp(iso_timestamp: &str) -> Option<String> {
    use chrono::{DateTime, Local};
    // Parse RFC 3339 timestamp (handles timezone offsets) and convert to local time
    DateTime::parse_from_rfc3339(iso_timestamp)
        .ok()
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
}

/// Render a conversation file to lines for display in the TUI viewer
pub fn render_conversation(
    file_path: &Path,
    options: &RenderOptions,
) -> std::io::Result<RenderedConversation> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    let mut messages = Vec::new();
    let mut entry_index: usize = 0;
    for line_result in reader.lines() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }

        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            let is_message = matches!(entry, LogEntry::User { .. } | LogEntry::Assistant { .. })
                || matches!(&entry, LogEntry::Progress { data, .. }
                    if options.show_thinking && crate::claude::parse_agent_progress(data).is_some());
            let start_line = lines.len();
            render_entry(&mut lines, &entry, options);
            let end_line = lines.len();

            // Track message ranges (exclude trailing blank line from the range)
            if is_message && end_line > start_line {
                let effective_end = if end_line > 0
                    && lines.get(end_line - 1).is_some_and(|l| l.spans.is_empty())
                {
                    end_line - 1
                } else {
                    end_line
                };
                if effective_end > start_line {
                    messages.push(MessageRange {
                        entry_index,
                        start_line,
                        end_line: effective_end,
                    });
                }
            }
            entry_index += 1;
        }
    }

    // Collapse consecutive empty lines into single empty lines.
    // Multiple render functions each add trailing empty lines, which can
    // result in double blanks when a tool result has empty output.
    // After dedup, remap message ranges to account for removed lines.
    let mut removed = vec![false; lines.len()];
    let mut i = 1;
    while i < lines.len() {
        if lines[i].spans.is_empty() && lines[i - 1].spans.is_empty() {
            removed[i] = true;
            i += 1;
        } else {
            i += 1;
        }
    }

    // Build index mapping: old line index -> new line index
    let mut new_index = Vec::with_capacity(lines.len());
    let mut offset = 0usize;
    for (idx, &is_removed) in removed.iter().enumerate() {
        if is_removed {
            new_index.push(idx - offset); // won't be used, but fill for completeness
            offset += 1;
        } else {
            new_index.push(idx - offset);
        }
    }
    let total_after = lines.len() - offset;

    // Remove the marked lines
    {
        let mut write = 0;
        for (read, &is_removed) in removed.iter().enumerate() {
            if !is_removed {
                if write != read {
                    lines.swap(write, read);
                }
                write += 1;
            }
        }
        lines.truncate(total_after);
    }

    // Remap message ranges
    for msg in &mut messages {
        msg.start_line = new_index[msg.start_line];
        // end_line is exclusive, so map the last included line and add 1
        if msg.end_line > 0 && msg.end_line <= new_index.len() {
            // Find the new index of the last non-removed line before end_line
            let mut last = msg.end_line - 1;
            while last > msg.start_line && removed[last] {
                last -= 1;
            }
            msg.end_line = new_index[last] + 1;
        } else if msg.end_line == new_index.len() {
            msg.end_line = total_after;
        }
        // Clamp
        msg.end_line = msg.end_line.min(total_after);
        msg.start_line = msg.start_line.min(msg.end_line);
    }

    // Remove empty ranges
    messages.retain(|m| m.start_line < m.end_line);

    Ok(RenderedConversation { lines, messages })
}

fn render_entry(lines: &mut Vec<RenderedLine>, entry: &LogEntry, options: &RenderOptions) {
    match entry {
        LogEntry::Summary { .. }
        | LogEntry::FileHistorySnapshot { .. }
        | LogEntry::System { .. }
        | LogEntry::CustomTitle { .. } => {}
        LogEntry::Progress { data, .. } => {
            // Handle agent_progress entries (only when show_thinking is enabled)
            if options.show_thinking
                && let Some(agent_progress) = crate::claude::parse_agent_progress(data)
            {
                render_agent_message(lines, &agent_progress, options);
            }
        }
        LogEntry::User {
            message,
            timestamp,
            parent_tool_use_id,
            ..
        } => {
            // Subagent messages: show nested when show_thinking, skip otherwise
            if parent_tool_use_id.is_some() && !options.show_thinking {
                return;
            }
            let ts = if options.show_timing {
                timestamp.as_deref().and_then(format_timestamp)
            } else {
                None
            };
            render_user_message(
                lines,
                message,
                options,
                ts.as_deref(),
                parent_tool_use_id.as_deref(),
            );
        }
        LogEntry::Assistant {
            message,
            timestamp,
            parent_tool_use_id,
            ..
        } => {
            // Subagent messages: show nested when show_thinking, skip otherwise
            if parent_tool_use_id.is_some() && !options.show_thinking {
                return;
            }
            let ts = if options.show_timing {
                timestamp.as_deref().and_then(format_timestamp)
            } else {
                None
            };
            render_assistant_message(
                lines,
                message,
                options,
                ts.as_deref(),
                parent_tool_use_id.as_deref(),
            );
        }
    }
}

fn render_user_message(
    lines: &mut Vec<RenderedLine>,
    message: &crate::claude::UserMessage,
    options: &RenderOptions,
    timestamp: Option<&str>,
    parent_id: Option<&str>,
) {
    let mut printed = false;
    let mut ts_remaining = timestamp;
    let nested_label = parent_id.map(subagent_label);

    // Detect if this is a skill invocation message
    let is_skill = match &message.content {
        UserContent::String(s) => s.trim().starts_with("Base directory for this skill:"),
        UserContent::Blocks(blocks) => blocks.iter().any(|block| {
            matches!(block, ContentBlock::Text { text } if text.trim().starts_with("Base directory for this skill:"))
        }),
    };

    // Extract text from user message, collecting all text blocks
    let text = match &message.content {
        UserContent::String(s) => process_command_message(s),
        UserContent::Blocks(blocks) => {
            let texts: Vec<String> = blocks
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        process_command_message(text)
                    } else {
                        None
                    }
                })
                .collect();
            if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n\n"))
            }
        }
    };

    if let Some(text) = text {
        let md_lines = render_markdown_to_lines(&text, options.content_width);
        if let Some(ref label) = nested_label {
            render_ledger_block_styled_dimmed(
                lines,
                label,
                th().text_primary,
                md_lines,
                options.show_timing,
            );
        } else if is_skill {
            render_ledger_block_styled_dimmed(
                lines,
                "You",
                th().text_primary,
                md_lines,
                options.show_timing,
            );
        } else {
            render_ledger_block_styled(
                lines,
                "You",
                th().text_primary,
                true,
                md_lines,
                ts_remaining,
            );
        }
        printed = true;
        ts_remaining = None;
    }

    // Tool results (if enabled)
    if options.tool_display.is_visible()
        && let UserContent::Blocks(blocks) = &message.content
    {
        for block in blocks {
            if let ContentBlock::ToolResult { content, .. } = block {
                if nested_label.is_some() {
                    // Dimmed tool result for subagent
                    let content_str = format_tool_result_content(content.as_ref());
                    render_ledger_block_plain_dimmed(
                        lines,
                        "  ↳ Tool",
                        th().accent_dim,
                        "<Result>",
                        options.show_timing,
                    );
                    if options.tool_display == ToolDisplayMode::Truncated {
                        let content_lines: Vec<&str> = content_str.lines().collect();
                        let total = content_lines.len();
                        if total > TRUNCATED_RESULT_LINES {
                            let truncated = content_lines[..TRUNCATED_RESULT_LINES].join("\n");
                            render_continuation_dimmed(lines, &truncated, options.show_timing);
                            render_truncation_indicator(
                                lines,
                                total - TRUNCATED_RESULT_LINES,
                                true,
                                options.show_timing,
                            );
                        } else {
                            render_continuation_dimmed(lines, &content_str, options.show_timing);
                        }
                    } else {
                        render_continuation_dimmed(lines, &content_str, options.show_timing);
                    }
                } else {
                    let content_str = match extract_tool_result_text(content.as_ref()) {
                        Some(text) => text,
                        None => format_tool_result_content(content.as_ref()),
                    };
                    // Pass timestamp to first tool result if no text block consumed it
                    let ts = if ts_remaining.is_some() {
                        let t = ts_remaining;
                        ts_remaining = None;
                        t
                    } else if options.show_timing {
                        Some("     ")
                    } else {
                        None
                    };
                    render_tool_result(
                        lines,
                        &content_str,
                        options.content_width,
                        ts,
                        options.tool_display,
                    );
                }
                printed = true;
            }
        }
    }

    if printed {
        lines.push(RenderedLine { spans: vec![] }); // Empty line after message
    }
}

/// Extract text content from tool result for markdown rendering.
/// Returns Some(text) if content is a string or array of text blocks.
/// Returns None for JSON structures that should be pretty-printed instead.
fn extract_tool_result_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(arr)) => {
            // Handle array of content blocks (e.g., [{type: "text", text: "..."}])
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect();
            if !texts.is_empty() {
                Some(texts.join("\n\n"))
            } else {
                None // Array without text blocks - render as JSON
            }
        }
        _ => None, // Objects, null, etc. - render as JSON
    }
}

/// Format tool result content to a string for display (non-text content)
fn format_tool_result_content(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(value) => {
            if let Ok(formatted) = serde_json::to_string_pretty(value) {
                formatted
            } else {
                "<invalid content>".to_string()
            }
        }
        None => "<no content>".to_string(),
    }
}

fn render_assistant_message(
    lines: &mut Vec<RenderedLine>,
    message: &AssistantMessage,
    options: &RenderOptions,
    timestamp: Option<&str>,
    parent_id: Option<&str>,
) {
    let mut printed = false;
    let mut ts_remaining = timestamp;
    let nested_label = parent_id.map(subagent_label);

    // Text blocks
    for block in &message.content {
        if let ContentBlock::Text { text } = block {
            if text.trim().is_empty() {
                continue;
            }
            let md_lines = render_markdown_to_lines(text, options.content_width);
            if let Some(ref label) = nested_label {
                render_ledger_block_styled_dimmed(
                    lines,
                    label,
                    th().accent,
                    md_lines,
                    options.show_timing,
                );
            } else {
                render_ledger_block_styled(
                    lines,
                    "Claude",
                    th().accent,
                    true,
                    md_lines,
                    ts_remaining,
                );
            }
            printed = true;
            // After first block consumes the timestamp, use blank padding for alignment
            if ts_remaining.is_some() {
                ts_remaining = None;
            }
        }
    }

    // Tool calls (if enabled)
    if options.tool_display.is_visible() {
        for block in &message.content {
            if let ContentBlock::ToolUse { name, input, .. } = block {
                if let Some(ref label) = nested_label {
                    let align_ts = if options.show_timing {
                        Some("     ")
                    } else {
                        None
                    };
                    render_tool_call(
                        lines,
                        name,
                        input,
                        label,
                        th().accent_dim,
                        true,
                        options.content_width,
                        align_ts,
                        options.tool_display,
                    );
                } else {
                    // Pass timestamp to first tool call if no text block consumed it
                    let ts = if ts_remaining.is_some() {
                        let t = ts_remaining;
                        ts_remaining = None;
                        t
                    } else if options.show_timing {
                        Some("     ")
                    } else {
                        None
                    };
                    render_tool_call(
                        lines,
                        name,
                        input,
                        "Claude",
                        th().accent_dim,
                        false,
                        options.content_width,
                        ts,
                        options.tool_display,
                    );
                }
                printed = true;
            }
        }
    }

    // Thinking blocks (if enabled, skip for subagents)
    if options.show_thinking && nested_label.is_none() {
        for block in &message.content {
            if let ContentBlock::Thinking { thinking, .. } = block {
                if thinking.is_empty() {
                    continue;
                }
                let md_lines = render_markdown_to_lines(thinking, options.content_width);
                let styled_lines = apply_thinking_style(md_lines);
                // Pass timestamp if no previous block consumed it
                let ts = if ts_remaining.is_some() {
                    let t = ts_remaining;
                    ts_remaining = None;
                    t
                } else if options.show_timing {
                    Some("     ")
                } else {
                    None
                };
                render_ledger_block_styled(
                    lines,
                    "Thinking",
                    th().accent_dim,
                    false,
                    styled_lines,
                    ts,
                );
                printed = true;
            }
        }
    }

    if printed {
        lines.push(RenderedLine { spans: vec![] });
    }
}

/// A line with styled spans from markdown rendering
struct StyledLine {
    spans: Vec<(String, LineStyle)>,
}

/// Render markdown text to styled lines for TUI display
fn render_markdown_to_lines(input: &str, max_width: usize) -> Vec<StyledLine> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(input, options);
    let mut renderer = TuiMarkdownRenderer::new(max_width);

    for event in parser {
        renderer.handle_event(event);
    }

    renderer.finish()
}

struct TuiTableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
}

impl TuiTableState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
        }
    }
}

struct TuiMarkdownRenderer {
    lines: Vec<StyledLine>,
    current_line: Vec<(String, LineStyle)>,
    max_width: usize,
    current_width: usize,
    style_stack: Vec<MarkdownStyle>,
    list_stack: Vec<ListContext>,
    in_code_block: bool,
    code_block_content: String,
    code_block_lang: String,
    in_list_item_start: bool, // Suppress paragraph blank line right after list bullet
    table_state: Option<TuiTableState>,
}

#[derive(Clone)]
enum MarkdownStyle {
    Bold,
    Italic,
    Strikethrough,
    Quote,
    Link,
    Heading,
}

#[derive(Clone)]
struct ListContext {
    index: Option<u64>,
    depth: usize,
}

impl TuiMarkdownRenderer {
    fn new(max_width: usize) -> Self {
        Self {
            lines: Vec::new(),
            current_line: Vec::new(),
            max_width,
            current_width: 0,
            style_stack: Vec::new(),
            list_stack: Vec::new(),
            in_code_block: false,
            code_block_content: String::new(),
            code_block_lang: String::new(),
            in_list_item_start: false,
            table_state: None,
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.inline_code(&code),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.rule(),
            // Render HTML blocks/inline as plain text so that XML-like tags
            // (e.g. <analysis>, <system-reminder>) are not silently dropped.
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                // Don't add blank line if we just started a list item (bullet is on same line)
                if !self.in_list_item_start
                    && (!self.lines.is_empty() || !self.current_line.is_empty())
                {
                    self.ensure_blank_line();
                }
                self.in_list_item_start = false;
            }
            Tag::Heading { .. } => {
                self.ensure_blank_line();
                self.style_stack.push(MarkdownStyle::Heading);
            }
            Tag::CodeBlock(kind) => {
                self.ensure_blank_line();
                self.in_code_block = true;
                self.code_block_content.clear();
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block_lang = lang.clone();
                let fence = if lang.is_empty() {
                    "```".to_string()
                } else {
                    format!("```{}", lang)
                };
                self.push_styled_text(
                    &fence,
                    LineStyle {
                        dimmed: true,
                        ..Default::default()
                    },
                );
                self.flush_line();
            }
            Tag::List(start) => {
                // Add blank line before top-level lists only
                if self.list_stack.is_empty() {
                    self.ensure_blank_line();
                } else {
                    self.flush_line();
                }
                let depth = self.list_stack.len();
                self.list_stack.push(ListContext {
                    index: start,
                    depth,
                });
            }
            Tag::Item => {
                self.flush_line();
                // Extract values from list context before calling methods
                let (indent, bullet) = if let Some(ctx) = self.list_stack.last_mut() {
                    let indent = "  ".repeat(ctx.depth);
                    let bullet = match &mut ctx.index {
                        None => (format!("{}- ", indent), false),
                        Some(n) => {
                            let b = format!("{}{}. ", indent, n);
                            *n += 1;
                            (b, true)
                        }
                    };
                    (Some(indent), Some(bullet))
                } else {
                    (None, None)
                };
                if let Some((text, is_numbered)) = bullet {
                    let style = if is_numbered {
                        LineStyle {
                            dimmed: true,
                            ..Default::default()
                        }
                    } else {
                        LineStyle::default()
                    };
                    self.push_styled_text(&text, style);
                }
                let _ = indent; // Mark as intentionally unused
                self.in_list_item_start = true; // Next paragraph shouldn't add blank line
            }
            Tag::Emphasis => self.style_stack.push(MarkdownStyle::Italic),
            Tag::Strong => self.style_stack.push(MarkdownStyle::Bold),
            Tag::Strikethrough => self.style_stack.push(MarkdownStyle::Strikethrough),
            Tag::BlockQuote(_) => {
                self.ensure_blank_line();
                self.push_styled_text(
                    "> ",
                    LineStyle {
                        fg: Some(th().green),
                        ..Default::default()
                    },
                );
                self.style_stack.push(MarkdownStyle::Quote);
            }
            Tag::Link { .. } => {
                self.style_stack.push(MarkdownStyle::Link);
            }
            Tag::Table(_) => {
                self.ensure_blank_line();
                self.table_state = Some(TuiTableState::new());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(ref mut state) = self.table_state {
                    state.current_row = Vec::new();
                }
            }
            Tag::TableCell => {
                if let Some(ref mut state) = self.table_state {
                    state.current_cell = String::new();
                }
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_line();
            }
            TagEnd::Heading(_) => {
                self.style_stack.pop();
                self.flush_line();
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                let code_content = std::mem::take(&mut self.code_block_content);
                let code_content = crate::markdown::wrap_code_lines(&code_content, self.max_width);

                // Try syntax highlighting first
                if let Some(highlighted_lines) =
                    crate::syntax::highlight_code_tui(&code_content, &self.code_block_lang)
                {
                    for line_tokens in highlighted_lines {
                        for token in line_tokens {
                            let style = LineStyle {
                                fg: Some(token.fg),
                                bold: token.bold,
                                italic: token.italic,
                                dimmed: false,
                            };
                            // Strip trailing newline from token text
                            let text = token.text.trim_end_matches('\n');
                            self.push_styled_text(text, style);
                        }
                        self.flush_line();
                    }
                } else {
                    // Fallback: uniform color for unknown languages
                    for code_line in code_content.lines() {
                        self.push_styled_text(
                            code_line,
                            LineStyle {
                                fg: Some(th().code_color),
                                ..Default::default()
                            },
                        );
                        self.flush_line();
                    }
                }

                // Closing fence
                self.push_styled_text(
                    "```",
                    LineStyle {
                        dimmed: true,
                        ..Default::default()
                    },
                );
                self.flush_line();
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.in_list_item_start = false; // Clear flag when list ends
                // Add blank line after top-level lists
                if self.list_stack.is_empty() {
                    self.ensure_blank_line();
                }
            }
            TagEnd::Item => {
                self.flush_line();
                self.in_list_item_start = false; // Clear flag when item ends
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.style_stack.pop();
            }
            TagEnd::BlockQuote(_) => {
                self.style_stack.pop();
                self.flush_line();
            }
            TagEnd::Table => {
                if let Some(state) = self.table_state.take() {
                    let table_lines = render_table_styled(&state.rows);
                    self.lines.extend(table_lines);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(ref mut state) = self.table_state {
                    let row = std::mem::take(&mut state.current_row);
                    state.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                if let Some(ref mut state) = self.table_state {
                    let cell = std::mem::take(&mut state.current_cell);
                    state.current_row.push(cell);
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        if let Some(ref mut state) = self.table_state {
            state.current_cell.push_str(&text.replace('\n', " "));
            return;
        }

        if self.in_code_block {
            self.code_block_content.push_str(text);
            return;
        }

        let style = self.current_style();

        // Handle text wrapping
        for word in text.split_inclusive(char::is_whitespace) {
            let word_width = word.width();

            // Check if we need to wrap
            if self.current_width + word_width > self.max_width && self.current_width > 0 {
                self.flush_line();
                // Add list indent on continuation
                if let Some(ctx) = self.list_stack.last() {
                    let indent = "  ".repeat(ctx.depth + 1);
                    self.push_styled_text(&indent, LineStyle::default());
                }
            }

            self.push_styled_text(word, style.clone());
        }
    }

    fn inline_code(&mut self, code: &str) {
        if let Some(ref mut state) = self.table_state {
            state.current_cell.push_str(code);
            return;
        }

        // Wrap to next line if the code won't fit on the current line
        let code_width = code.width();
        if self.current_width + code_width > self.max_width && self.current_width > 0 {
            self.flush_line();
            if let Some(ctx) = self.list_stack.last() {
                let indent = "  ".repeat(ctx.depth + 1);
                self.push_styled_text(&indent, LineStyle::default());
            }
        }

        self.push_styled_text(
            code,
            LineStyle {
                fg: Some(th().code_color),
                ..Default::default()
            },
        );
    }

    fn soft_break(&mut self) {
        // A single newline in markdown is a soft break — treat as space
        self.text(" ");
    }

    fn hard_break(&mut self) {
        self.flush_line();
    }

    fn rule(&mut self) {
        self.ensure_blank_line();
        let rule = "─".repeat(self.max_width.min(40));
        self.push_styled_text(
            &rule,
            LineStyle {
                dimmed: true,
                ..Default::default()
            },
        );
        self.flush_line();
    }

    fn push_styled_text(&mut self, text: &str, style: LineStyle) {
        if !text.is_empty() {
            self.current_line.push((text.to_string(), style));
            self.current_width += text.width();
        }
    }

    fn flush_line(&mut self) {
        if !self.current_line.is_empty() {
            self.lines.push(StyledLine {
                spans: std::mem::take(&mut self.current_line),
            });
        }
        self.current_width = 0;
    }

    fn ensure_blank_line(&mut self) {
        self.flush_line();
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.lines.push(StyledLine { spans: vec![] });
        }
    }

    fn current_style(&self) -> LineStyle {
        let mut style = LineStyle::default();

        for s in &self.style_stack {
            match s {
                MarkdownStyle::Bold => style.bold = true,
                MarkdownStyle::Italic => {
                    style.italic = true;
                }
                MarkdownStyle::Strikethrough => style.dimmed = true,
                MarkdownStyle::Quote => style.fg = Some(th().green),
                MarkdownStyle::Link => style.fg = Some(th().blue),
                MarkdownStyle::Heading => {
                    style.bold = true;
                    style.fg = Some(th().heading);
                }
            }
        }

        style
    }

    fn finish(mut self) -> Vec<StyledLine> {
        self.flush_line();
        // Remove trailing empty lines
        while self.lines.last().is_some_and(|l| l.spans.is_empty()) {
            self.lines.pop();
        }
        self.lines
    }
}

/// Render a table as styled lines with box-drawing characters
fn render_table_styled(rows: &[Vec<String>]) -> Vec<StyledLine> {
    if rows.is_empty() {
        return Vec::new();
    }

    let dim_style = LineStyle {
        dimmed: true,
        ..Default::default()
    };

    let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_widths = vec![0usize; num_cols];

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < num_cols {
                col_widths[i] = col_widths[i].max(cell.trim().width());
            }
        }
    }

    let mut lines = Vec::new();

    // Build a horizontal border line
    let build_border = |left: char, mid: char, right: char| -> StyledLine {
        let mut s = String::new();
        s.push(left);
        for (i, &width) in col_widths.iter().enumerate() {
            s.extend(std::iter::repeat_n('─', width + 2));
            if i < col_widths.len() - 1 {
                s.push(mid);
            }
        }
        s.push(right);
        StyledLine {
            spans: vec![(s, dim_style.clone())],
        }
    };

    // Top border
    lines.push(build_border('┌', '┬', '┐'));

    // Rows
    for (row_idx, row) in rows.iter().enumerate() {
        let mut spans = Vec::new();
        for (i, width) in col_widths.iter().enumerate() {
            spans.push(("│ ".to_string(), dim_style.clone()));
            let cell = row.get(i).map(|s| s.trim()).unwrap_or("");
            let cell_width = cell.width();
            let padding = width.saturating_sub(cell_width);
            spans.push((cell.to_string(), LineStyle::default()));
            spans.push((format!("{} ", " ".repeat(padding)), dim_style.clone()));
        }
        spans.push(("│".to_string(), dim_style.clone()));
        lines.push(StyledLine { spans });

        // Separator between rows
        if row_idx < rows.len() - 1 {
            lines.push(build_border('├', '┼', '┤'));
        }
    }

    // Bottom border
    lines.push(build_border('└', '┴', '┘'));

    lines
}

/// Apply italic and dimmed styling to thinking block content
fn apply_thinking_style(styled_lines: Vec<StyledLine>) -> Vec<StyledLine> {
    styled_lines
        .into_iter()
        .map(|line| StyledLine {
            spans: line
                .spans
                .into_iter()
                .map(|(text, mut style)| {
                    style.italic = true;
                    style.fg = Some(th().thinking_text);
                    (text, style)
                })
                .collect(),
        })
        .collect()
}

/// Render ledger block with styled markdown lines
fn render_ledger_block_styled(
    lines: &mut Vec<RenderedLine>,
    name: &str,
    color: (u8, u8, u8),
    bold: bool,
    styled_lines: Vec<StyledLine>,
    timestamp: Option<&str>,
) {
    for (i, styled_line) in styled_lines.iter().enumerate() {
        let mut spans = Vec::new();

        // Timestamp prefix (only on first line if provided)
        if i == 0 {
            if let Some(ts) = timestamp {
                spans.push((
                    format!(" {} ", ts),
                    LineStyle {
                        fg: Some((140, 140, 140)),
                        dimmed: false,
                        bold: false,
                        italic: false,
                    },
                ));
            }
        } else if timestamp.is_some() {
            // Pad continuation lines to align with timestamped first line
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }

        // Name column (right-aligned, only on first line)
        let name_text = if i == 0 {
            format!("{:>width$}", name, width = NAME_WIDTH)
        } else {
            " ".repeat(NAME_WIDTH)
        };

        spans.push((
            name_text,
            LineStyle {
                fg: Some(color),
                bold,
                dimmed: false,
                italic: false,
            },
        ));

        // Separator
        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                ..Default::default()
            },
        ));

        // Content spans
        if styled_line.spans.is_empty() {
            // Empty line - just push name and separator
        } else {
            for (text, style) in &styled_line.spans {
                spans.push((text.clone(), style.clone()));
            }
        }

        lines.push(RenderedLine { spans });
    }

    // If no lines, still output at least the name
    if styled_lines.is_empty() {
        let mut spans = Vec::new();

        // Timestamp prefix if provided
        if let Some(ts) = timestamp {
            spans.push((
                format!(" {} ", ts),
                LineStyle {
                    fg: Some((140, 140, 140)),
                    dimmed: false,
                    bold: false,
                    italic: false,
                },
            ));
        }

        spans.push((
            format!("{:>width$}", name, width = NAME_WIDTH),
            LineStyle {
                fg: Some(color),
                bold,
                dimmed: false,
                italic: false,
            },
        ));
        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                ..Default::default()
            },
        ));
        lines.push(RenderedLine { spans });
    }
}

/// Render a truncation indicator line like "(N more lines...)"
fn render_truncation_indicator(
    lines: &mut Vec<RenderedLine>,
    remaining: usize,
    dimmed: bool,
    show_timing: bool,
) {
    let mut spans = Vec::new();

    if show_timing {
        spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
    }

    spans.push((" ".repeat(NAME_WIDTH), LineStyle::default()));
    spans.push((
        " │ ".to_string(),
        LineStyle {
            fg: Some(th().border),
            dimmed,
            ..Default::default()
        },
    ));
    spans.push((
        format!("({} more lines...)", remaining),
        LineStyle {
            dimmed: true,
            ..Default::default()
        },
    ));

    lines.push(RenderedLine { spans });
}

/// Render a formatted tool call with proper styling
#[allow(clippy::too_many_arguments)]
fn render_tool_call(
    lines: &mut Vec<RenderedLine>,
    name: &str,
    input: &serde_json::Value,
    label: &str,
    label_color: (u8, u8, u8),
    dimmed: bool,
    content_width: usize,
    timestamp: Option<&str>,
    tool_display: ToolDisplayMode,
) {
    let formatted = tool_format::format_tool_call(name, input, content_width);

    let mut spans = Vec::new();

    // Timestamp prefix (only on first line if provided)
    if let Some(ts) = timestamp {
        spans.push((
            format!(" {} ", ts),
            LineStyle {
                fg: Some((140, 140, 140)),
                dimmed: false,
                bold: false,
                italic: false,
            },
        ));
    }

    // Name column
    spans.push((
        format!("{:>width$}", label, width = NAME_WIDTH),
        LineStyle {
            fg: Some(label_color),
            bold: false,
            dimmed,
            italic: false,
        },
    ));

    // Separator
    spans.push((
        " │ ".to_string(),
        LineStyle {
            fg: Some(th().border),
            dimmed,
            ..Default::default()
        },
    ));

    // Print the header in subtle gray
    spans.push((
        formatted.header.clone(),
        LineStyle {
            fg: Some(th().tool_text),
            dimmed,
            ..Default::default()
        },
    ));

    lines.push(RenderedLine { spans });

    // Render the body if present, with empty line separator
    if let Some(body) = formatted.body {
        let show_timing = timestamp.is_some();

        // Empty line between header and body
        let mut empty_spans = Vec::new();
        if show_timing {
            empty_spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }
        empty_spans.push((" ".repeat(NAME_WIDTH), LineStyle::default()));
        empty_spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                dimmed,
                ..Default::default()
            },
        ));
        lines.push(RenderedLine { spans: empty_spans });

        if tool_display == ToolDisplayMode::Truncated {
            let body_lines: Vec<&str> = body.lines().collect();
            let total = body_lines.len();
            if total > TRUNCATED_BODY_LINES {
                let truncated = body_lines[..TRUNCATED_BODY_LINES].join("\n");
                render_tool_body(lines, &truncated, dimmed, show_timing);
                render_truncation_indicator(
                    lines,
                    total - TRUNCATED_BODY_LINES,
                    dimmed,
                    show_timing,
                );
            } else {
                render_tool_body(lines, &body, dimmed, show_timing);
            }
        } else {
            render_tool_body(lines, &body, dimmed, show_timing);
        }
    }
}

/// Render tool body with diff-aware coloring
fn render_tool_body(lines: &mut Vec<RenderedLine>, text: &str, dimmed: bool, show_timing: bool) {
    for line in text.lines() {
        let mut spans = Vec::new();

        // Timing alignment padding (if timing is enabled)
        if show_timing {
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }

        // Empty name column
        spans.push((" ".repeat(NAME_WIDTH), LineStyle::default()));

        // Separator
        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                dimmed,
                ..Default::default()
            },
        ));

        // Content with diff coloring
        if line.starts_with("+ ") {
            spans.push((
                line.to_string(),
                LineStyle {
                    fg: Some(th().diff_add),
                    dimmed,
                    ..Default::default()
                },
            ));
        } else if line.starts_with("- ") {
            spans.push((
                line.to_string(),
                LineStyle {
                    fg: Some(th().diff_remove),
                    dimmed,
                    ..Default::default()
                },
            ));
        } else {
            spans.push((
                line.to_string(),
                LineStyle {
                    dimmed: true,
                    ..Default::default()
                },
            ));
        }

        lines.push(RenderedLine { spans });
    }
}

/// Wrap plain text to fit the available width while preserving existing line breaks.
fn wrap_plain_text_lines(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return text.lines().map(|line| line.to_string()).collect();
    }

    let mut wrapped = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            wrapped.push(String::new());
            continue;
        }

        let pieces: Vec<String> = textwrap::wrap(line, max_width)
            .into_iter()
            .map(|piece| piece.into_owned())
            .collect();

        if pieces.is_empty() {
            wrapped.push(String::new());
        } else {
            wrapped.extend(pieces);
        }
    }

    if wrapped.is_empty() {
        wrapped.push(String::new());
    }

    wrapped
}

/// Render tool result with arrow indicator using plain-text wrapping.
fn render_tool_result(
    lines: &mut Vec<RenderedLine>,
    text: &str,
    content_width: usize,
    timestamp: Option<&str>,
    tool_display: ToolDisplayMode,
) {
    let wrapped_lines = wrap_plain_text_lines(text, content_width);
    let total = wrapped_lines.len();
    let limit = if tool_display == ToolDisplayMode::Truncated && total > TRUNCATED_RESULT_LINES {
        TRUNCATED_RESULT_LINES
    } else {
        total
    };

    for (i, line_text) in wrapped_lines.iter().take(limit).enumerate() {
        let mut spans = Vec::new();

        // Timestamp prefix (only on first line if provided)
        if i == 0 {
            if let Some(ts) = timestamp {
                spans.push((
                    format!(" {} ", ts),
                    LineStyle {
                        fg: Some((140, 140, 140)),
                        dimmed: false,
                        bold: false,
                        italic: false,
                    },
                ));
            }
        } else if timestamp.is_some() {
            // Pad continuation lines to align with timestamped first line
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }

        // First line gets the label, rest are empty
        if i == 0 {
            spans.push((
                format!("{:>width$}", "↳ Result", width = NAME_WIDTH),
                LineStyle {
                    fg: Some(th().tool_text),
                    ..Default::default()
                },
            ));
        } else {
            spans.push((" ".repeat(NAME_WIDTH), LineStyle::default()));
        }

        // Separator
        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                ..Default::default()
            },
        ));

        spans.push((line_text.clone(), LineStyle::default()));

        lines.push(RenderedLine { spans });
    }

    if limit < total {
        render_truncation_indicator(lines, total - limit, false, timestamp.is_some());
    }
}

/// Get a truncated agent ID for display (max 7 characters)
fn short_agent_id(agent_id: &str) -> &str {
    &agent_id[..agent_id.len().min(7)]
}

/// Create a label for subagent entries from a parent_tool_use_id.
fn subagent_label(parent_tool_use_id: &str) -> String {
    format!("↳{}", claude::short_parent_id(parent_tool_use_id))
}

/// Render agent (subagent) progress message
fn render_agent_message(
    lines: &mut Vec<RenderedLine>,
    agent_progress: &crate::claude::AgentProgressData,
    options: &RenderOptions,
) {
    use crate::claude::{AgentContent, ContentBlock};

    let agent_id = &agent_progress.agent_id;
    let short_id = short_agent_id(agent_id);
    let msg = &agent_progress.message;
    let mut printed = false;

    match msg.message_type.as_str() {
        "user" => {
            let AgentContent::Blocks(blocks) = &msg.message.content;

            // Aggregate text blocks and render together
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();

            if !texts.is_empty() {
                let combined = texts.join("\n\n");
                let md_lines = render_markdown_to_lines(&combined, options.content_width);
                let name = format!("↳{}", short_id);
                render_ledger_block_styled_dimmed(
                    lines,
                    &name,
                    th().text_primary,
                    md_lines,
                    options.show_timing,
                );
                printed = true;
            }

            // Tool results
            if options.tool_display.is_visible() {
                for block in blocks {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        render_ledger_block_plain_dimmed(
                            lines,
                            "  ↳ Tool",
                            th().accent_dim,
                            "<Result>",
                            options.show_timing,
                        );
                        let content_str = format_tool_result_content(content.as_ref());
                        if options.tool_display == ToolDisplayMode::Truncated {
                            let content_lines: Vec<&str> = content_str.lines().collect();
                            let total = content_lines.len();
                            if total > TRUNCATED_RESULT_LINES {
                                let truncated = content_lines[..TRUNCATED_RESULT_LINES].join("\n");
                                render_continuation_dimmed(lines, &truncated, options.show_timing);
                                render_truncation_indicator(
                                    lines,
                                    total - TRUNCATED_RESULT_LINES,
                                    true,
                                    options.show_timing,
                                );
                            } else {
                                render_continuation_dimmed(
                                    lines,
                                    &content_str,
                                    options.show_timing,
                                );
                            }
                        } else {
                            render_continuation_dimmed(lines, &content_str, options.show_timing);
                        }
                        printed = true;
                    }
                }
            }
        }
        "assistant" => {
            let AgentContent::Blocks(blocks) = &msg.message.content;

            // Aggregate text blocks and render together
            let texts: Vec<&str> = blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();

            if !texts.is_empty() {
                let combined = texts.join("\n\n");
                let md_lines = render_markdown_to_lines(&combined, options.content_width);
                let name = format!("↳{}", short_id);
                render_ledger_block_styled_dimmed(
                    lines,
                    &name,
                    th().accent,
                    md_lines,
                    options.show_timing,
                );
                printed = true;
            }

            // Tool calls
            if options.tool_display.is_visible() {
                let align_ts = if options.show_timing {
                    Some("     ")
                } else {
                    None
                };
                for block in blocks {
                    if let ContentBlock::ToolUse { name, input, .. } = block {
                        let label = format!("↳{}", short_id);
                        render_tool_call(
                            lines,
                            name,
                            input,
                            &label,
                            th().accent_dim,
                            true,
                            options.content_width,
                            align_ts,
                            options.tool_display,
                        );
                        printed = true;
                    }
                }
            }
        }
        _ => {}
    }

    if printed {
        lines.push(RenderedLine { spans: vec![] });
    }
}

/// Render ledger block with styled markdown lines (dimmed for subagents)
fn render_ledger_block_styled_dimmed(
    lines: &mut Vec<RenderedLine>,
    name: &str,
    color: (u8, u8, u8),
    styled_lines: Vec<StyledLine>,
    show_timing: bool,
) {
    for (i, styled_line) in styled_lines.iter().enumerate() {
        let mut spans = Vec::new();

        if show_timing {
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }

        let name_text = if i == 0 {
            format!("{:>width$}", name, width = NAME_WIDTH)
        } else {
            " ".repeat(NAME_WIDTH)
        };

        spans.push((
            name_text,
            LineStyle {
                fg: Some(color),
                bold: false,
                dimmed: true,
                italic: false,
            },
        ));

        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                dimmed: true,
                ..Default::default()
            },
        ));

        for (text, mut style) in styled_line.spans.iter().cloned() {
            style.dimmed = true;
            spans.push((text, style));
        }

        lines.push(RenderedLine { spans });
    }

    if styled_lines.is_empty() {
        let mut spans = Vec::new();
        if show_timing {
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }
        spans.push((
            format!("{:>width$}", name, width = NAME_WIDTH),
            LineStyle {
                fg: Some(color),
                bold: false,
                dimmed: true,
                italic: false,
            },
        ));
        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                dimmed: true,
                ..Default::default()
            },
        ));
        lines.push(RenderedLine { spans });
    }
}

/// Render ledger block with plain text (dimmed for subagents)
fn render_ledger_block_plain_dimmed(
    lines: &mut Vec<RenderedLine>,
    name: &str,
    color: (u8, u8, u8),
    text: &str,
    show_timing: bool,
) {
    for (i, line_text) in text.lines().enumerate() {
        let mut spans = Vec::new();

        if show_timing {
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }

        let name_text = if i == 0 {
            format!("{:>width$}", name, width = NAME_WIDTH)
        } else {
            " ".repeat(NAME_WIDTH)
        };

        spans.push((
            name_text,
            LineStyle {
                fg: Some(color),
                bold: false,
                dimmed: true,
                italic: false,
            },
        ));

        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                dimmed: true,
                ..Default::default()
            },
        ));

        spans.push((
            line_text.to_string(),
            LineStyle {
                dimmed: true,
                ..Default::default()
            },
        ));

        lines.push(RenderedLine { spans });
    }
}

/// Render continuation lines (dimmed for subagents)
fn render_continuation_dimmed(lines: &mut Vec<RenderedLine>, text: &str, show_timing: bool) {
    for line_text in text.lines() {
        let mut spans = Vec::new();

        if show_timing {
            spans.push((" ".repeat(TIMESTAMP_WIDTH), LineStyle::default()));
        }

        spans.push((
            " ".repeat(NAME_WIDTH),
            LineStyle {
                dimmed: true,
                ..Default::default()
            },
        ));
        spans.push((
            " │ ".to_string(),
            LineStyle {
                fg: Some(th().border),
                dimmed: true,
                ..Default::default()
            },
        ));
        spans.push((
            line_text.to_string(),
            LineStyle {
                dimmed: true,
                ..Default::default()
            },
        ));

        lines.push(RenderedLine { spans });
    }
}

/// Process user message text to handle command-related XML tags.
/// Returns None if the message should be skipped entirely (e.g., empty local-command-stdout).
fn process_command_message(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // Check for local-command-caveat - skip these system messages entirely
    if trimmed.starts_with("<local-command-caveat>") && trimmed.ends_with("</local-command-caveat>")
    {
        return None;
    }

    // Check for empty or whitespace-only local-command-stdout - skip these entirely
    if trimmed.starts_with("<local-command-stdout>") && trimmed.ends_with("</local-command-stdout>")
    {
        let tag_start = "<local-command-stdout>".len();
        let tag_end = trimmed.len() - "</local-command-stdout>".len();
        let inner = &trimmed[tag_start..tag_end];
        if inner.trim().is_empty() {
            return None;
        }
        // Non-empty local-command-stdout: show the content without the tags
        return Some(inner.trim().to_string());
    }

    // Check if this is a command message with <command-name> tag
    if let Some(start) = trimmed.find("<command-name>")
        && let Some(end) = trimmed.find("</command-name>")
    {
        let content_start = start + "<command-name>".len();
        if content_start < end {
            let command_name = &trimmed[content_start..end];

            // Skip /clear commands - internal context-clearing, not meaningful to display
            if command_name == "/clear" {
                return None;
            }

            // Also extract command args if present
            if let Some(args_start) = trimmed.find("<command-args>")
                && let Some(args_end) = trimmed.find("</command-args>")
            {
                let args_content_start = args_start + "<command-args>".len();
                if args_content_start < args_end {
                    let args = trimmed[args_content_start..args_end].trim();
                    if !args.is_empty() {
                        return Some(format!("{} {}", command_name, args));
                    }
                }
            }

            return Some(command_name.to_string());
        }
    }

    // Skill invocation expanded prompts - show description instead of full prompt
    if trimmed.starts_with("Base directory for this skill:") {
        let description = trimmed
            .lines()
            .skip(1)
            .find(|l| !l.trim().is_empty())
            .unwrap_or("invoked");
        return Some(format!("*Skill: {}*", description));
    }

    Some(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::LineStyle;

    fn lines_to_text(lines: &[RenderedLine]) -> Vec<String> {
        lines.iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|(text, _style): &(String, LineStyle)| text.as_str())
                    .collect::<String>()
            })
            .collect()
    }

    /// Helper to render markdown and extract just the content text (without styling)
    fn render_to_text(input: &str, width: usize) -> String {
        let lines = render_markdown_to_lines(input, width);
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|(text, _)| text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn test_render_tool_result_preserves_plain_multiline_text() {
        let mut lines = Vec::new();
        let text = "2034 }\n2035     let max_scroll = state.total_lines.saturating_sub(viewport_height);\n2036     state.scroll_offset = old_scroll.min(max_scroll);";

        render_tool_result(&mut lines, text, 120, None, ToolDisplayMode::Full);

        let rendered = lines_to_text(&lines);
        assert_eq!(rendered.len(), 3, "Expected 3 lines, got:\n{rendered:#?}");
        assert!(rendered[0].contains("2034 }"), "missing first line: {rendered:#?}");
        assert!(
            rendered[1].contains("2035     let max_scroll"),
            "missing second line: {rendered:#?}"
        );
        assert!(
            rendered[2].contains("2036     state.scroll_offset"),
            "missing third line: {rendered:#?}"
        );
    }

    #[test]
    fn test_plain_text() {
        let result = render_to_text("Hello world", 80);
        assert_eq!(result.trim(), "Hello world");
    }

    #[test]
    fn test_heading() {
        let result = render_to_text("# Heading 1", 80);
        assert!(result.contains("Heading 1"));
    }

    #[test]
    fn test_heading_with_paragraph() {
        let result = render_to_text("# Heading\n\nSome text", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should have: heading, blank, text
        assert_eq!(lines.len(), 3, "Expected 3 lines, got:\n{}", result);
        assert!(lines[0].contains("Heading"));
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "Some text");
    }

    #[test]
    fn test_paragraph_with_list() {
        let result = render_to_text("Some intro:\n\n- Item 1\n- Item 2", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should have: para, blank, item1, item2
        assert_eq!(lines.len(), 4, "Expected 4 lines, got:\n{}", result);
        assert_eq!(lines[0], "Some intro:");
        assert_eq!(lines[1], "");
        assert!(lines[2].contains("- Item 1"));
        assert!(lines[3].contains("- Item 2"));
    }

    #[test]
    fn test_numbered_list_with_bold() {
        // This is the bug case: numbered list item starting with bold text
        let result = render_to_text("1. **Task 10:** description\n2. **Task 11:** more", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should have: item1, item2 (NO blank lines between number and content)
        assert_eq!(lines.len(), 2, "Expected 2 lines, got:\n{}", result);
        assert!(
            lines[0].starts_with("1. "),
            "Line should start with '1. ': {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("Task 10"),
            "Line should contain 'Task 10': {:?}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("2. "),
            "Line should start with '2. ': {:?}",
            lines[1]
        );
        assert!(
            lines[1].contains("Task 11"),
            "Line should contain 'Task 11': {:?}",
            lines[1]
        );
    }

    #[test]
    fn test_numbered_list_no_extra_blank_lines() {
        let input = "## Changes\n\n1. **First change:**\n   - details\n2. **Second change:**\n   - more details";
        let result = render_to_text(input, 80);
        let lines: Vec<&str> = result.lines().collect();

        // Verify no blank lines between "1." and "First change"
        let line1_idx = lines
            .iter()
            .position(|l| l.starts_with("1. "))
            .expect("Should find '1. '");
        assert!(
            lines[line1_idx].contains("First change"),
            "First item should be on same line as '1. '"
        );

        // Verify no blank lines between "2." and "Second change"
        let line2_idx = lines
            .iter()
            .position(|l| l.starts_with("2. "))
            .expect("Should find '2. '");
        assert!(
            lines[line2_idx].contains("Second change"),
            "Second item should be on same line as '2. '"
        );
    }

    #[test]
    fn test_consecutive_list_items_no_blanks() {
        let result = render_to_text("- First\n- Second\n- Third", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should be exactly 3 lines, no blanks between items
        assert_eq!(
            lines.len(),
            3,
            "Expected 3 lines with no blanks, got:\n{}",
            result
        );
        assert!(lines[0].contains("- First"));
        assert!(lines[1].contains("- Second"));
        assert!(lines[2].contains("- Third"));
    }

    #[test]
    fn test_nested_list() {
        let result = render_to_text("- Item 1\n  - Nested 1\n  - Nested 2\n- Item 2", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should have: item1, nested1, nested2, item2 (no extra blanks)
        assert_eq!(lines.len(), 4, "Expected 4 lines, got:\n{}", result);
        assert!(lines[0].contains("- Item 1"));
        assert!(lines[1].contains("- Nested 1"));
        assert!(lines[2].contains("- Nested 2"));
        assert!(lines[3].contains("- Item 2"));
    }

    #[test]
    fn test_code_block() {
        let result = render_to_text("Text\n\n```rust\nlet x = 1;\n```\n\nMore text", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should have: text, blank, fence, code, fence, blank, more text
        assert!(result.contains("```"));
        assert!(result.contains("let x = 1;"));

        // Check for proper spacing
        let text_idx = lines.iter().position(|l| l == &"Text").unwrap();
        let more_idx = lines.iter().position(|l| l == &"More text").unwrap();
        // Should have blank line after Text and before More text
        assert_eq!(lines[text_idx + 1], "", "Should have blank line after Text");
        assert_eq!(
            lines[more_idx - 1],
            "",
            "Should have blank line before More text"
        );
    }

    #[test]
    fn test_block_quote() {
        let result = render_to_text("Text\n\n> Quote here", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Block quote renders with quote prefix on one line, blank, then content
        // This is due to how the markdown parser handles block quotes
        assert_eq!(lines[0], "Text");
        assert_eq!(lines[1], ""); // blank before quote
        assert!(lines[2].starts_with("> "), "Should have quote prefix");
        // Content may be on same line or next line depending on parser
        let has_content =
            lines[2].contains("Quote here") || (lines.len() > 4 && lines[4].contains("Quote here"));
        assert!(has_content, "Should contain quote content");
    }

    #[test]
    fn test_horizontal_rule() {
        let result = render_to_text("Before\n\n---\n\nAfter", 80);
        let lines: Vec<&str> = result.lines().collect();
        // Should have proper spacing around rule
        let before_idx = lines.iter().position(|l| l == &"Before").unwrap();
        let after_idx = lines.iter().position(|l| l == &"After").unwrap();
        // Rule should be on its own with blanks around it
        assert_eq!(
            lines[before_idx + 1],
            "",
            "Should have blank line after Before"
        );
        assert!(lines[before_idx + 2].contains("─"), "Should have rule");
        assert_eq!(
            lines[after_idx - 1],
            "",
            "Should have blank line before After"
        );
    }

    #[test]
    fn test_multiple_paragraphs() {
        let result = render_to_text(
            "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.",
            80,
        );
        let lines: Vec<&str> = result.lines().collect();
        // Should have: p1, blank, p2, blank, p3
        assert_eq!(lines.len(), 5, "Expected 5 lines, got:\n{}", result);
        assert_eq!(lines[0], "First paragraph.");
        assert_eq!(lines[1], "");
        assert_eq!(lines[2], "Second paragraph.");
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "Third paragraph.");
    }

    #[test]
    fn test_list_with_multiline_items() {
        let input = "1. First item\n   with continuation\n2. Second item\n   also continued";
        let result = render_to_text(input, 80);
        let lines: Vec<&str> = result.lines().collect();

        // First item should start with "1. "
        assert!(lines[0].starts_with("1. "), "First line: {:?}", lines[0]);
        // Soft breaks join continuation to the same paragraph, so all first-item
        // text may appear on a single line at wide widths
        let first_item_text = lines.join(" ");
        assert!(
            first_item_text.contains("First item"),
            "Should contain first item text"
        );
        assert!(
            first_item_text.contains("with continuation"),
            "Should contain continuation"
        );

        // Second item should start with "2. "
        let line2_idx = lines
            .iter()
            .position(|l| l.starts_with("2. "))
            .expect("Should find '2. '");
        assert!(line2_idx >= 1, "Second item should appear after first");
    }

    #[test]
    fn test_no_trailing_blank_lines() {
        let result = render_to_text("Text\n\n## Heading\n\nParagraph", 80);
        // Should not end with blank lines
        assert!(
            !result.ends_with("\n\n"),
            "Should not have trailing blank lines: {:?}",
            result
        );
    }

    #[test]
    fn test_inline_code() {
        let result = render_to_text("Use `code` here", 80);
        assert!(result.contains("code"));
    }

    #[test]
    fn test_bold_and_italic() {
        let result = render_to_text("**bold** and *italic* text", 80);
        // Just verify it renders without panicking and contains the text
        assert!(result.contains("bold"));
        assert!(result.contains("italic"));
    }

    #[test]
    fn test_table_basic() {
        let input = "| A | B |\n|---|---|\n| 1 | 2 |";
        let result = render_to_text(input, 80);
        eprintln!("Table output:\n{}", result);
        assert!(result.contains('┌'), "Expected top-left corner");
        assert!(result.contains('│'), "Expected vertical border");
        assert!(result.contains('└'), "Expected bottom-left corner");
        assert!(result.contains(" A "), "Expected cell A");
        assert!(result.contains(" B "), "Expected cell B");
        assert!(result.contains(" 1 "), "Expected cell 1");
        assert!(result.contains(" 2 "), "Expected cell 2");
    }

    #[test]
    fn test_table_column_widths() {
        let input = "| Short | Longer text |\n|---|---|\n| A | B |";
        let result = render_to_text(input, 80);
        eprintln!("Table output:\n{}", result);
        assert!(result.contains("Short"), "Expected Short");
        assert!(result.contains("Longer text"), "Expected Longer text");
        // Columns should be sized to fit longest content
        let lines: Vec<&str> = result.lines().collect();
        // All border lines should be same width
        let border_widths: Vec<usize> = lines
            .iter()
            .filter(|l| l.starts_with('┌') || l.starts_with('├') || l.starts_with('└'))
            .map(|l| l.chars().count())
            .collect();
        assert!(
            border_widths.windows(2).all(|w| w[0] == w[1]),
            "Border lines should be same width: {:?}",
            border_widths
        );
    }

    #[test]
    fn test_table_multiple_rows() {
        let input = "| H1 | H2 | H3 |\n|----|----|----|\n| A | B | C |\n| D | E | F |";
        let result = render_to_text(input, 80);
        eprintln!("Table output:\n{}", result);
        assert!(result.contains('├'), "Expected row separators");
        assert!(result.contains('┼'), "Expected cross junctions");
    }

    #[test]
    fn test_format_timestamp() {
        // UTC timestamp with Z suffix
        let ts = "2026-02-04T19:46:38.440Z";
        let result = format_timestamp(ts);
        assert!(result.is_some(), "Should parse UTC timestamp");
        let formatted = result.unwrap();
        // Should be HH:MM format (local time)
        assert_eq!(formatted.len(), 5, "Should be HH:MM format: {}", formatted);
        assert!(
            formatted.contains(':'),
            "Should contain colon: {}",
            formatted
        );

        // Timestamp with timezone offset
        let ts2 = "2026-02-04T14:46:38-05:00";
        let result2 = format_timestamp(ts2);
        assert!(result2.is_some(), "Should parse timestamp with offset");
    }
}
