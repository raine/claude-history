use crate::claude::{AssistantMessage, ContentBlock, LogEntry, UserContent};
use crate::cli::DebugLevel;
use crate::debug;
use crate::debug_log;
use crate::error::Result;
use crate::markdown::render_markdown;
use crate::pager;
use crate::tool_format;
use crate::tui::theme;
use crate::tui::viewer::process_command_message;
use colored::{ColoredString, Colorize, CustomColor};
use crossterm::terminal;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

/// Configuration options for displaying conversations
#[derive(Debug, Clone, Default)]
pub struct DisplayOptions {
    /// Hide tool calls and results
    pub no_tools: bool,
    /// Show thinking/reasoning blocks
    pub show_thinking: bool,
    /// Debug level for error logging
    pub debug_level: Option<DebugLevel>,
    /// Use a pager for output (less/more)
    pub use_pager: bool,
    /// Disable colored output
    pub no_color: bool,
}

const NAME_WIDTH: usize = 9;
const SEPARATOR: &str = " │ ";
const SEPARATOR_WIDTH: usize = 3; // Display width of " │ "

/// Convert a theme RGB tuple to a colored CustomColor
fn cc(rgb: (u8, u8, u8)) -> CustomColor {
    CustomColor {
        r: rgb.0,
        g: rgb.1,
        b: rgb.2,
    }
}

fn teal() -> CustomColor {
    cc(theme::detect_theme().accent)
}
fn dim_teal() -> CustomColor {
    cc(theme::detect_theme().accent_dim)
}
fn separator_color() -> CustomColor {
    cc(theme::detect_theme().border)
}
fn tool_text() -> CustomColor {
    cc(theme::detect_theme().tool_text)
}
fn diff_add() -> CustomColor {
    cc(theme::detect_theme().diff_add)
}
fn diff_remove() -> CustomColor {
    cc(theme::detect_theme().diff_remove)
}

/// Trait for formatting conversation output
///
/// Implementors handle the actual rendering of conversation elements,
/// allowing the same processing logic to output in different formats
/// (ledger-style with markdown, plain text, etc.)
trait OutputFormatter {
    /// Format and output user text content
    fn format_user_text(&mut self, text: &str);

    /// Format and output assistant text content
    fn format_assistant_text(&mut self, text: &str);

    /// Format and output a tool call
    fn format_tool_call(&mut self, name: &str, input: &serde_json::Value);

    /// Format and output a tool result
    fn format_tool_result(&mut self, content: Option<&serde_json::Value>);

    /// Format and output a thinking/reasoning block
    fn format_thinking(&mut self, thought: &str);

    /// End the current message block (add spacing)
    fn end_message(&mut self);

    /// Format and output agent (subagent) user text content
    fn format_agent_user_text(&mut self, agent_id: &str, text: &str);

    /// Format and output agent (subagent) assistant text content
    fn format_agent_assistant_text(&mut self, agent_id: &str, text: &str);

    /// Format and output an agent tool call
    fn format_agent_tool_call(&mut self, agent_id: &str, name: &str, input: &serde_json::Value);

    /// Format and output an agent tool result
    fn format_agent_tool_result(&mut self, agent_id: &str, content: Option<&serde_json::Value>);
}

/// Ledger-style formatter with markdown rendering and aligned columns
struct LedgerFormatter<'a, W: Write + ?Sized> {
    writer: &'a mut W,
    content_width: usize,
}

impl<'a, W: Write + ?Sized> LedgerFormatter<'a, W> {
    fn new(writer: &'a mut W, content_width: usize) -> Self {
        Self {
            writer,
            content_width,
        }
    }

