//! JSONL conversation file parsing.
//!
//! This module handles parsing Claude conversation JSONL files and extracting
//! conversation metadata like preview text, message counts, and working directory.

use super::{Conversation, ParseError};
use crate::claude::{
    LogEntry, TokenUsage, extract_search_text_from_assistant, extract_search_text_from_user,
    extract_text_from_assistant, extract_text_from_user,
};
use crate::cli::DebugLevel;
use crate::debug;
use crate::error::Result;
use chrono::{DateTime, Local};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};

use std::path::PathBuf;
use std::time::SystemTime;

/// Process a single conversation file and extract all necessary information
pub fn process_conversation_file(
    path: PathBuf,
    show_last: bool,
    modified: Option<SystemTime>,
    debug_level: Option<DebugLevel>,
) -> Result<Option<Conversation>> {
    let file = File::open(&path)?;
    let reader = BufReader::new(file);
    process_conversation_reader(path, reader, show_last, modified, debug_level)
}

/// Process a conversation from any BufRead source (for testability)
pub(crate) fn process_conversation_reader<R: BufRead>(
    path: PathBuf,
    reader: R,
    show_last: bool,
    modified: Option<SystemTime>,
    debug_level: Option<DebugLevel>,
) -> Result<Option<Conversation>> {
    let filename = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("unknown");

    // Collect all lines for context access when logging parse errors
    let lines: Vec<String> = reader.lines().map_while(|l| l.ok()).collect();

    let mut all_parts = Vec::new();
    let mut preview_parts = Vec::new();
    let mut user_messages = Vec::new();
    let mut seen_real_user_message = false;
    let mut skip_next_assistant = false;
    let mut extracted_cwd: Option<PathBuf> = None;
    let mut message_count: usize = 0;
    let mut parse_errors: Vec<ParseError> = Vec::new();
    let mut extracted_summary: Option<String> = None;
    let mut extracted_custom_title: Option<String> = None;
    let mut extracted_model: Option<String> = None;
    // Track token usage per message ID to avoid double-counting streaming entries
    let mut token_usage_by_msg: HashMap<String, TokenUsage> = HashMap::new();
    let mut anonymous_token_count: u64 = 0;
    // Track first and last message timestamps for conversation duration
    let mut first_timestamp: Option<chrono::DateTime<chrono::FixedOffset>> = None;
    let mut last_timestamp: Option<chrono::DateTime<chrono::FixedOffset>> = None;

    for (line_idx, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<LogEntry>(line) {
            Ok(entry) => {
                // Extract text content
                match entry {
                    LogEntry::User {
                        message,
                        cwd,
                        timestamp,
                        ..
                    } => {
                        // Track timestamps for conversation duration
                        if let Some(ref ts_str) = timestamp
                            && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str)
                        {
                            if first_timestamp.is_none() {
                                first_timestamp = Some(ts);
                            }
                            last_timestamp = Some(ts);
                        }

                        // Extract cwd from the first user message that has it
                        if extracted_cwd.is_none()
                            && let Some(cwd_str) = cwd
                        {
                            // Skip claude-mem observer sessions — they contain
                            // automated observation logs, not real conversations.
                            if cwd_str.contains("claude-mem") {
                                return Ok(None);
                            }

                            extracted_cwd = Some(PathBuf::from(cwd_str));
                        }

                        let preview_text = extract_text_from_user(&message);
                        let search_text = extract_search_text_from_user(&message);

                        if preview_text.is_empty() && search_text.is_empty() {
                            continue;
                        }

                        if !preview_text.is_empty() {
                            user_messages.push(preview_text.clone());
                        }

                        // Check for skill invocations first - extract clean preview
                        // (e.g. "/consult how to do X?" from command XML tags)
                        let effective_preview =
                            if let Some(skill_preview) = extract_skill_preview(&preview_text) {
                                skill_preview
                            } else if !preview_text.is_empty()
                                && is_clear_metadata_message(&preview_text)
                            {
                                if !search_text.is_empty() {
                                    all_parts.push(search_text);
                                }
                                continue;
                            } else {
                                preview_text
                            };

                        if !search_text.is_empty() {
                            all_parts.push(search_text);
                        }

                        // Check if this is a warmup message (first user message is "Warmup")
                        let is_warmup =
                            !seen_real_user_message && effective_preview.trim() == "Warmup";
                        if is_warmup {
                            skip_next_assistant = true;
                        } else if !effective_preview.is_empty() {
                            message_count += 1;
                            preview_parts.push(effective_preview);
                            seen_real_user_message = true;
                        }
                    }
                    LogEntry::Assistant {
                        message, timestamp, ..
                    } => {
                        // Track timestamps for conversation duration
                        if let Some(ref ts_str) = timestamp
                            && let Ok(ts) = chrono::DateTime::parse_from_rfc3339(ts_str)
                        {
                            if first_timestamp.is_none() {
                                first_timestamp = Some(ts);
                            }
                            last_timestamp = Some(ts);
                        }

                        // Extract model name from first assistant message that has it
                        if extracted_model.is_none()
                            && let Some(model) = &message.model
                        {
                            extracted_model = Some(model.clone());
                        }

                        // Track token usage by message ID to avoid double-counting
                        // Multiple JSONL entries can exist for the same message (streaming)
                        if let Some(usage) = &message.usage {
                            if let Some(msg_id) = &message.id {
                                // Store/update usage for this message ID (last one wins)
                                token_usage_by_msg.insert(msg_id.clone(), usage.clone());
                            } else {
                                // No message ID - accumulate directly (legacy format)
                                anonymous_token_count += usage.input_tokens
                                    + usage.output_tokens
                                    + usage.cache_creation_input_tokens
                                    + usage.cache_read_input_tokens;
                            }
                        }

                        let preview_text = extract_text_from_assistant(&message);
                        let search_text = extract_search_text_from_assistant(&message);

                        if !search_text.is_empty() {
                            all_parts.push(search_text);
                        }

                        // Skip this assistant message if it follows a warmup user message
                        if skip_next_assistant {
                            skip_next_assistant = false;
                        } else if seen_real_user_message && !preview_text.is_empty() {
                            // Only add assistant messages to preview after we've seen a real user message
                            message_count += 1;
                            preview_parts.push(preview_text);
                        }
                    }
                    LogEntry::Summary { summary } => {
                        // Extract summary from the first summary entry
                        if extracted_summary.is_none() {
                            extracted_summary = Some(summary.clone());
                        }
                    }
                    LogEntry::CustomTitle { custom_title } => {
                        // Take the last custom title (user may rename multiple times)
                        // Empty title clears any previous title
                        let trimmed = custom_title.trim();
                        extracted_custom_title = if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_owned())
                        };
                    }
                    LogEntry::System { .. } => {}
                    _ => {}
                }
            }
            Err(e) => {
                // Capture parse error with surrounding context
                let start = line_idx.saturating_sub(2);
                let context_before: Vec<String> = lines[start..line_idx].to_vec();
                let end = (line_idx + 3).min(lines.len());
                let context_after: Vec<String> = lines[line_idx + 1..end].to_vec();

                parse_errors.push(ParseError {
                    line_number: line_idx + 1, // 1-indexed for display
                    line_content: line.clone(),
                    error_message: e.to_string(),
                    context_before,
                    context_after,
                });

                debug::warn(
                    debug_level,
                    &format!(
                        "Parse error in {} at line {}: {}",
                        filename,
                        line_idx + 1,
                        e
                    ),
                );
            }
        }
    }

    // Check if this is a clear-only conversation or if preview is empty after filtering
    if is_clear_only_conversation(&user_messages) {
        debug::debug(
            debug_level,
            &format!("Filtered {}: clear-only conversation", filename),
        );
        return Ok(None);
    }

    if all_parts.is_empty() || preview_parts.is_empty() {
        debug::debug(
            debug_level,
            &format!(
                "Filtered {}: empty conversation (all_parts={}, preview_parts={})",
                filename,
                all_parts.len(),
                preview_parts.len()
            ),
        );
        return Ok(None);
    }

    // Use file modification time, falling back to current time if unavailable
    let timestamp = modified
        .map(DateTime::<Local>::from)
        .unwrap_or_else(Local::now);

    // Create preview (first or last 3 messages)
    // Skip leading assistant messages by using preview_parts instead of all_parts
    let preview = if show_last {
        preview_parts
            .iter()
            .rev()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ... ")
    } else {
        preview_parts
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" ... ")
    };

    // Create full text for searching (all messages + summary + custom title)
    let mut full_text = all_parts.join(" ");
    if let Some(ref summary) = extracted_summary {
        full_text = format!("{} {}", summary, full_text);
    }
    if let Some(ref custom_title) = extracted_custom_title {
        full_text = format!("{} {}", custom_title, full_text);
    }

    // Normalize whitespace
    let preview = normalize_whitespace(&preview);
    let full_text = normalize_whitespace(&full_text);

    // Sum token usage from deduplicated messages (all token types)
    let total_tokens: u64 = token_usage_by_msg
        .values()
        .map(|u| {
            u.input_tokens
                + u.output_tokens
                + u.cache_creation_input_tokens
                + u.cache_read_input_tokens
        })
        .sum::<u64>()
        + anonymous_token_count;

    // Calculate conversation duration in minutes
    let duration_minutes = match (first_timestamp, last_timestamp) {
        (Some(first), Some(last)) => {
            let duration = last.signed_duration_since(first);
            let minutes = duration.num_minutes();
            if minutes > 0 {
                Some(minutes as u64)
            } else {
                None
            }
        }
        _ => None,
    };

    Ok(Some(Conversation {
        path,
        index: 0,
        timestamp,
        preview,
        full_text,
        project_name: None,
        project_path: None,
        cwd: extracted_cwd,
        message_count,
        parse_errors,
        summary: extracted_summary,
        custom_title: extracted_custom_title,
        model: extracted_model,
        total_tokens,
        duration_minutes,
    }))
}

