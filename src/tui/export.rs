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

use crate::claude::{self, ContentBlock, LogEntry, UserContent, UserMessage};
use crate::tool_format;
use chrono::Local;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

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
    fn extension(&self) -> &'static str {
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
#[derive(Clone, Copy, Debug, Default)]
pub struct ExportOptions {
    pub show_tools: bool,
    pub show_thinking: bool,
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

/// Copy text to the system clipboard.
///
/// On Linux, pipes to `xclip` or `xsel` first (these persist the data
/// independently of the calling process). Falls back to arboard if
/// neither tool is available.
pub fn copy_to_system_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // Try xclip first, then xsel
        if let Ok(result) = copy_via_command("xclip", &["-selection", "clipboard"], text) {
            return result;
        }
        if let Ok(result) = copy_via_command("xsel", &["--clipboard", "--input"], text) {
            return result;
        }
        // Fall through to arboard
    }

    // arboard fallback (primary method on macOS/Windows)
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => clipboard
            .set_text(text)
            .map_err(|e| format!("Clipboard error: {}", e)),
        Err(e) => Err(format!("Clipboard unavailable: {}", e)),
    }
}

/// Try to copy text via an external command (e.g. xclip, xsel).
/// Returns `Ok(Ok(()))` on success, `Ok(Err(msg))` if the command ran but failed,
/// or `Err(())` if the command was not found (caller should try next option).
#[cfg(target_os = "linux")]
fn copy_via_command(cmd: &str, args: &[&str], text: &str) -> Result<Result<(), String>, ()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| ())?; // command not found → try next

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }

    match child.wait() {
        Ok(status) if status.success() => Ok(Ok(())),
        Ok(status) => Ok(Err(format!("{} exited with {}", cmd, status))),
        Err(e) => Ok(Err(format!("{} error: {}", cmd, e))),
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

    match copy_to_system_clipboard(&content) {
        Ok(()) => ExportResult {
            message: "Copied to clipboard".to_string(),
        },
        Err(e) => ExportResult { message: e },
    }
}

/// Generate content in the specified format
fn generate_content(
    source_path: &Path,
    format: ExportFormat,
    options: ExportOptions,
) -> std::io::Result<String> {
    match format {
        ExportFormat::Jsonl => fs::read_to_string(source_path),
        ExportFormat::Plain => generate_plain(source_path, options),
        ExportFormat::Markdown => generate_markdown(source_path, options),
        ExportFormat::Ledger => generate_ledger(source_path, options),
    }
}