    /// Write lines in ledger format with a name on the first line
    fn write_labeled_lines<'b>(
        &mut self,
        name: &str,
        style: impl Fn(&str) -> ColoredString,
        lines: impl IntoIterator<Item = &'b str>,
    ) {
        for (i, line) in lines.into_iter().enumerate() {
            if i == 0 {
                let padded = format!("{:>width$}", name, width = NAME_WIDTH);
                let _ = write!(self.writer, "{}", style(&padded));
            } else {
                let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            }
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));
            let _ = writeln!(self.writer, "{}", line);
        }
    }

    /// Print lines in ledger format with a name on the first line
    fn print_lines(&mut self, name: &str, style: impl Fn(&str) -> ColoredString, text: &str) {
        let wrapped_lines = wrap_text(text, self.content_width);
        self.write_labeled_lines(name, style, wrapped_lines.iter().map(|s| s.as_str()));
    }

    /// Print continuation lines with dimmed content
    fn print_continuation(&mut self, text: &str) {
        for line in wrap_text(text, self.content_width) {
            let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));
            let _ = writeln!(self.writer, "{}", line.dimmed());
        }
    }

    /// Print tool body with diff-aware coloring
    fn print_tool_body(&mut self, text: &str) {
        for line in text.lines() {
            let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));
            if line.starts_with("+ ") {
                let _ = writeln!(self.writer, "{}", line.custom_color(diff_add()));
            } else if line.starts_with("- ") {
                let _ = writeln!(self.writer, "{}", line.custom_color(diff_remove()));
            } else {
                let _ = writeln!(self.writer, "{}", line.dimmed());
            }
        }
    }

    /// Print pre-formatted markdown text with ledger layout
    fn print_markdown(&mut self, name: &str, style: impl Fn(&str) -> ColoredString, text: &str) {
        let lines: Vec<&str> = text.lines().collect();

        if lines.is_empty() {
            let padded = format!("{:>width$}", name, width = NAME_WIDTH);
            let _ = write!(self.writer, "{}", style(&padded));
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));
            let _ = writeln!(self.writer);
            return;
        }

        self.write_labeled_lines(name, style, lines.iter().copied());
    }
}

impl<W: Write + ?Sized> OutputFormatter for LedgerFormatter<'_, W> {
    fn format_user_text(&mut self, text: &str) {
        let rendered = render_markdown(text, self.content_width);
        self.print_markdown("You", |s| s.white().bold(), &rendered);
    }

    fn format_assistant_text(&mut self, text: &str) {
        let rendered = render_markdown(text, self.content_width);
        self.print_markdown("Claude", |s| s.custom_color(teal()).bold(), &rendered);
    }

    fn format_tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, self.content_width);

        // Print the header with appropriate styling
        let padded_name = format!("{:>width$}", "Claude", width = NAME_WIDTH);
        let _ = write!(self.writer, "{}", padded_name.custom_color(dim_teal()));
        let _ = write!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));

        // Print the header in subtle gray
        let _ = writeln!(
            self.writer,
            "{}",
            formatted.header.custom_color(tool_text())
        );

        // Print the body if present, with empty line separator
        if let Some(body) = formatted.body {
            // Empty line between header and body
            let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            let _ = writeln!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));
            self.print_tool_body(&body);
        }
    }

    fn format_tool_result(&mut self, content: Option<&serde_json::Value>) {
        // Render markdown for string content, otherwise format as JSON
        let rendered = match content {
            Some(serde_json::Value::String(s)) => render_markdown(s, self.content_width),
            _ => format_tool_content(content),
        };

        // Print with ↳ Result label
        self.print_markdown("↳ Result", |s| s.custom_color(tool_text()), &rendered);
    }

    fn format_thinking(&mut self, thought: &str) {
        self.print_lines("Thinking", |s| s.custom_color(dim_teal()), thought);
    }

    fn end_message(&mut self) {
        let _ = writeln!(self.writer);
    }

    fn format_agent_user_text(&mut self, agent_id: &str, text: &str) {
        let rendered = render_markdown(text, self.content_width);
        let name = format!("↳{}", short_agent_id(agent_id));
        self.print_markdown(&name, |s| s.white().dimmed(), &rendered);
    }

    fn format_agent_assistant_text(&mut self, agent_id: &str, text: &str) {
        let rendered = render_markdown(text, self.content_width);
        let name = format!("↳{}", short_agent_id(agent_id));
        self.print_markdown(&name, |s| s.custom_color(teal()).dimmed(), &rendered);
    }

    fn format_agent_tool_call(&mut self, agent_id: &str, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, self.content_width);
        let label = format!("↳{}", short_agent_id(agent_id));

        // Print the header with appropriate styling (dimmed for subagents)
        let padded_name = format!("{:>width$}", label, width = NAME_WIDTH);
        let _ = write!(
            self.writer,
            "{}",
            padded_name.custom_color(dim_teal()).dimmed()
        );
        let _ = write!(self.writer, "{}", SEPARATOR.custom_color(separator_color()));

        // Print the header - dimmed for subagents
        let _ = writeln!(self.writer, "{}", formatted.header.dimmed());

        // Print the body if present
        if let Some(body) = formatted.body {
            self.print_continuation(&body);
        }
    }

    fn format_agent_tool_result(&mut self, _agent_id: &str, content: Option<&serde_json::Value>) {
        self.print_lines(
            "  ↳ Tool",
            |s| s.custom_color(dim_teal()).dimmed(),
            "<Result>",
        );
        let content_str = format_tool_content(content);
        self.print_continuation(&content_str);
    }
}