/// Detects metadata emitted by the /clear command wrapper messages and
/// other system-injected boilerplate that should not appear in previews.
pub(crate) fn is_clear_metadata_message(message: &str) -> bool {
    let trimmed = message.trim();

    trimmed.is_empty()
        || trimmed.starts_with(
            "Caveat: The messages below were generated by the user while running local commands.",
        )
        || trimmed.contains("<local-command-caveat>")
        || trimmed.contains("<command-name>/clear</command-name>")
        || trimmed.contains("<command-message>clear</command-message>")
        || trimmed.contains("<local-command-stdout>")
        || trimmed.starts_with("Base directory for this skill:")
}

/// Extract a clean preview from a skill invocation message (e.g. "/consult how to do X?").
/// Returns None if the message is not a skill invocation or is a /clear command.
pub(crate) fn extract_skill_preview(message: &str) -> Option<String> {
    let trimmed = message.trim();

    let start = trimmed.find("<command-name>")?;
    let end = trimmed.find("</command-name>")?;
    let content_start = start + "<command-name>".len();
    if content_start >= end {
        return None;
    }

    let command_name = &trimmed[content_start..end];
    if command_name == "/clear" {
        return None;
    }

    // Extract command args if present
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

    Some(command_name.to_string())
}

/// Check if a conversation only contains /clear command messages
pub(crate) fn is_clear_only_conversation(user_messages: &[String]) -> bool {
    if user_messages.is_empty() {
        return false;
    }

    let mut saw_caveat = false;
    let mut saw_command = false;
    let mut saw_stdout = false;

    for msg in user_messages {
        let trimmed = msg.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_caveat = trimmed.starts_with(
            "Caveat: The messages below were generated by the user while running local commands.",
        );
        let has_command_tag = trimmed.contains("<command-name>/clear</command-name>");
        let has_stdout_tag = trimmed.contains("<local-command-stdout>");

        if is_caveat {
            saw_caveat = true;
        }
        if has_command_tag {
            saw_command = true;
        }
        if has_stdout_tag {
            saw_stdout = true;
        }

        // Any substantive user message immediately disqualifies this from being clear-only
        if !(is_caveat || has_command_tag || has_stdout_tag) {
            return false;
        }
    }

    saw_caveat && saw_command && saw_stdout
}

