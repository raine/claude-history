//! Conversation export functionality.
//!
//! This module provides functions to export conversations in different formats:
//! - Ledger format (formatted text with speaker names)
//! - Plain text (simple speaker: message format)
//! - Markdown (with headers for speakers)
//! - JSONL (raw format)
//!
//! Conversations can be exported to files or copied to the clipboard.
//! Export respects the current display settings for thinking blocks and tool calls.

use crate::claude::{ContentBlock, LogEntry, UserContent, UserMessage};
use crate::tool_format;
use arboard::Clipboard;
use chrono::Local;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Export format options
#[derive(Clone, Copy, Debug)]
pub enum ExportFormat {
    Ledger,
    Plain,
    Markdown,
    Jsonl,
}

impl ExportFormat {
    /// Get format from menu option index (0-3)
    pub fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(ExportFormat::Ledger),
            1 => Some(ExportFormat::Plain),
            2 => Some(ExportFormat::Markdown),
            3 => Some(ExportFormat::Jsonl),
            _ => None,
        }
    }

    /// Get file extension for this format
    pub fn extension(&self) -> &'static str {
        match self {
            ExportFormat::Ledger | ExportFormat::Plain => "txt",
            ExportFormat::Markdown => "md",
            ExportFormat::Jsonl => "jsonl",
        }
    }
}

/// Result of an export operation
pub struct ExportResult {
    pub message: String,
}

/// Options for export content generation
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    pub show_tools: bool,
    pub show_thinking: bool,
    /// Label for assistant messages (e.g., "Claude" or "Cursor")
    pub assistant_label: String,
}

/// Export conversation to file
pub fn export_to_file(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
) -> ExportResult {
    let timestamp = Local::now().format("%Y-%m-%d-%H%M%S");
    let ext = format.extension();
    let filename = format!("conversation-{}.{}", timestamp, ext);

    let content = match generate_content(source_path, format, options) {
        Ok(c) => c,
        Err(e) => {
            return ExportResult {
                message: format!("Failed to read: {}", e),
            };
        }
    };

    match fs::write(&filename, &content) {
        Ok(_) => ExportResult {
            message: format!("Exported to {}", filename),
        },
        Err(e) => ExportResult {
            message: format!("Failed to write: {}", e),
        },
    }
}

/// Copy conversation to clipboard
pub fn export_to_clipboard(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
) -> ExportResult {
    let content = match generate_content(source_path, format, options) {
        Ok(c) => c,
        Err(e) => {
            return ExportResult {
                message: format!("Failed to read: {}", e),
            };
        }
    };

    match Clipboard::new() {
        Ok(mut clipboard) => match clipboard.set_text(&content) {
            Ok(_) => ExportResult {
                message: "Copied to clipboard".to_string(),
            },
            Err(e) => ExportResult {
                message: format!("Clipboard error: {}", e),
            },
        },
        Err(e) => ExportResult {
            message: format!("Clipboard unavailable: {}", e),
        },
    }
}

/// Generate content in the specified format from a file path
fn generate_content(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
) -> std::io::Result<String> {
    match format {
        ExportFormat::Jsonl => fs::read_to_string(source_path),
        _ => {
            let entries = read_entries_from_file(source_path)?;
            Ok(generate_content_from_entries(&entries, format, options))
        }
    }
}

/// Read log entries from a JSONL file for export
fn read_entries_from_file(path: &Path) -> std::io::Result<Vec<LogEntry>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    Ok(reader
        .lines()
        .filter_map(|l| l.ok())
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect())
}

/// Generate content in the specified format from pre-parsed entries
pub fn generate_content_from_entries(
    entries: &[LogEntry],
    format: ExportFormat,
    options: ExportOptions,
) -> String {
    match format {
        ExportFormat::Plain => generate_plain_from_entries(entries, options),
        ExportFormat::Markdown => generate_markdown_from_entries(entries, options),
        ExportFormat::Ledger => generate_ledger_from_entries(entries, options),
        ExportFormat::Jsonl => {
            // JSONL from entries is not supported (use file-based export instead)
            String::from("<JSONL export requires file path>")
        }
    }
}