/// Plain text formatter without formatting or alignment
struct PlainFormatter<'a, W: Write + ?Sized> {
    writer: &'a mut W,
}

/// Default content width for plain text output
const PLAIN_CONTENT_WIDTH: usize = 80;

impl<'a, W: Write + ?Sized> OutputFormatter for PlainFormatter<'a, W> {
    fn format_user_text(&mut self, text: &str) {
        let _ = writeln!(self.writer, "You: {}", text);
    }

    fn format_assistant_text(&mut self, text: &str) {
        let _ = writeln!(self.writer, "Claude: {}", text);
    }

    fn format_tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
        let _ = writeln!(self.writer, "Claude: {}", formatted.header);
        if let Some(body) = formatted.body {
            for line in body.lines() {
                let _ = writeln!(self.writer, "  {}", line);
            }
        }
    }

    fn format_tool_result(&mut self, content: Option<&serde_json::Value>) {
        let _ = writeln!(self.writer, "Tool: <Result>");
        let content_str = format_tool_content(content);
        let _ = writeln!(self.writer, "{}", content_str);
    }

    fn format_thinking(&mut self, thought: &str) {
        let _ = writeln!(self.writer, "Thinking: {}", thought);
    }

    fn end_message(&mut self) {
        let _ = writeln!(self.writer);
    }

    fn format_agent_user_text(&mut self, agent_id: &str, text: &str) {
        let _ = writeln!(
            self.writer,
            "  [{}] User: {}",
            short_agent_id(agent_id),
            text
        );
    }

    fn format_agent_assistant_text(&mut self, agent_id: &str, text: &str) {
        let _ = writeln!(
            self.writer,
            "  [{}] Agent: {}",
            short_agent_id(agent_id),
            text
        );
    }

    fn format_agent_tool_call(&mut self, agent_id: &str, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
        let _ = writeln!(
            self.writer,
            "  [{}] Agent: {}",
            short_agent_id(agent_id),
            formatted.header
        );
        if let Some(body) = formatted.body {
            for line in body.lines() {
                let _ = writeln!(self.writer, "    {}", line);
            }
        }
    }

    fn format_agent_tool_result(&mut self, _agent_id: &str, content: Option<&serde_json::Value>) {
        let _ = writeln!(self.writer, "    Tool: <Result>");
        let content_str = format_tool_content(content);
        for line in content_str.lines() {
            let _ = writeln!(self.writer, "    {}", line);
        }
    }
}

/// Format tool result content to a string
fn format_tool_content(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(value) => {
            if let Some(s) = value.as_str() {
                s.to_string()
            } else if let Ok(formatted) = serde_json::to_string_pretty(value) {
                formatted
            } else {
                "<invalid content>".to_string()
            }
        }
        None => "<no content>".to_string(),
    }
}

/// Create a display ID for subagent entries from a parent_tool_use_id.
fn subagent_display_id(parent_tool_use_id: &str) -> String {
    crate::claude::short_parent_id(parent_tool_use_id)
}

/// Get the terminal width, defaulting to 80 if unavailable
fn get_terminal_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// Wrap text using textwrap for proper unicode handling
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }
    textwrap::wrap(text, max_width)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect()
}

enum DisplayFormat {
    Ledger { content_width: usize },
    Plain,
}

/// Display a conversation from a file
pub fn display_conversation(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    let terminal_width = get_terminal_width();
    let content_width = terminal_width.saturating_sub(NAME_WIDTH + SEPARATOR_WIDTH);

    stream_log_entries(file_path, options, DisplayFormat::Ledger { content_width })
}

/// Display a conversation in plain text format (no ledger formatting)
pub fn display_conversation_plain(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    stream_log_entries(file_path, options, DisplayFormat::Plain)
}