/// Normalize whitespace in a string
pub(crate) fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<&str>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Helper to create a user message JSON line
    fn user_msg(text: &str, cwd: Option<&str>) -> String {
        let cwd_json = match cwd {
            Some(c) => format!(r#""cwd": "{}","#, c),
            None => String::new(),
        };
        format!(
            r#"{{"type": "user", "timestamp": "2024-01-01T00:00:00Z", {}  "message": {{"role": "user", "content": "{}"}}}}"#,
            cwd_json, text
        )
    }

    /// Helper to create an assistant message JSON line
    fn assistant_msg(text: &str) -> String {
        format!(
            r#"{{"type": "assistant", "timestamp": "2024-01-01T00:00:00Z", "message": {{"role": "assistant", "content": [{{"type": "text", "text": "{}"}}]}}}}"#,
            text
        )
    }

    /// Helper to create an assistant message with model and usage
    fn assistant_msg_with_usage(
        text: &str,
        model: &str,
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
    ) -> String {
        format!(
            r#"{{"type": "assistant", "timestamp": "2024-01-01T00:00:00Z", "message": {{"role": "assistant", "model": "{}", "usage": {{"input_tokens": {}, "output_tokens": {}, "cache_creation_input_tokens": {}, "cache_read_input_tokens": {}}}, "content": [{{"type": "text", "text": "{}"}}]}}}}"#,
            model, input, output, cache_creation, cache_read, text
        )
    }

    /// Helper to parse JSONL content
    fn parse_jsonl(content: &str) -> Result<Option<Conversation>> {
        let reader = Cursor::new(content);
        process_conversation_reader(
            PathBuf::from("test.jsonl"),
            reader,
            false, // show_last
            None,  // modified
            None,  // debug_level
        )
    }

    // === Warmup message filtering ===

    #[test]
    fn filters_warmup_messages_from_preview() {
        let content = [
            user_msg("Warmup", None),
            assistant_msg("Ready"),
            user_msg("Hello world", None),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();

        // Preview should NOT include the warmup exchange
        assert!(!conv.preview.contains("Warmup"));
        assert!(!conv.preview.contains("Ready"));
        assert!(conv.preview.contains("Hello world"));
        assert!(conv.preview.contains("Hi there"));

        // But full_text SHOULD include warmup content for searching
        assert!(conv.full_text.contains("Warmup"));
        assert!(conv.full_text.contains("Ready"));
    }

    #[test]
    fn warmup_only_conversation_excluded_from_preview_but_preserved() {
        // A conversation with only warmup should still be valid if it has content
        let content = [
            user_msg("Warmup", None),
            assistant_msg("Ready"),
            user_msg("Actual question", None),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(!conv.preview.contains("Warmup"));
        assert!(conv.preview.contains("Actual question"));
    }

    // === Clear command filtering ===

    #[test]
    fn filters_clear_only_conversations() {
        let content = [
            user_msg(
                "Caveat: The messages below were generated by the user while running local commands.",
                None,
            ),
            user_msg("<command-name>/clear</command-name>", None),
            user_msg("<local-command-stdout></local-command-stdout>", None),
        ]
        .join("\n");

        let result = parse_jsonl(&content).unwrap();
        assert!(
            result.is_none(),
            "Clear-only conversation should be filtered"
        );
    }

    #[test]
    fn preserves_clear_command_in_mixed_conversation() {
        let content = [
            user_msg("Hello", None),
            assistant_msg("Hi"),
            user_msg(
                "Caveat: The messages below were generated by the user while running local commands.",
                None,
            ),
            user_msg("<command-name>/clear</command-name>", None),
            user_msg("Another question", None),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        // The conversation should be preserved since it has real content
        assert!(conv.preview.contains("Hello"));
        assert!(conv.preview.contains("Another question"));
    }

    // === CWD extraction ===

    #[test]
    fn extracts_cwd_from_first_user_message() {
        let content = [
            user_msg("Hello", Some("/home/user/project")),
            assistant_msg("Hi"),
            user_msg("More", Some("/other/path")),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.cwd,
            Some(PathBuf::from("/home/user/project")),
            "Should extract cwd from first user message"
        );
    }

    #[test]
    fn handles_missing_cwd() {
        let content = [user_msg("Hello", None), assistant_msg("Hi")].join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(conv.cwd.is_none());
    }

    // === Empty conversation handling ===

    #[test]
    fn handles_empty_conversation() {
        let content = "";
        let result = parse_jsonl(content).unwrap();
        assert!(result.is_none(), "Empty conversation should return None");
    }

    #[test]
    fn handles_only_whitespace() {
        let content = "\n\n   \n\n";
        let result = parse_jsonl(content).unwrap();
        assert!(result.is_none());
    }

    // === Message counting ===

    #[test]
    fn counts_messages_correctly() {
        let content = [
            user_msg("First", None),
            assistant_msg("Response 1"),
            user_msg("Second", None),
            assistant_msg("Response 2"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(conv.message_count, 4, "Should count 4 messages");
    }

    #[test]
    fn excludes_warmup_from_message_count() {
        let content = [
            user_msg("Warmup", None),
            assistant_msg("Ready"),
            user_msg("Real question", None),
            assistant_msg("Real answer"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        // Warmup and Ready should not be counted
        assert_eq!(
            conv.message_count, 2,
            "Should count 2 messages (excluding warmup)"
        );
    }

    // === Parse error handling ===

    #[test]
    fn captures_parse_errors_with_context() {
        let content = [
            user_msg("Line 1", None),
            "invalid json here".to_string(),
            user_msg("Line 3", None),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(conv.parse_errors.len(), 1);

        let error = &conv.parse_errors[0];
        assert_eq!(error.line_number, 2);
        assert!(error.line_content.contains("invalid json"));
        assert!(!error.error_message.is_empty());
        // Context before should have line 1
        assert_eq!(error.context_before.len(), 1);
        // Context after should have line 3
        assert_eq!(error.context_after.len(), 1);
    }

    // === Preview order ===

    #[test]
    fn show_last_reverses_preview_order() {
        let content = [
            user_msg("First", None),
            assistant_msg("Response 1"),
            user_msg("Second", None),
            assistant_msg("Response 2"),
            user_msg("Third", None),
            assistant_msg("Response 3"),
        ]
        .join("\n");

        // Parse with show_last = false
        let conv_first = {
            let reader = Cursor::new(&content);
            process_conversation_reader(PathBuf::from("test.jsonl"), reader, false, None, None)
                .unwrap()
                .unwrap()
        };

        // Parse with show_last = true
        let conv_last = {
            let reader = Cursor::new(&content);
            process_conversation_reader(PathBuf::from("test.jsonl"), reader, true, None, None)
                .unwrap()
                .unwrap()
        };

        // show_last=false should start with "First"
        assert!(
            conv_first.preview.starts_with("First"),
            "Preview should start with First: {}",
            conv_first.preview
        );

        // show_last=true should start with the last message (Response 3)
        assert!(
            conv_last.preview.starts_with("Response 3"),
            "Preview should start with Response 3: {}",
            conv_last.preview
        );
    }

    // === Helper function tests ===

    #[test]
    fn is_clear_metadata_message_detects_patterns() {
        assert!(is_clear_metadata_message(""));
        assert!(is_clear_metadata_message("   "));
        assert!(is_clear_metadata_message(
            "Caveat: The messages below were generated by the user while running local commands."
        ));
        assert!(is_clear_metadata_message(
            "<local-command-caveat>something</local-command-caveat>"
        ));
        assert!(is_clear_metadata_message(
            "<command-name>/clear</command-name>"
        ));
        assert!(is_clear_metadata_message(
            "<command-message>clear</command-message>"
        ));
        assert!(is_clear_metadata_message(
            "<local-command-stdout>output</local-command-stdout>"
        ));
        // <command-args> alone should NOT match - it appears in all skill invocations
        assert!(!is_clear_metadata_message(
            "<command-args>foo</command-args>"
        ));

        assert!(is_clear_metadata_message(
            "Base directory for this skill: /Users/raine/.claude/skills/consult\n\nConsult an external LLM."
        ));

        // Should NOT match normal messages
        assert!(!is_clear_metadata_message("Hello world"));
        assert!(!is_clear_metadata_message("What is the meaning of life?"));

        // Skill invocation with command-name should NOT be filtered as clear metadata
        assert!(!is_clear_metadata_message(
            "<command-message>consult</command-message>\n<command-name>/consult</command-name>\n<command-args>how to do X?</command-args>"
        ));
    }

    #[test]
    fn extract_skill_preview_extracts_command_with_args() {
        assert_eq!(
            extract_skill_preview(
                "<command-message>consult</command-message>\n<command-name>/consult</command-name>\n<command-args>how to do X?</command-args>"
            ),
            Some("/consult how to do X?".to_string())
        );
    }

    #[test]
    fn extract_skill_preview_extracts_command_without_args() {
        assert_eq!(
            extract_skill_preview("<command-name>/help</command-name>"),
            Some("/help".to_string())
        );
    }

    #[test]
    fn extract_skill_preview_skips_clear() {
        assert_eq!(
            extract_skill_preview(
                "<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"
            ),
            None
        );
    }

    #[test]
    fn extract_skill_preview_returns_none_for_normal_text() {
        assert_eq!(extract_skill_preview("Hello world"), None);
    }

    #[test]
    fn skill_invocation_conversation_not_filtered() {
        // A conversation that starts with /clear then has a skill invocation
        // should NOT be filtered out
        let content = [
            user_msg(
                "Caveat: The messages below were generated by the user while running local commands.",
                None,
            ),
            user_msg(
                "<command-name>/clear</command-name> <command-message>clear</command-message> <command-args></command-args>",
                None,
            ),
            user_msg("<local-command-stdout></local-command-stdout>", None),
            user_msg(
                "<command-message>consult</command-message> <command-name>/consult</command-name> <command-args>how to implement sidebar?</command-args>",
                None,
            ),
            assistant_msg("Here's how to implement it..."),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap();
        assert!(
            conv.is_some(),
            "Conversation with skill invocation should not be filtered"
        );
        let conv = conv.unwrap();
        assert!(
            conv.preview.contains("/consult"),
            "Preview should contain the skill command: {}",
            conv.preview
        );
    }

    #[test]
    fn normalize_whitespace_collapses_runs() {
        assert_eq!(normalize_whitespace("hello  world"), "hello world");
        assert_eq!(normalize_whitespace("  hello   world  "), "hello world");
        assert_eq!(normalize_whitespace("a\n\n\nb"), "a b");
        assert_eq!(
            normalize_whitespace("\t\thello\t\tworld\t\t"),
            "hello world"
        );
        assert_eq!(normalize_whitespace(""), "");
    }

    #[test]
    fn is_clear_only_conversation_requires_all_three_markers() {
        // Empty is not clear-only
        assert!(!is_clear_only_conversation(&[]));

        // Just caveat is not enough
        assert!(!is_clear_only_conversation(&[
            "Caveat: The messages below were generated by the user while running local commands."
                .to_string()
        ]));

        // Caveat + command but no stdout
        assert!(!is_clear_only_conversation(&[
            "Caveat: The messages below were generated by the user while running local commands."
                .to_string(),
            "<command-name>/clear</command-name>".to_string(),
        ]));

        // All three = clear-only
        assert!(is_clear_only_conversation(&[
            "Caveat: The messages below were generated by the user while running local commands."
                .to_string(),
            "<command-name>/clear</command-name>".to_string(),
            "<local-command-stdout></local-command-stdout>".to_string(),
        ]));

        // Any substantive message disqualifies
        assert!(!is_clear_only_conversation(&[
            "Caveat: The messages below were generated by the user while running local commands."
                .to_string(),
            "<command-name>/clear</command-name>".to_string(),
            "<local-command-stdout></local-command-stdout>".to_string(),
            "Hello world".to_string(),
        ]));
    }

    // === Summary extraction ===

    #[test]
    fn extracts_summary_from_jsonl() {
        let content = [
            r#"{"type": "summary", "summary": "Test conversation summary", "leafUuid": "abc123"}"#
                .to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.summary,
            Some("Test conversation summary".to_string()),
            "Should extract summary from summary entry"
        );
    }

    #[test]
    fn summary_included_in_full_text() {
        let content = [
            r#"{"type": "summary", "summary": "Important topic discussion", "leafUuid": "abc123"}"#
                .to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            conv.full_text.contains("Important topic discussion"),
            "Summary should be included in full_text for searching"
        );
    }

    #[test]
    fn handles_conversation_without_summary() {
        let content = [user_msg("Hello", None), assistant_msg("Hi there")].join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(conv.summary.is_none(), "Should have no summary");
    }

    #[test]
    fn takes_first_summary_if_multiple() {
        let content = [
            r#"{"type": "summary", "summary": "First summary", "leafUuid": "abc"}"#.to_string(),
            user_msg("Hello", None),
            r#"{"type": "summary", "summary": "Second summary", "leafUuid": "def"}"#.to_string(),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.summary,
            Some("First summary".to_string()),
            "Should keep first summary encountered"
        );
    }

    // === Model and token extraction ===

    #[test]
    fn extracts_model_from_assistant_message() {
        let content = [
            user_msg("Hello", None),
            assistant_msg_with_usage("Hi there", "claude-opus-4-5-20251101", 100, 50, 0, 0),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.model,
            Some("claude-opus-4-5-20251101".to_string()),
            "Should extract model from assistant message"
        );
    }

    #[test]
    fn accumulates_tokens_across_messages() {
        let content = [
            user_msg("Hello", None),
            assistant_msg_with_usage("Hi", "claude-opus-4-5-20251101", 100, 50, 10, 5),
            user_msg("How are you?", None),
            assistant_msg_with_usage("Good!", "claude-opus-4-5-20251101", 200, 100, 20, 10),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        // Total = (100+50+10+5) + (200+100+20+10) = 495 (all token types)
        assert_eq!(
            conv.total_tokens, 495,
            "Should accumulate all token types from all assistant messages"
        );
    }

    #[test]
    fn takes_first_model_if_multiple() {
        let content = [
            user_msg("Hello", None),
            assistant_msg_with_usage("Hi", "claude-opus-4-5-20251101", 100, 50, 0, 0),
            user_msg("Follow up", None),
            assistant_msg_with_usage("Response", "claude-sonnet-4-20250514", 200, 100, 0, 0),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.model,
            Some("claude-opus-4-5-20251101".to_string()),
            "Should keep first model encountered"
        );
    }

    #[test]
    fn handles_missing_model_and_usage() {
        let content = [user_msg("Hello", None), assistant_msg("Hi there")].join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(conv.model.is_none(), "Should have no model");
        assert_eq!(conv.total_tokens, 0, "Should have zero tokens");
    }

    // === Custom title extraction ===

    #[test]
    fn extracts_custom_title_from_jsonl() {
        let content = [
            r#"{"type": "custom-title", "customTitle": "my session", "sessionId": "abc123"}"#
                .to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.custom_title,
            Some("my session".to_string()),
            "Should extract custom title"
        );
    }

    #[test]
    fn takes_last_custom_title_if_multiple() {
        let content = [
            r#"{"type": "custom-title", "customTitle": "first name", "sessionId": "abc"}"#
                .to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
            r#"{"type": "custom-title", "customTitle": "renamed", "sessionId": "abc"}"#.to_string(),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert_eq!(
            conv.custom_title,
            Some("renamed".to_string()),
            "Should keep last custom title (user renamed)"
        );
    }

    #[test]
    fn custom_title_included_in_full_text() {
        let content = [
            r#"{"type": "custom-title", "customTitle": "unique-session-name", "sessionId": "abc"}"#
                .to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            conv.full_text.contains("unique-session-name"),
            "Custom title should be included in full_text for searching"
        );
    }

    #[test]
    fn ignores_empty_custom_title() {
        let content = [
            r#"{"type": "custom-title", "customTitle": "", "sessionId": "abc"}"#.to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            conv.custom_title.is_none(),
            "Empty custom title should be treated as None"
        );
    }

    #[test]
    fn empty_custom_title_clears_previous() {
        let content = [
            r#"{"type": "custom-title", "customTitle": "initial name", "sessionId": "abc"}"#
                .to_string(),
            user_msg("Hello", None),
            assistant_msg("Hi there"),
            r#"{"type": "custom-title", "customTitle": "", "sessionId": "abc"}"#.to_string(),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            conv.custom_title.is_none(),
            "Empty custom title should clear previous title"
        );
    }

    #[test]
    fn handles_conversation_without_custom_title() {
        let content = [user_msg("Hello", None), assistant_msg("Hi there")].join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(conv.custom_title.is_none(), "Should have no custom title");
    }

    // === ToolResult search indexing ===

    /// Helper to create a user message with tool result (string content)
    fn user_msg_with_tool_result(text: &str, tool_output: &str) -> String {
        format!(
            r#"{{"type": "user", "timestamp": "2024-01-01T00:00:00Z", "message": {{"role": "user", "content": [{{"type": "text", "text": "{}"}}, {{"type": "tool_result", "tool_use_id": "toolu_123", "content": "{}"}}]}}}}"#,
            text, tool_output
        )
    }

    /// Helper to create a user message with tool result (array-of-blocks content)
    fn user_msg_with_tool_result_blocks(text: &str, tool_output: &str) -> String {
        format!(
            r#"{{"type": "user", "timestamp": "2024-01-01T00:00:00Z", "message": {{"role": "user", "content": [{{"type": "text", "text": "{}"}}, {{"type": "tool_result", "tool_use_id": "toolu_123", "content": [{{"type": "text", "text": "{}"}}]}}]}}}}"#,
            text, tool_output
        )
    }

    #[test]
    fn tool_result_string_included_in_full_text() {
        let content = [
            user_msg_with_tool_result("run this", "command output here"),
            assistant_msg("Done"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            conv.full_text.contains("command output here"),
            "Tool result string should be in full_text for search: {}",
            conv.full_text
        );
    }

    #[test]
    fn tool_result_array_included_in_full_text() {
        let content = [
            user_msg_with_tool_result_blocks("check file", "file contents xyz"),
            assistant_msg("Got it"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            conv.full_text.contains("file contents xyz"),
            "Tool result array blocks should be in full_text: {}",
            conv.full_text
        );
    }

    #[test]
    fn tool_result_not_in_preview() {
        let content = [
            user_msg_with_tool_result(
                "run this",
                "verbose tool output should not appear in preview",
            ),
            assistant_msg("Done"),
        ]
        .join("\n");

        let conv = parse_jsonl(&content).unwrap().unwrap();
        assert!(
            !conv.preview.contains("verbose tool output"),
            "Tool result should NOT be in preview: {}",
            conv.preview
        );
        assert!(
            conv.preview.contains("run this"),
            "Text blocks should still be in preview: {}",
            conv.preview
        );
    }

    #[test]
    fn clear_conversation_still_filtered_with_tool_results() {
        let content = [
            user_msg(
                "Caveat: The messages below were generated by the user while running local commands.",
                None,
            ),
            user_msg("<command-name>/clear</command-name>", None),
            user_msg("<local-command-stdout></local-command-stdout>", None),
        ]
        .join("\n");

        let result = parse_jsonl(&content).unwrap();
        assert!(
            result.is_none(),
            "Clear-only conversation should still be filtered"
        );
    }
}
