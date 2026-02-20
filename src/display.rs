use crate::claude::{AssistantMessage, ContentBlock, LogEntry, UserContent};
use crate::cli::DebugLevel;
use crate::debug;
use crate::debug_log;
use crate::error::Result;
use crate::markdown::render_markdown;
use crate::pager;
use crate::tool_format;
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

// Colors matching the TUI theme
const TEAL: CustomColor = CustomColor {
    r: 78,
    g: 201,
    b: 176,
};
const DIM_TEAL: CustomColor = CustomColor {
    r: 60,
    g: 160,
    b: 140,
};
const SEPARATOR_COLOR: CustomColor = CustomColor {
    r: 80,
    g: 80,
    b: 80,
};
// Colors for tool formatting
const TOOL_TEXT: CustomColor = CustomColor {
    r: 140,
    g: 145,
    b: 150,
};
const DIFF_ADD: CustomColor = CustomColor {
    r: 120,
    g: 200,
    b: 120,
};
const DIFF_REMOVE: CustomColor = CustomColor {
    r: 220,
    g: 120,
    b: 120,
};

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

    /// Print lines in ledger format with a name on the first line
    fn print_lines<F>(&mut self, name: &str, style: F, text: &str)
    where
        F: Fn(&str) -> ColoredString,
    {
        let wrapped_lines = wrap_text(text, self.content_width);

        for (i, line) in wrapped_lines.iter().enumerate() {
            if i == 0 {
                let padded = format!("{:>width$}", name, width = NAME_WIDTH);
                let _ = write!(self.writer, "{}", style(&padded));
            } else {
                let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            }
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));
            let _ = writeln!(self.writer, "{}", line);
        }
    }

    /// Print continuation lines with dimmed content
    fn print_continuation(&mut self, text: &str) {
        for line in wrap_text(text, self.content_width) {
            let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));
            let _ = writeln!(self.writer, "{}", line.dimmed());
        }
    }

    /// Print tool body with diff-aware coloring
    fn print_tool_body(&mut self, text: &str) {
        for line in text.lines() {
            let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));
            if line.starts_with("+ ") {
                let _ = writeln!(self.writer, "{}", line.custom_color(DIFF_ADD));
            } else if line.starts_with("- ") {
                let _ = writeln!(self.writer, "{}", line.custom_color(DIFF_REMOVE));
            } else {
                let _ = writeln!(self.writer, "{}", line.dimmed());
            }
        }
    }

    /// Print pre-formatted markdown text with ledger layout
    fn print_markdown<F>(&mut self, name: &str, style: F, text: &str)
    where
        F: Fn(&str) -> ColoredString,
    {
        let lines: Vec<&str> = text.lines().collect();

        if lines.is_empty() {
            let padded = format!("{:>width$}", name, width = NAME_WIDTH);
            let _ = write!(self.writer, "{}", style(&padded));
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));
            let _ = writeln!(self.writer);
            return;
        }

        for (i, line) in lines.iter().enumerate() {
            if i == 0 {
                let padded = format!("{:>width$}", name, width = NAME_WIDTH);
                let _ = write!(self.writer, "{}", style(&padded));
            } else {
                let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            }
            let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));
            let _ = writeln!(self.writer, "{}", line);
        }
    }
}

impl<W: Write + ?Sized> OutputFormatter for LedgerFormatter<'_, W> {
    fn format_user_text(&mut self, text: &str) {
        let rendered = render_markdown(text, self.content_width);
        self.print_markdown("You", |s| s.white().bold(), &rendered);
    }

    fn format_assistant_text(&mut self, text: &str) {
        let rendered = render_markdown(text, self.content_width);
        self.print_markdown("Claude", |s| s.custom_color(TEAL).bold(), &rendered);
    }

    fn format_tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, self.content_width);

        // Print the header with appropriate styling
        let padded_name = format!("{:>width$}", "Claude", width = NAME_WIDTH);
        let _ = write!(self.writer, "{}", padded_name.custom_color(DIM_TEAL));
        let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));

        // Print the header in subtle gray
        let _ = writeln!(self.writer, "{}", formatted.header.custom_color(TOOL_TEXT));

        // Print the body if present, with empty line separator
        if let Some(body) = formatted.body {
            // Empty line between header and body
            let _ = write!(self.writer, "{:>width$}", "", width = NAME_WIDTH);
            let _ = writeln!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));
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
        self.print_markdown("↳ Result", |s| s.custom_color(TOOL_TEXT), &rendered);
    }

    fn format_thinking(&mut self, thought: &str) {
        self.print_lines("Thinking", |s| s.custom_color(DIM_TEAL), thought);
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
        self.print_markdown(&name, |s| s.custom_color(TEAL).dimmed(), &rendered);
    }

    fn format_agent_tool_call(&mut self, agent_id: &str, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, self.content_width);
        let label = format!("↳{}", short_agent_id(agent_id));

        // Print the header with appropriate styling (dimmed for subagents)
        let padded_name = format!("{:>width$}", label, width = NAME_WIDTH);
        let _ = write!(
            self.writer,
            "{}",
            padded_name.custom_color(DIM_TEAL).dimmed()
        );
        let _ = write!(self.writer, "{}", SEPARATOR.custom_color(SEPARATOR_COLOR));

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
            |s| s.custom_color(DIM_TEAL).dimmed(),
            "<Result>",
        );
        let content_str = format_tool_content(content);
        self.print_continuation(&content_str);
    }
}

