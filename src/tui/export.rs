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

use crate::tool_format;
use crate::tui::viewer::{NAME_WIDTH, SEPARATOR_WIDTH};
use crate::tui::{RenderOptions, ToolDisplayMode, render_conversation};
use crate::turns::{self, Part, Speaker, Turn, Visibility};
use chrono::Local;
use crossterm::clipboard::CopyToClipboard;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
#[cfg(target_os = "linux")]
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

impl ExportOptions {
    fn visibility(self) -> Visibility {
        Visibility {
            tools: self.show_tools,
            thinking: self.show_thinking,
        }
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardDestination {
    System,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClipboardTransport {
    System,
    Osc52,
}

const CLIPBOARD_TRANSPORT_ENV: &str = "CLAUDE_HISTORY_CLIPBOARD";
const REMOTE_SESSION_ENV_VARS: [&str; 4] =
    ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "MOSH_CONNECTION"];

fn clipboard_transport() -> Result<ClipboardTransport, String> {
    clipboard_transport_from_env(|name| std::env::var_os(name))
}

fn clipboard_transport_from_env(
    mut var: impl FnMut(&str) -> Option<OsString>,
) -> Result<ClipboardTransport, String> {
    let override_value = var(CLIPBOARD_TRANSPORT_ENV);
    let mut remote_transport = || {
        if REMOTE_SESSION_ENV_VARS
            .iter()
            .any(|name| var(name).is_some())
        {
            ClipboardTransport::Osc52
        } else {
            ClipboardTransport::System
        }
    };

    match override_value {
        None => Ok(remote_transport()),
        Some(value) => match value.to_str() {
            Some("auto") => Ok(remote_transport()),
            Some("system") => Ok(ClipboardTransport::System),
            Some("osc52") => Ok(ClipboardTransport::Osc52),
            _ => Err(format!(
                "Invalid {CLIPBOARD_TRANSPORT_ENV}: expected auto, system, or osc52"
            )),
        },
    }
}

fn copy_via_terminal(mut writer: impl Write, text: &str) -> Result<(), String> {
    crossterm::execute!(writer, CopyToClipboard::to_clipboard_from(text))
        .map_err(|e| format!("Terminal clipboard error: {e}"))
}

/// Copy text to the clipboard appropriate for this terminal session.
///
/// Remote sessions use OSC 52 so the terminal host receives the text. Local
/// sessions use the operating system clipboard. `CLAUDE_HISTORY_CLIPBOARD`
/// overrides selection with `auto`, `system`, or `osc52`.
pub fn copy_to_system_clipboard(text: &str) -> Result<ClipboardDestination, String> {
    if clipboard_transport()? == ClipboardTransport::Osc52 {
        copy_via_terminal(std::io::stderr(), text)?;
        return Ok(ClipboardDestination::Terminal);
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = linux_clipboard_candidates();
        for (cmd, args) in &candidates {
            match copy_via_command(cmd, args, text) {
                Ok(Ok(())) => return Ok(ClipboardDestination::System),
                Ok(Err(_)) => continue,
                Err(()) => continue,
            }
        }
    }

    match arboard::Clipboard::new() {
        Ok(mut clipboard) => clipboard
            .set_text(text)
            .map(|()| ClipboardDestination::System)
            .map_err(|e| format!("Clipboard error: {e}")),
        Err(e) => Err(format!("Clipboard unavailable: {e}")),
    }
}

/// Return clipboard tool candidates based on the active display server.
#[cfg(target_os = "linux")]
fn linux_clipboard_candidates() -> Vec<(&'static str, &'static [&'static str])> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();

    let mut candidates = Vec::new();
    if wayland {
        candidates.push(("wl-copy", ["--type", "text/plain;charset=utf-8"].as_slice()));
    }
    if x11 {
        candidates.push(("xclip", ["-selection", "clipboard"].as_slice()));
        candidates.push(("xsel", ["--clipboard", "--input"].as_slice()));
    }
    candidates
}

/// Try to copy text via an external command (e.g. wl-copy, xclip, xsel).
/// Returns `Ok(Ok(()))` on success, `Ok(Err(msg))` if the command ran but failed,
/// or `Err(())` if the command was not found (caller should try next option).
#[cfg(target_os = "linux")]
fn copy_via_command(cmd: &str, args: &[&str], text: &str) -> Result<Result<(), String>, ()> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ())?; // command not available → try next

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
        Ok(ClipboardDestination::System) => ExportResult {
            message: "Copied to clipboard".to_string(),
        },
        Ok(ClipboardDestination::Terminal) => ExportResult {
            message: "Sent to terminal clipboard".to_string(),
        },
        Err(e) => ExportResult { message: e },
    }
}