/// Generate plain text format (simple "Speaker: message" lines)
fn generate_plain(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut output = String::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            match entry {
                LogEntry::User {
                    message,
                    parent_tool_use_id,
                    ..
                } => {
                    if parent_tool_use_id.is_some() && !options.show_thinking {
                        continue;
                    }
                    let prefix = subagent_prefix(&parent_tool_use_id);
                    if let Some(text) = extract_user_text(&message) {
                        output.push_str(&format!("{}You: {}\n\n", prefix, text));
                    }
                    // Tool results
                    if options.show_tools
                        && let UserContent::Blocks(blocks) = &message.content
                    {
                        for block in blocks {
                            if let ContentBlock::ToolResult { content, .. } = block {
                                let content_str = format_tool_result_for_export(content.as_ref());
                                output.push_str(&format!(
                                    "{}Tool Result: {}\n\n",
                                    prefix, content_str
                                ));
                            }
                        }
                    }
                }
                LogEntry::Assistant {
                    message,
                    parent_tool_use_id,
                    ..
                } => {
                    if parent_tool_use_id.is_some() && !options.show_thinking {
                        continue;
                    }
                    let prefix = subagent_prefix(&parent_tool_use_id);
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => {
                                output.push_str(&format!("{}Claude: {}\n\n", prefix, text));
                            }
                            ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                                let formatted = format_tool_call_for_export(name, input);
                                output.push_str(&format!("{}Tool: {}\n\n", prefix, formatted));
                            }
                            ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                                output.push_str(&format!("{}Thinking: {}\n\n", prefix, thinking));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(output)
}

/// Generate markdown format (with ## headers for speakers)
fn generate_markdown(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut output = String::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            match entry {
                LogEntry::User {
                    message,
                    parent_tool_use_id,
                    ..
                } => {
                    if parent_tool_use_id.is_some() && !options.show_thinking {
                        continue;
                    }
                    let prefix = subagent_prefix(&parent_tool_use_id);
                    if let Some(text) = extract_user_text(&message) {
                        output.push_str(&format!("## {}You\n\n{}\n\n", prefix, text));
                    }
                    // Tool results
                    if options.show_tools
                        && let UserContent::Blocks(blocks) = &message.content
                    {
                        for block in blocks {
                            if let ContentBlock::ToolResult { content, .. } = block {
                                let content_str = format_tool_result_for_export(content.as_ref());
                                let fenced = markdown_code_fence(&content_str);
                                output.push_str(&format!(
                                    "### {}Tool Result\n\n{}\n\n",
                                    prefix, fenced
                                ));
                            }
                        }
                    }
                }
                LogEntry::Assistant {
                    message,
                    parent_tool_use_id,
                    ..
                } => {
                    if parent_tool_use_id.is_some() && !options.show_thinking {
                        continue;
                    }
                    let prefix = subagent_prefix(&parent_tool_use_id);
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => {
                                output.push_str(&format!("## {}Claude\n\n{}\n\n", prefix, text));
                            }
                            ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                                let formatted = format_tool_call_for_export(name, input);
                                let fenced = markdown_code_fence(&formatted);
                                output.push_str(&format!(
                                    "### {}Tool: {}\n\n{}\n\n",
                                    prefix, name, fenced
                                ));
                            }
                            ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                                output.push_str(&format!(
                                    "### {}Thinking\n\n{}\n\n",
                                    prefix, thinking
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(output)
}

/// Total line width for ledger export (including name column and separator)
const LEDGER_WIDTH: usize = 90;

/// Generate ledger-style format (formatted like the TUI viewer)
fn generate_ledger(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut output = String::new();

    const NAME_WIDTH: usize = 9;
    // 3 for " │ " separator
    let content_width = LEDGER_WIDTH - NAME_WIDTH - 3;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<LogEntry>(&line) {
            match entry {
                LogEntry::User {
                    message,
                    parent_tool_use_id,
                    ..
                } => {
                    if parent_tool_use_id.is_some() && !options.show_thinking {
                        continue;
                    }
                    let speaker = match &parent_tool_use_id {
                        Some(id) => format!("↳{}", claude::short_parent_id(id)),
                        None => "You".to_string(),
                    };
                    if let Some(text) = extract_user_text(&message) {
                        let wrapped = wrap_plain_text(&text, content_width);
                        append_ledger_block(&mut output, &speaker, &wrapped, NAME_WIDTH);
                        output.push('\n');
                    }
                    // Tool results
                    if options.show_tools
                        && let UserContent::Blocks(blocks) = &message.content
                    {
                        for block in blocks {
                            if let ContentBlock::ToolResult { content, .. } = block {
                                let content_str = format_tool_result_for_export(content.as_ref());
                                if content_str.trim().is_empty() {
                                    continue;
                                }
                                let wrapped = wrap_plain_text(&content_str, content_width);
                                append_ledger_block(&mut output, "↳ Result", &wrapped, NAME_WIDTH);
                                output.push('\n');
                            }
                        }
                    }
                }
                LogEntry::Assistant {
                    message,
                    parent_tool_use_id,
                    ..
                } => {
                    if parent_tool_use_id.is_some() && !options.show_thinking {
                        continue;
                    }
                    let speaker = match &parent_tool_use_id {
                        Some(id) => format!("↳{}", claude::short_parent_id(id)),
                        None => "Claude".to_string(),
                    };
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => {
                                let rendered =
                                    crate::markdown::render_markdown_plain(text, content_width);
                                let rendered = rendered.trim_end();
                                append_ledger_block(&mut output, &speaker, rendered, NAME_WIDTH);
                                output.push('\n');
                            }
                            ContentBlock::ToolUse { name, input, .. } if options.show_tools => {
                                let formatted =
                                    format_tool_call_for_ledger(name, input, content_width);
                                let tool_label = if parent_tool_use_id.is_some() {
                                    &speaker
                                } else {
                                    "Tool"
                                };
                                append_ledger_block(
                                    &mut output,
                                    tool_label,
                                    &formatted,
                                    NAME_WIDTH,
                                );
                                output.push('\n');
                            }
                            ContentBlock::Thinking { thinking, .. } if options.show_thinking => {
                                let rendered =
                                    crate::markdown::render_markdown_plain(thinking, content_width);
                                let rendered = rendered.trim_end();
                                append_ledger_block(&mut output, "Thinking", rendered, NAME_WIDTH);
                                output.push('\n');
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(output)
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

/// Generate a prefix string for subagent entries in exports.
/// Returns "[↳ID] " for nested entries, empty string for top-level.
fn subagent_prefix(parent_tool_use_id: &Option<String>) -> String {
    match parent_tool_use_id {
        Some(id) => format!("[↳{}] ", claude::short_parent_id(id)),
        None => String::new(),
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

/// Default width for non-ledger export (no wrapping needed for markdown export)
const EXPORT_WIDTH: usize = usize::MAX;

/// Format a tool call for export (non-ledger formats)
fn format_tool_call_for_export(name: &str, input: &serde_json::Value) -> String {
    let formatted = tool_format::format_tool_call(name, input, EXPORT_WIDTH);
    match formatted.body {
        Some(body) => format!("{}\n{}", formatted.header, body),
        None => formatted.header,
    }
}

/// Format a tool call for ledger export with line wrapping
fn format_tool_call_for_ledger(name: &str, input: &serde_json::Value, max_width: usize) -> String {
    let formatted = tool_format::format_tool_call(name, input, max_width);
    let text = match formatted.body {
        Some(body) => format!("{}\n{}", formatted.header, body),
        None => formatted.header,
    };
    // Wrap any remaining long lines
    wrap_plain_text(&text, max_width)
}

/// Wrap plain text to max_width, preserving existing line breaks
fn wrap_plain_text(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        if line.is_empty() {
            continue;
        }
        let wrapped: Vec<_> = textwrap::wrap(line, max_width)
            .into_iter()
            .map(|cow| cow.into_owned())
            .collect();
        for (j, w) in wrapped.iter().enumerate() {
            if j > 0 {
                result.push('\n');
            }
            result.push_str(w);
        }
    }
    result
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_plain_text_preserves_short_lines() {
        let result = wrap_plain_text("short line", 80);
        assert_eq!(result, "short line");
    }

    #[test]
    fn test_wrap_plain_text_wraps_long_line() {
        let long = "word ".repeat(20); // 100 chars
        let result = wrap_plain_text(long.trim(), 40);
        for line in result.lines() {
            assert!(line.len() <= 40, "Line exceeds max_width: {:?}", line);
        }
        // All words should be preserved
        assert_eq!(result.matches("word").count(), 20);
    }

    #[test]
    fn test_wrap_plain_text_preserves_existing_newlines() {
        let text = "line one\nline two\nline three";
        let result = wrap_plain_text(text, 80);
        assert_eq!(result.lines().count(), 3);
    }

    #[test]
    fn test_wrap_plain_text_preserves_empty_lines() {
        let text = "line one\n\nline three";
        let result = wrap_plain_text(text, 80);
        assert_eq!(result, "line one\n\nline three");
    }

    #[test]
    fn test_append_ledger_block_format() {
        let mut output = String::new();
        append_ledger_block(&mut output, "Claude", "Hello\nWorld", 9);
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("   Claude │ Hello"));
        assert!(lines[1].starts_with("          │ World"));
    }

    #[test]
    fn test_ledger_line_width() {
        // Verify that a wrapped line fits within LEDGER_WIDTH
        let name_width = 9;
        let content_width = LEDGER_WIDTH - name_width - 3;
        let long_text = "word ".repeat(20);
        let wrapped = wrap_plain_text(long_text.trim(), content_width);
        let mut output = String::new();
        append_ledger_block(&mut output, "Claude", &wrapped, name_width);
        for line in output.lines() {
            // Count display width (name + " │ " + content)
            let width = line.chars().count();
            assert!(
                width <= LEDGER_WIDTH,
                "Ledger line exceeds {} chars (got {}): {:?}",
                LEDGER_WIDTH,
                width,
                line
            );
        }
    }

    #[test]
    fn test_ledger_markdown_rendering() {
        // Verify that markdown is rendered (not raw) in ledger export
        let content_width = LEDGER_WIDTH - 9 - 3;
        let rendered =
            crate::markdown::render_markdown_plain("This has **bold** and `code`", content_width);
        // Should not contain markdown formatting markers for bold
        assert!(
            !rendered.contains("**"),
            "Should strip bold markers: {:?}",
            rendered
        );
        // Should contain backticks for inline code
        assert!(
            rendered.contains("`code`"),
            "Should keep inline code backticks: {:?}",
            rendered
        );
        // Should not contain ANSI codes
        assert!(
            !rendered.contains("\x1b"),
            "Should not contain ANSI codes: {:?}",
            rendered
        );
    }

    #[test]
    fn test_generate_ledger_wraps_and_renders() {
        // Create a sample JSONL with a long assistant message containing markdown
        let long_text = "This is a **really long** sentence that should definitely wrap because it contains many words and exceeds the content width of the ledger format which is 68 characters.";
        let entry = serde_json::json!({
            "type": "assistant",
            "message": {
                "id": "test",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": long_text}],
                "model": "test",
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            },
            "timestamp": "2024-01-01T00:00:00Z"
        });

        let tmpdir = std::env::temp_dir();
        let tmppath = tmpdir.join("claude-history-test-ledger.jsonl");
        std::fs::write(&tmppath, format!("{}\n", entry)).unwrap();

        let result = generate_ledger(
            &tmppath,
            ExportOptions {
                show_tools: false,
                show_thinking: false,
            },
        )
        .unwrap();

        std::fs::remove_file(&tmppath).ok();

        eprintln!("Ledger output:\n{}", result);

        // Every line should fit within LEDGER_WIDTH
        for line in result.lines() {
            if line.is_empty() {
                continue;
            }
            let width = line.chars().count();
            assert!(
                width <= LEDGER_WIDTH,
                "Ledger line exceeds {} chars (got {}): {:?}",
                LEDGER_WIDTH,
                width,
                line
            );
        }

        // Should contain the speaker name
        assert!(result.contains("Claude"), "Should have speaker name");
        // Should not contain ANSI codes
        assert!(!result.contains("\x1b"), "Should not contain ANSI codes");
        // Bold markers should be stripped (markdown rendered)
        assert!(
            !result.contains("**"),
            "Should not contain raw bold markers"
        );
        // Content should be wrapped across multiple lines
        let content_lines: Vec<&str> = result.lines().filter(|l| !l.is_empty()).collect();
        assert!(
            content_lines.len() > 1,
            "Long text should wrap to multiple lines, got: {:?}",
            content_lines
        );
    }
}