/// Plain text formatter without formatting or alignment
struct PlainFormatter;

/// Default content width for plain text output
const PLAIN_CONTENT_WIDTH: usize = 80;

impl OutputFormatter for PlainFormatter {
    fn format_user_text(&mut self, text: &str) {
        println!("You: {}", text);
    }

    fn format_assistant_text(&mut self, text: &str) {
        println!("Claude: {}", text);
    }

    fn format_tool_call(&mut self, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
        println!("Claude: {}", formatted.header);
        if let Some(body) = formatted.body {
            for line in body.lines() {
                println!("  {}", line);
            }
        }
    }

    fn format_tool_result(&mut self, content: Option<&serde_json::Value>) {
        println!("Tool: <Result>");
        let content_str = format_tool_content(content);
        println!("{}", content_str);
    }

    fn format_thinking(&mut self, thought: &str) {
        println!("Thinking: {}", thought);
    }

    fn end_message(&mut self) {
        println!();
    }

    fn format_agent_user_text(&mut self, agent_id: &str, text: &str) {
        println!("  [{}] User: {}", short_agent_id(agent_id), text);
    }

    fn format_agent_assistant_text(&mut self, agent_id: &str, text: &str) {
        println!("  [{}] Agent: {}", short_agent_id(agent_id), text);
    }

    fn format_agent_tool_call(&mut self, agent_id: &str, name: &str, input: &serde_json::Value) {
        let formatted = tool_format::format_tool_call(name, input, PLAIN_CONTENT_WIDTH);
        println!(
            "  [{}] Agent: {}",
            short_agent_id(agent_id),
            formatted.header
        );
        if let Some(body) = formatted.body {
            for line in body.lines() {
                println!("    {}", line);
            }
        }
    }

    fn format_agent_tool_result(&mut self, _agent_id: &str, content: Option<&serde_json::Value>) {
        println!("    Tool: <Result>");
        let content_str = format_tool_content(content);
        for line in content_str.lines() {
            println!("    {}", line);
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

/// Process user message text to handle command-related XML tags
/// Returns None if the message should be skipped entirely (e.g., empty local-command-stdout)
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

    // Return original text for non-command messages
    Some(text.to_string())
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

/// Display a conversation from a file
pub fn display_conversation(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let terminal_width = get_terminal_width();
    let content_width = terminal_width.saturating_sub(NAME_WIDTH + SEPARATOR_WIDTH);

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

    let mut formatter = LedgerFormatter::new(writer, content_width);

    for (line_number, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => {
                process_entry(
                    &mut formatter,
                    &entry,
                    options.no_tools,
                    options.show_thinking,
                );
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

    // Close stdin and wait for pager to finish
    drop(stdout_handle);
    if let Some(mut child) = pager_child {
        let _ = child.wait();
    }

    Ok(())
}

/// Display a conversation in plain text format (no ledger formatting)
pub fn display_conversation_plain(file_path: &Path, options: &DisplayOptions) -> Result<()> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut formatter = PlainFormatter;

    for (line_number, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => {
                process_entry(
                    &mut formatter,
                    &entry,
                    options.no_tools,
                    options.show_thinking,
                );
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
        | LogEntry::System { .. } => {
            // Skip metadata entries
        }
        LogEntry::Progress { data, .. } => {
            // Handle agent_progress entries (only when show_thinking is enabled)
            if show_thinking && let Some(agent_progress) = crate::claude::parse_agent_progress(data)
            {
                process_agent_message(formatter, &agent_progress, no_tools);
            }
        }
        LogEntry::User { message, .. } => {
            process_user_message(formatter, message, no_tools);
        }
        LogEntry::Assistant { message, .. } => {
            process_assistant_message(formatter, message, no_tools, show_thinking);
        }
    }
}

/// Process a user message using the provided formatter
fn process_user_message<F: OutputFormatter>(
    formatter: &mut F,
    message: &crate::claude::UserMessage,
    no_tools: bool,
) {
    match &message.content {
        UserContent::String(text) => {
            if let Some(processed) = process_command_message(text) {
                formatter.format_user_text(&processed);
                formatter.end_message();
            }
        }
        UserContent::Blocks(blocks) => {
            let mut printed_content = false;
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        if let Some(processed) = process_command_message(text) {
                            formatter.format_user_text(&processed);
                            printed_content = true;
                        }
                    }
                    ContentBlock::ToolResult { content, .. } => {
                        if !no_tools {
                            formatter.format_tool_result(content.as_ref());
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
) {
    let formatted = FormattedMessage::from(message);
    let mut printed_content = false;

    // Print text blocks
    for text in formatted.text_blocks {
        if text.trim().is_empty() {
            continue;
        }
        formatter.format_assistant_text(text);
        printed_content = true;
    }

    // Print tool calls
    if !no_tools {
        for (tool_name, tool_input) in formatted.tool_calls {
            formatter.format_tool_call(tool_name, tool_input);
            printed_content = true;
        }
    }

    // Print thinking blocks
    if show_thinking {
        for thought in formatted.thinking_steps {
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
                formatter.format_agent_user_text(agent_id, &combined);
                printed = true;
            }

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
                formatter.format_agent_assistant_text(agent_id, &combined);
                printed = true;
            }

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
        assistant_label: "Claude".to_string(),
        assistant_color: (78, 201, 176),     // TEAL
        assistant_dim_color: (60, 160, 140),  // DIM_TEAL
    };

    let rendered_lines = render_conversation(file_path, &render_options)?;

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
}