fn stream_log_entries(
    file_path: &Path,
    options: &DisplayOptions,
    format: DisplayFormat,
) -> Result<()> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);

    // Spawn pager if requested
    let mut pager_child = if options.use_pager {
        pager::spawn_pager().ok()
    } else {
        None
    };

    let mut stdout_handle = io::stdout().lock();
    let writer: &mut dyn Write = if let Some(ref mut child) = pager_child {
        child.stdin.as_mut().unwrap()
    } else {
        &mut stdout_handle
    };

    match format {
        DisplayFormat::Ledger { content_width } => {
            let mut formatter = LedgerFormatter::new(writer, content_width);
            process_log_entries(reader, file_path, options, &mut formatter)?;
        }
        DisplayFormat::Plain => {
            let mut formatter = PlainFormatter { writer };
            process_log_entries(reader, file_path, options, &mut formatter)?;
        }
    }

    // Close stdin and wait for pager to finish
    drop(stdout_handle);
    if let Some(mut child) = pager_child {
        let _ = child.wait();
    }

    Ok(())
}

fn process_log_entries<F: OutputFormatter>(
    reader: BufReader<File>,
    file_path: &Path,
    options: &DisplayOptions,
    formatter: &mut F,
) -> Result<()> {
    for (line_number, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => {
                process_entry(formatter, &entry, options.no_tools, options.show_thinking);
            }
            Err(e) => {
                debug::error(
                    options.debug_level,
                    &format!("Failed to parse line {}: {}", line_number + 1, e),
                );
                if options.debug_level.is_some() {
                    let _ = debug_log::log_display_error(
                        file_path,
                        line_number + 1,
                        &e.to_string(),
                        &line,
                    );
                }
            }
        }
    }

    Ok(())
}

/// Process a log entry using the provided formatter
fn process_entry<F: OutputFormatter>(
    formatter: &mut F,
    entry: &LogEntry,
    no_tools: bool,
    show_thinking: bool,
) {
    match entry {
        LogEntry::Summary { .. }
        | LogEntry::FileHistorySnapshot { .. }
        | LogEntry::System { .. }
        | LogEntry::CustomTitle { .. }
        | LogEntry::AiTitle { .. }
        | LogEntry::AgentName { .. }
        | LogEntry::PermissionMode { .. }
        | LogEntry::PiMetadata {
            searchable: false, ..
        }
        | LogEntry::Unknown => {
            // Skip metadata entries
        }
        LogEntry::PiMetadata {
            label,
            text,
            searchable: true,
            ..
        } => {
            let content = if text.is_empty() {
                format!("[{label}]")
            } else {
                format!("[{label}] {text}")
            };
            process_user_message(
                formatter,
                &crate::claude::UserMessage {
                    role: "user".to_owned(),
                    content: UserContent::String(content),
                },
                no_tools,
                None,
            );
        }
        LogEntry::Progress { data, .. } => {
            // Handle agent_progress entries (only when show_thinking is enabled)
            if show_thinking && let Some(agent_progress) = crate::claude::parse_agent_progress(data)
            {
                process_agent_message(formatter, &agent_progress, no_tools);
            }
        }
        LogEntry::User {
            message,
            parent_tool_use_id,
            ..
        } => {
            if parent_tool_use_id.is_some() && !show_thinking {
                return;
            }
            process_user_message(formatter, message, no_tools, parent_tool_use_id.as_deref());
        }
        LogEntry::Assistant {
            message,
            parent_tool_use_id,
            ..
        } => {
            if parent_tool_use_id.is_some() && !show_thinking {
                return;
            }
            process_assistant_message(
                formatter,
                message,
                no_tools,
                show_thinking,
                parent_tool_use_id.as_deref(),
            );
        }
    }
}

/// Process a user message using the provided formatter
fn process_user_message<F: OutputFormatter>(
    formatter: &mut F,
    message: &crate::claude::UserMessage,
    no_tools: bool,
    parent_id: Option<&str>,
) {
    let agent_id = parent_id.map(subagent_display_id);

    match &message.content {
        UserContent::String(text) => {
            if let Some(processed) = process_command_message(text) {
                if let Some(ref id) = agent_id {
                    formatter.format_agent_user_text(id, &processed);
                } else {
                    formatter.format_user_text(&processed);
                }
                formatter.end_message();
            }
        }
        UserContent::Blocks(blocks) => {
            let mut printed_content = false;
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        if let Some(processed) = process_command_message(text) {
                            if let Some(ref id) = agent_id {
                                formatter.format_agent_user_text(id, &processed);
                            } else {
                                formatter.format_user_text(&processed);
                            }
                            printed_content = true;
                        }
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        if !no_tools {
                            if let Some(ref id) = agent_id {
                                formatter.format_agent_tool_result(id, content.as_ref());
                            } else {
                                formatter.format_tool_result(content.as_ref());
                            }
                            printed_content = true;
                        }
                    }
                    _ => {}
                }
            }
            if printed_content {
                formatter.end_message();
            }
        }
    }
}