/// Extract the text content of a single message by its entry index in the JSONL file.
/// Returns the message text suitable for clipboard copying.
pub fn extract_message_text(
    source_path: &Path,
    entry_index: usize,
    options: ExportOptions,
) -> Result<String, String> {
    let turns = turns::project_file(source_path, options.visibility())
        .map_err(|e| format!("Failed to read: {e}"))?;
    let turn = turns
        .iter()
        .find(|turn| turn.entry_index == entry_index)
        .ok_or_else(|| "Message not found".to_string())?;
    let mut output = String::new();
    for part in &turn.parts {
        let text = match part {
            Part::Text(text) | Part::Thinking(text) => text.clone(),
            Part::ToolCall { name, input } => format_tool_call_for_export(name, input),
            Part::ToolResult(content) => turns::tool_result_text(content.as_ref()),
        };
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&text);
    }
    Ok(output)
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

fn project_turns(path: &Path, options: ExportOptions) -> std::io::Result<Vec<Turn>> {
    turns::project_file(path, options.visibility())
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// "[↳ID] " for subagent turns, empty for top-level ones.
fn subagent_prefix(turn: &Turn) -> String {
    match &turn.subagent {
        Some(id) => format!("[↳{id}] "),
        None => String::new(),
    }
}

fn speaker_name(turn: &Turn) -> String {
    match &turn.speaker {
        Speaker::User => "You".to_owned(),
        Speaker::Assistant { name } => name.clone().unwrap_or_else(|| "Claude".to_owned()),
        Speaker::Metadata { label } => label.clone(),
    }
}

/// Generate plain text format (simple "Speaker: message" lines)
fn generate_plain(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let mut output = String::new();
    for turn in project_turns(path, options)? {
        let prefix = subagent_prefix(&turn);
        let speaker = speaker_name(&turn);
        for part in &turn.parts {
            match part {
                Part::Text(text) => match &turn.speaker {
                    Speaker::Metadata { label } => {
                        output.push_str(&format!("{prefix}You: [{label}] {text}\n\n"));
                    }
                    _ => output.push_str(&format!("{prefix}{speaker}: {text}\n\n")),
                },
                Part::ToolResult(content) => {
                    let content = turns::tool_result_text(content.as_ref());
                    output.push_str(&format!("{prefix}Tool Result: {content}\n\n"));
                }
                Part::ToolCall { name, input } => {
                    let formatted = format_tool_call_for_export(name, input);
                    output.push_str(&format!("{prefix}Tool: {formatted}\n\n"));
                }
                Part::Thinking(thinking) => {
                    output.push_str(&format!("{prefix}Thinking: {thinking}\n\n"));
                }
            }
        }
    }
    Ok(output)
}

/// Generate markdown format (with ## headers for speakers)
fn generate_markdown(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let mut output = String::new();
    for turn in project_turns(path, options)? {
        let prefix = subagent_prefix(&turn);
        let speaker = speaker_name(&turn);
        for part in &turn.parts {
            match part {
                Part::Text(text) => match &turn.speaker {
                    Speaker::Metadata { label } => {
                        output.push_str(&format!("## {prefix}You\n\n[{label}] {text}\n\n"));
                    }
                    _ => output.push_str(&format!("## {prefix}{speaker}\n\n{text}\n\n")),
                },
                Part::ToolResult(content) => {
                    let fenced = markdown_code_fence(&turns::tool_result_text(content.as_ref()));
                    output.push_str(&format!("### {prefix}Tool Result\n\n{fenced}\n\n"));
                }
                Part::ToolCall { name, input } => {
                    let fenced = markdown_code_fence(&format_tool_call_for_export(name, input));
                    output.push_str(&format!("### {prefix}Tool: {name}\n\n{fenced}\n\n"));
                }
                Part::Thinking(thinking) => {
                    output.push_str(&format!("### {prefix}Thinking\n\n{thinking}\n\n"));
                }
            }
        }
    }
    Ok(output)
}

/// Total line width for ledger export (including name column and separator)
const LEDGER_WIDTH: usize = 90;

/// Generate ledger-style format: the viewer's lines without styling.
fn generate_ledger(path: &Path, options: ExportOptions) -> std::io::Result<String> {
    let render_options = RenderOptions {
        tool_display: if options.show_tools {
            ToolDisplayMode::Full
        } else {
            ToolDisplayMode::Hidden
        },
        show_thinking: options.show_thinking,
        show_timing: false,
        content_width: LEDGER_WIDTH - NAME_WIDTH - SEPARATOR_WIDTH,
        expanded_tool_outputs: BTreeSet::new(),
    };
    let rendered = render_conversation(path, &render_options)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let mut output = String::new();
    for line in &rendered.lines {
        for (text, _) in &line.spans {
            output.push_str(text);
        }
        output.push('\n');
    }
    Ok(output)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn transport_for_env(entries: &[(&str, &str)]) -> Result<ClipboardTransport, String> {
        clipboard_transport_from_env(|name| {
            entries
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn local_session_uses_system_clipboard() {
        assert_eq!(transport_for_env(&[]), Ok(ClipboardTransport::System));
    }

    #[test]
    fn remote_session_uses_osc52() {
        for name in REMOTE_SESSION_ENV_VARS {
            assert_eq!(
                transport_for_env(&[(name, "present")]),
                Ok(ClipboardTransport::Osc52),
                "{name} should identify a remote session"
            );
        }
    }

    #[test]
    fn clipboard_transport_override_takes_precedence() {
        assert_eq!(
            transport_for_env(&[
                ("SSH_CONNECTION", "client server"),
                (CLIPBOARD_TRANSPORT_ENV, "system"),
            ]),
            Ok(ClipboardTransport::System)
        );
        assert_eq!(
            transport_for_env(&[(CLIPBOARD_TRANSPORT_ENV, "osc52")]),
            Ok(ClipboardTransport::Osc52)
        );
        assert_eq!(
            transport_for_env(&[("SSH_TTY", "/dev/pts/1"), (CLIPBOARD_TRANSPORT_ENV, "auto"),]),
            Ok(ClipboardTransport::Osc52)
        );
    }

    #[test]
    fn invalid_clipboard_transport_override_is_rejected() {
        let error = transport_for_env(&[(CLIPBOARD_TRANSPORT_ENV, "remote")]).unwrap_err();
        assert_eq!(
            error,
            "Invalid CLAUDE_HISTORY_CLIPBOARD: expected auto, system, or osc52"
        );
    }

    #[test]
    fn terminal_clipboard_uses_osc52_clipboard_selection() {
        let mut output = Vec::new();
        copy_via_terminal(&mut output, "hello 🌍").unwrap();

        assert_eq!(output, b"\x1b]52;c;aGVsbG8g8J+MjQ==\x1b\\");
    }

    #[test]
    fn terminal_clipboard_surfaces_write_errors() {
        struct BrokenWriter;

        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("write failed"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        assert_eq!(
            copy_via_terminal(BrokenWriter, "text"),
            Err("Terminal clipboard error: write failed".to_string())
        );
    }

    #[test]
    fn pi_exports_use_pi_assistant_label() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v3-branched.jsonl");
        let options = ExportOptions::default();

        let plain = generate_plain(&path, options).unwrap();
        let markdown = generate_markdown(&path, options).unwrap();
        let ledger = generate_ledger(&path, options).unwrap();

        assert!(plain.contains("Pi: root answer"));
        assert!(markdown.contains("## Pi\n\nroot answer"));
        assert!(ledger.contains("Pi │ root answer"));
        assert!(!plain.contains("Claude: root answer"));
        for metadata in [
            "Branch summary",
            "Compaction",
            "Thinking level",
            "Model",
            "Label",
            "custom state searchable",
        ] {
            assert!(!plain.contains(metadata));
            assert!(!markdown.contains(metadata));
            assert!(!ledger.contains(metadata));
        }
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