/// Generate plain text format from entries
fn generate_plain_from_entries(entries: &[LogEntry], options: ExportOptions) -> String {
    let mut output = String::new();

    for entry in entries {
        match entry {
            LogEntry::User { message, .. } => {
                if let Some(text) = extract_user_text(message) {
                    output.push_str(&format!("You: {}\n\n", text));
                }
                if options.show_tools {
                    if let UserContent::Blocks(blocks) = &message.content {
                        for block in blocks {
                            if let ContentBlock::ToolResult { content, .. } = block {
                                let content_str = format_tool_result_for_export(content.as_ref());
                                output.push_str(&format!("Tool Result: {}\n\n", content_str));
                            }
                        }
                    }
                }
            }
            LogEntry::Assistant { message, .. } => {
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            output.push_str(&format!("{}: {}\n\n", options.assistant_label, text));
                        }
                        ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                            let formatted = format_tool_call_for_export(name, input);
                            output.push_str(&format!("Tool: {}\n\n", formatted));
                        }
                        ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                            output.push_str(&format!("Thinking: {}\n\n", thinking));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    output
}

/// Generate markdown format from entries
fn generate_markdown_from_entries(entries: &[LogEntry], options: ExportOptions) -> String {
    let mut output = String::new();

    for entry in entries {
        match entry {
            LogEntry::User { message, .. } => {
                if let Some(text) = extract_user_text(message) {
                    output.push_str(&format!("## You\n\n{}\n\n", text));
                }
                if options.show_tools {
                    if let UserContent::Blocks(blocks) = &message.content {
                        for block in blocks {
                            if let ContentBlock::ToolResult { content, .. } = block {
                                let content_str = format_tool_result_for_export(content.as_ref());
                                let fenced = markdown_code_fence(&content_str);
                                output.push_str(&format!("### Tool Result\n\n{}\n\n", fenced));
                            }
                        }
                    }
                }
            }
            LogEntry::Assistant { message, .. } => {
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            output.push_str(&format!("## {}\n\n{}\n\n", options.assistant_label, text));
                        }
                        ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                            let formatted = format_tool_call_for_export(name, input);
                            let fenced = markdown_code_fence(&formatted);
                            output.push_str(&format!("### Tool: {}\n\n{}\n\n", name, fenced));
                        }
                        ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                            output.push_str(&format!("### Thinking\n\n{}\n\n", thinking));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    output
}

/// Generate ledger-style format from entries
fn generate_ledger_from_entries(entries: &[LogEntry], options: ExportOptions) -> String {
    let mut output = String::new();
    const NAME_WIDTH: usize = 9;

    for entry in entries {
        match entry {
            LogEntry::User { message, .. } => {
                if let Some(text) = extract_user_text(message) {
                    append_ledger_block(&mut output, "You", &text, NAME_WIDTH);
                    output.push('\n');
                }
                if options.show_tools {
                    if let UserContent::Blocks(blocks) = &message.content {
                        for block in blocks {
                            if let ContentBlock::ToolResult { content, .. } = block {
                                let content_str = format_tool_result_for_export(content.as_ref());
                                append_ledger_block(
                                    &mut output,
                                    "↳ Result",
                                    &content_str,
                                    NAME_WIDTH,
                                );
                                output.push('\n');
                            }
                        }
                    }
                }
            }
            LogEntry::Assistant { message, .. } => {
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            append_ledger_block(&mut output, &options.assistant_label, text, NAME_WIDTH);
                            output.push('\n');
                        }
                        ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                            let formatted = format_tool_call_for_export(name, input);
                            append_ledger_block(&mut output, "Tool", &formatted, NAME_WIDTH);
                            output.push('\n');
                        }
                        ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                            append_ledger_block(&mut output, "Thinking", thinking, NAME_WIDTH);
                            output.push('\n');
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    output
}

/// Append a ledger-formatted block to the output
fn append_ledger_block(output: &mut String, speaker: &str, text: &str, name_width: usize) {
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            output.push_str(&format!(
                "{:>width$} │ {}\n",
                speaker,
                line,
                width = name_width
            ));
        } else {
            output.push_str(&format!("{:>width$} │ {}\n", "", line, width = name_width));
        }
    }
}

/// Extract text from a user message, handling command messages
fn extract_user_text(message: &UserMessage) -> Option<String> {
    match &message.content {
        UserContent::String(s) => process_command_text(s),
        UserContent::Blocks(blocks) => {
            for block in blocks {
                if let ContentBlock::Text { text } = block
                    && let Some(processed) = process_command_text(text)
                {
                    return Some(processed);
                }
            }
            None
        }
    }
}

/// Process command message text, extracting content from XML tags
fn process_command_text(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // Handle <local-command-stdout> tags
    if trimmed.starts_with("<local-command-stdout>") && trimmed.ends_with("</local-command-stdout>")
    {
        let inner = &trimmed
            ["<local-command-stdout>".len()..trimmed.len() - "</local-command-stdout>".len()];
        if inner.trim().is_empty() {
            return None;
        }
        return Some(inner.trim().to_string());
    }

    // Handle <command-name> tags
    if let Some(start) = trimmed.find("<command-name>")
        && let Some(end) = trimmed.find("</command-name>")
    {
        let content_start = start + "<command-name>".len();
        if content_start < end {
            let command_name = &trimmed[content_start..end];

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

    Some(text.to_string())
}

/// Wrap content in markdown code fence, handling nested backticks
fn markdown_code_fence(content: &str) -> String {
    // Find the longest run of backticks in content and use one more
    let max_backticks = content
        .split(|c| c != '`')
        .map(|s| s.len())
        .max()
        .unwrap_or(0);
    let fence_len = std::cmp::max(3, max_backticks + 1);
    let fence: String = std::iter::repeat_n('`', fence_len).collect();
    format!("{}\n{}\n{}", fence, content, fence)
}

/// Default width for export (no wrapping needed for markdown export)
const EXPORT_WIDTH: usize = usize::MAX;

/// Format a tool call for export
fn format_tool_call_for_export(name: &str, input: &serde_json::Value) -> String {
    // Use large width to avoid wrapping in export (full command on one line is better for copying)
    let formatted = tool_format::format_tool_call(name, input, EXPORT_WIDTH);
    match formatted.body {
        Some(body) => format!("{}\n{}", formatted.header, body),
        None => formatted.header,
    }
}

/// Format tool result content for export
fn format_tool_result_for_export(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => {
            // Handle array of content blocks
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect();
            if !texts.is_empty() {
                texts.join("\n\n")
            } else {
                serde_json::to_string_pretty(&arr).unwrap_or_else(|_| "<error>".to_string())
            }
        }
        Some(value) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "<error>".to_string())
        }
        None => "<no content>".to_string(),
    }
}