/// Helper struct to categorize assistant message content
struct FormattedMessage<'a> {
    text_blocks: Vec<&'a str>,
    tool_calls: Vec<(&'a str, &'a serde_json::Value)>,
    thinking_steps: Vec<&'a str>,
}

impl<'a> From<&'a AssistantMessage> for FormattedMessage<'a> {
    fn from(msg: &'a AssistantMessage) -> Self {
        let mut text_blocks = Vec::new();
        let mut tool_calls = Vec::new();
        let mut thinking_steps = Vec::new();

        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => text_blocks.push(text.as_str()),
                ContentBlock::ToolUse { name, input, .. } => {
                    tool_calls.push((name.as_str(), input))
                }
                ContentBlock::Thinking { thinking, .. } => thinking_steps.push(thinking.as_str()),
                _ => {}
            }
        }

        Self {
            text_blocks,
            tool_calls,
            thinking_steps,
        }
    }
}

/// Process an assistant message using the provided formatter
fn process_assistant_message<F: OutputFormatter>(
    formatter: &mut F,
    message: &AssistantMessage,
    no_tools: bool,
    show_thinking: bool,
    parent_id: Option<&str>,
) {
    let formatted = FormattedMessage::from(message);
    let mut printed_content = false;
    let agent_id = parent_id.map(subagent_display_id);

    // Print text blocks
    for text in formatted.text_blocks {
        if text.trim().is_empty() {
            continue;
        }
        if let Some(ref id) = agent_id {
            formatter.format_agent_assistant_text(id, text);
        } else {
            formatter.format_assistant_text(text);
        }
        printed_content = true;
    }

    // Print tool calls
    if !no_tools {
        for (tool_name, tool_input) in formatted.tool_calls {
            if let Some(ref id) = agent_id {
                formatter.format_agent_tool_call(id, tool_name, tool_input);
            } else {
                formatter.format_tool_call(tool_name, tool_input);
            }
            printed_content = true;
        }
    }

    // Print thinking blocks (skip for subagents)
    if show_thinking && agent_id.is_none() {
        for thought in formatted.thinking_steps {
            if thought.is_empty() {
                continue;
            }
            formatter.format_thinking(thought);
            printed_content = true;
        }
    }

    // Only add spacing if we printed something
    if printed_content {
        formatter.end_message();
    }
}

/// Get a truncated agent ID for display (max 7 characters)
fn short_agent_id(agent_id: &str) -> &str {
    &agent_id[..agent_id.len().min(7)]
}

/// Aggregate text content blocks and render them with the caller-specific formatter.
fn render_agent_text_blocks(
    blocks: &[crate::claude::ContentBlock],
    mut format_text: impl FnMut(&str),
) -> bool {
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|block| match block {
            crate::claude::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    if texts.is_empty() {
        return false;
    }

    let combined = texts.join("\n\n");
    format_text(&combined);
    true
}

/// Process an agent progress message using the provided formatter
fn process_agent_message<F: OutputFormatter>(
    formatter: &mut F,
    agent_progress: &crate::claude::AgentProgressData,
    no_tools: bool,
) {
    use crate::claude::{AgentContent, ContentBlock};

    let agent_id = &agent_progress.agent_id;
    let msg = &agent_progress.message;

    match msg.message_type.as_str() {
        "user" => {
            // User messages in agent context are typically tool results or the initial prompt
            let AgentContent::Blocks(blocks) = &msg.message.content;
            let mut printed = false;

            printed |= render_agent_text_blocks(blocks, |text| {
                formatter.format_agent_user_text(agent_id, text);
            });

            // Tool results
            for block in blocks {
                if let ContentBlock::ToolResult { content, .. } = block
                    && !no_tools
                {
                    formatter.format_agent_tool_result(agent_id, content.as_ref());
                    printed = true;
                }
            }

            if printed {
                formatter.end_message();
            }
        }
        "assistant" => {
            let AgentContent::Blocks(blocks) = &msg.message.content;
            let mut printed = false;

            printed |= render_agent_text_blocks(blocks, |text| {
                formatter.format_agent_assistant_text(agent_id, text);
            });

            // Tool calls
            for block in blocks {
                if let ContentBlock::ToolUse { name, input, .. } = block
                    && !no_tools
                {
                    formatter.format_agent_tool_call(agent_id, name, input);
                    printed = true;
                }
            }

            if printed {
                formatter.end_message();
            }
        }
        _ => {}
    }
}

/// Render a conversation in TUI ledger format to terminal (for debugging)
pub fn render_to_terminal(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    use crate::tui::{RenderOptions, render_conversation};
    use std::collections::BTreeSet;

    let terminal_width = get_terminal_width();
    let content_width = terminal_width.saturating_sub(NAME_WIDTH + SEPARATOR_WIDTH);

    let render_options = RenderOptions {
        tool_display: if options.no_tools {
            crate::tui::ToolDisplayMode::Hidden
        } else {
            crate::tui::ToolDisplayMode::Full
        },
        show_thinking: options.show_thinking,
        show_timing: false, // Non-TUI render doesn't support timing toggle
        content_width,
        expanded_tool_outputs: BTreeSet::new(),
        show_annotations: true,
        annotations: crate::annotations::for_conversation(file_path),
        focused_annotation: None,
    };

    let rendered = render_conversation(file_path, &render_options)?;
    let rendered_lines = rendered.lines;

    // Spawn pager if requested
    let mut pager_child = if options.use_pager {
        pager::spawn_pager().ok()
    } else {
        None
    };

    // Get writer - either pager stdin or stdout
    let mut stdout_handle = io::stdout().lock();
    let writer: &mut dyn Write = if let Some(ref mut child) = pager_child {
        child.stdin.as_mut().unwrap()
    } else {
        &mut stdout_handle
    };

    // Convert RenderedLine spans to colored terminal output
    'outer: for line in &rendered_lines {
        for (text, style) in &line.spans {
            // Apply styling only if colors are enabled
            let output: Box<dyn std::fmt::Display> = if options.no_color {
                Box::new(text.as_str())
            } else {
                let mut styled = text.as_str().normal();

                if let Some((r, g, b)) = style.fg {
                    styled = styled.custom_color(CustomColor { r, g, b });
                }
                if style.bold {
                    styled = styled.bold();
                }
                if style.dimmed {
                    styled = styled.dimmed();
                }
                if style.italic {
                    styled = styled.italic();
                }

                Box::new(styled)
            };

            // Stop if the output pipe is closed (e.g., pager quit)
            if write!(writer, "{}", output).is_err() {
                break 'outer;
            }
        }
        if writeln!(writer).is_err() {
            break;
        }
    }

    // Close stdin and wait for pager to finish
    drop(stdout_handle);
    if let Some(mut child) = pager_child {
        let _ = child.wait();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_command_message_skips_local_command_caveat() {
        let caveat = "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>";
        assert_eq!(process_command_message(caveat), None);
    }

    #[test]
    fn process_command_message_skips_local_command_caveat_with_whitespace() {
        let caveat = "  <local-command-caveat>Some caveat text</local-command-caveat>  ";
        assert_eq!(process_command_message(caveat), None);
    }

    #[test]
    fn process_command_message_preserves_normal_text() {
        assert_eq!(
            process_command_message("Hello world"),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn process_command_message_skips_empty_stdout() {
        assert_eq!(
            process_command_message("<local-command-stdout></local-command-stdout>"),
            None
        );
        assert_eq!(
            process_command_message("<local-command-stdout>   </local-command-stdout>"),
            None
        );
    }

    #[test]
    fn process_command_message_extracts_nonempty_stdout() {
        assert_eq!(
            process_command_message("<local-command-stdout>output here</local-command-stdout>"),
            Some("output here".to_string())
        );
    }

    #[test]
    fn process_command_message_skips_clear_command() {
        assert_eq!(
            process_command_message("<command-name>/clear</command-name>"),
            None
        );
        // Also skip clear with command-message and command-args tags
        assert_eq!(
            process_command_message(
                "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"
            ),
            None
        );
    }

    #[test]
    fn process_command_message_extracts_other_command_names() {
        assert_eq!(
            process_command_message("<command-name>/help</command-name>"),
            Some("/help".to_string())
        );
    }

    #[test]
    fn process_command_message_condenses_skill_invocation() {
        let skill_msg = "Base directory for this skill: /Users/raine/.claude/skills/consult\n\nConsult an external LLM with the user's query.\n\n**Arguments:** `how to add more aliases?`";
        assert_eq!(
            process_command_message(skill_msg),
            Some("*Skill: Consult an external LLM with the user's query.*".to_string())
        );
    }

    #[test]
    fn process_command_message_skill_invocation_fallback() {
        let skill_msg = "Base directory for this skill: /path/to/skill";
        assert_eq!(
            process_command_message(skill_msg),
            Some("*Skill: invoked*".to_string())
        );
    }
}
