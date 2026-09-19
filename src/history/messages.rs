//! What counts as a message.
//!
//! Every consumer of a transcript — the summary parser, the agent protocol's
//! `mN` handles, semantic evidence ranges — must agree on which records
//! occupy a message ordinal. [`MessageOrdinals`] is the single owner of that
//! rule; walkers feed it records in file order and act on the [`Placement`]
//! it returns. Changing anything here shifts every `mN`/`ma_` reference and
//! requires a history cache schema bump.

use crate::claude::{
    AgentContent, AgentProgressData, AssistantMessage, ContentBlock, LogEntry, UserContent,
    UserMessage, extract_text_from_user,
};
use crate::command_tags::{parse_command_name, parse_command_name_and_args};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// An inclusive 1-based range of message ordinals within one transcript.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageRange {
    pub start: usize,
    pub end: usize,
}

impl MessageRange {
    pub fn single(message: usize) -> Self {
        Self {
            start: message,
            end: message,
        }
    }

    pub fn contains(&self, other: &MessageRange) -> bool {
        self.start <= other.start && self.end >= other.end
    }

    pub fn union(&self, other: &MessageRange) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// Where a record lands in the message sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    /// A new message with this ordinal.
    Message(usize),
    /// A streamed duplicate of an assistant message already placed at this
    /// ordinal; its content supersedes the earlier record.
    Replaces(usize),
    /// A control record, or a message filtered out of the sequence.
    Control,
}

/// Assigns message ordinals to transcript records in file order.
///
/// The rule: a user record counts once it has visible text or tool blocks,
/// unless it is `/clear` wrapper metadata or the leading `Warmup` probe; an
/// assistant record counts only after a real user message and only when it
/// has content, with streamed duplicates (same `message.id`) replacing the
/// original; Pi/OMP metadata counts when marked searchable; subagent progress
/// records count when their message has content.
#[derive(Debug, Default)]
pub struct MessageOrdinals {
    count: usize,
    seen_real_user: bool,
    assistant_ordinals: HashMap<String, usize>,
}

impl MessageOrdinals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Messages placed so far.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Places any record except subagent progress, which the caller parses
    /// first and hands to [`Self::place_subagent`].
    pub fn place(&mut self, entry: &LogEntry) -> Placement {
        match entry {
            LogEntry::User { message, .. } => self.place_user(message),
            LogEntry::Assistant { message, .. } => self.place_assistant(message),
            LogEntry::PiMetadata { searchable, .. } => {
                if *searchable {
                    self.next()
                } else {
                    Placement::Control
                }
            }
            _ => Placement::Control,
        }
    }

    pub fn place_subagent(&mut self, progress: &AgentProgressData) -> Placement {
        if !matches!(progress.message.message_type.as_str(), "user" | "assistant") {
            return Placement::Control;
        }
        let AgentContent::Blocks(blocks) = &progress.message.message.content;
        if blocks_count_as_message(blocks) {
            self.next()
        } else {
            Placement::Control
        }
    }

    fn place_user(&mut self, message: &UserMessage) -> Placement {
        let text = user_visible_text(message);
        if extract_skill_preview(&text).is_none()
            && !text.is_empty()
            && is_clear_metadata_message(&text)
        {
            return Placement::Control;
        }
        if !self.seen_real_user && text.trim() == "Warmup" {
            return Placement::Control;
        }
        let counts = match &message.content {
            UserContent::String(text) => !text.trim().is_empty(),
            UserContent::Blocks(blocks) => blocks_count_as_message(blocks),
        };
        if !counts {
            return Placement::Control;
        }
        self.seen_real_user = true;
        self.next()
    }

    fn place_assistant(&mut self, message: &AssistantMessage) -> Placement {
        if !self.seen_real_user || !blocks_count_as_message(&message.content) {
            return Placement::Control;
        }
        let Some(id) = &message.id else {
            return self.next();
        };
        if let Some(&ordinal) = self.assistant_ordinals.get(id) {
            return Placement::Replaces(ordinal);
        }
        let placement = self.next();
        self.assistant_ordinals.insert(id.clone(), self.count);
        placement
    }

    fn next(&mut self) -> Placement {
        self.count += 1;
        Placement::Message(self.count)
    }
}

/// The text blocks of a user message, borrowed when there is at most one so
/// the cold parse does not allocate per record.
fn user_visible_text(message: &UserMessage) -> Cow<'_, str> {
    match &message.content {
        UserContent::String(text) => Cow::Borrowed(text),
        UserContent::Blocks(blocks) => {
            let mut texts = blocks.iter().filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            });
            match (texts.next(), texts.next()) {
                (None, _) => Cow::Borrowed(""),
                (Some(only), None) => Cow::Borrowed(only),
                (Some(_), Some(_)) => Cow::Owned(extract_text_from_user(message)),
            }
        }
    }
}

/// Content blocks give a message substance when any carries visible text, a
/// tool call or a tool result.
pub fn blocks_count_as_message(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|block| match block {
        ContentBlock::Text { text } => !text.trim().is_empty(),
        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => true,
        ContentBlock::Thinking { thinking, .. } => !thinking.trim().is_empty(),
        ContentBlock::Image { .. } | ContentBlock::Other => false,
    })
}

/// Detects metadata emitted by the /clear command wrapper messages and
/// other system-injected boilerplate that should not appear in previews.
pub fn is_clear_metadata_message(message: &str) -> bool {
    let trimmed = message.trim();

    trimmed.is_empty()
        || trimmed.starts_with(
            "Caveat: The messages below were generated by the user while running local commands.",
        )
        || trimmed.contains("<local-command-caveat>")
        || trimmed.contains("<command-name>/clear</command-name>")
        || trimmed.contains("<command-message>clear</command-message>")
        || (trimmed.contains("<command-name>") && !trimmed.contains("<command-name>/"))
        || trimmed.contains("<local-command-stdout>")
        || trimmed.starts_with("Base directory for this skill:")
}

/// Extract a clean preview from a skill invocation message (e.g. "/consult how to do X?").
/// Returns None if the message is not a skill invocation or is a /clear command.
pub fn extract_skill_preview(message: &str) -> Option<String> {
    let command_name = parse_command_name(message)?;
    if !command_name.starts_with('/') || command_name == "/clear" {
        return None;
    }
    parse_command_name_and_args(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(value: serde_json::Value) -> LogEntry {
        serde_json::from_value(value).expect("valid log entry")
    }

    fn user(text: &str) -> LogEntry {
        entry(json!({"type": "user", "message": {"role": "user", "content": text}}))
    }

    fn user_blocks(blocks: serde_json::Value) -> LogEntry {
        entry(json!({"type": "user", "message": {"role": "user", "content": blocks}}))
    }

    fn assistant(id: Option<&str>, text: &str) -> LogEntry {
        entry(json!({
            "type": "assistant",
            "message": {"role": "assistant", "id": id, "content": [{"type": "text", "text": text}]}
        }))
    }

    fn placements(entries: &[LogEntry]) -> Vec<Placement> {
        let mut ordinals = MessageOrdinals::new();
        entries.iter().map(|entry| ordinals.place(entry)).collect()
    }

    #[test]
    fn user_then_assistant_take_consecutive_ordinals() {
        assert_eq!(
            placements(&[user("hi"), assistant(None, "hello")]),
            vec![Placement::Message(1), Placement::Message(2)]
        );
    }

    #[test]
    fn assistant_before_any_user_is_control() {
        assert_eq!(
            placements(&[assistant(None, "hello"), user("hi")]),
            vec![Placement::Control, Placement::Message(1)]
        );
    }

    #[test]
    fn warmup_probe_and_its_reply_are_control() {
        assert_eq!(
            placements(&[
                user("Warmup"),
                assistant(None, "ready"),
                user("real question"),
                assistant(None, "answer"),
            ]),
            vec![
                Placement::Control,
                Placement::Control,
                Placement::Message(1),
                Placement::Message(2),
            ]
        );
        // Only the leading probe is special.
        assert_eq!(
            placements(&[user("real"), user("Warmup")]),
            vec![Placement::Message(1), Placement::Message(2)]
        );
    }

    #[test]
    fn clear_metadata_is_control_but_skill_invocations_count() {
        assert_eq!(
            placements(&[
                user("<command-name>/clear</command-name>"),
                user("<local-command-stdout>ok</local-command-stdout>"),
                user(
                    "<command-message>consult</command-message><command-name>/consult</command-name>"
                ),
            ]),
            vec![
                Placement::Control,
                Placement::Control,
                Placement::Message(1)
            ]
        );
    }

    #[test]
    fn streamed_assistant_duplicates_replace_their_original() {
        assert_eq!(
            placements(&[
                user("q"),
                assistant(Some("msg_1"), "partial"),
                assistant(Some("msg_1"), "partial complete"),
                assistant(Some("msg_2"), "next"),
            ]),
            vec![
                Placement::Message(1),
                Placement::Message(2),
                Placement::Replaces(2),
                Placement::Message(3),
            ]
        );
    }

    #[test]
    fn empty_and_image_only_messages_are_control() {
        assert_eq!(
            placements(&[
                user("   "),
                user_blocks(json!([{"type": "image", "source": {}}])),
                user_blocks(json!([{"type": "tool_result", "tool_use_id": "t1"}])),
            ]),
            vec![
                Placement::Control,
                Placement::Control,
                Placement::Message(1)
            ]
        );
    }

    #[test]
    fn pi_metadata_counts_when_searchable_even_without_text() {
        let searchable = LogEntry::PiMetadata {
            label: "Hook".to_owned(),
            text: String::new(),
            timestamp: None,
            searchable: true,
            usage: None,
        };
        let hidden = LogEntry::PiMetadata {
            label: "Model".to_owned(),
            text: "claude".to_owned(),
            timestamp: None,
            searchable: false,
            usage: None,
        };
        assert_eq!(
            placements(&[searchable, hidden]),
            vec![Placement::Message(1), Placement::Control]
        );
    }

    #[test]
    fn subagent_progress_counts_by_content() {
        let mut ordinals = MessageOrdinals::new();
        let with_text: AgentProgressData = serde_json::from_value(json!({
            "type": "agent_progress",
            "agentId": "a1",
            "message": {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": "sub"}]}}
        }))
        .unwrap();
        let empty: AgentProgressData = serde_json::from_value(json!({
            "type": "agent_progress",
            "agentId": "a1",
            "message": {"type": "assistant", "message": {"role": "assistant", "content": [{"type": "text", "text": ""}]}}
        }))
        .unwrap();
        assert_eq!(ordinals.place_subagent(&with_text), Placement::Message(1));
        assert_eq!(ordinals.place_subagent(&empty), Placement::Control);
        assert_eq!(ordinals.count(), 1);
    }

    /// The summary parser and the agent transcript are the two walkers that
    /// act on placements; they must see the same sequence for the same file.
    #[test]
    fn parser_and_agent_transcript_agree_on_ordinals() {
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"Warmup"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","id":"warm","content":[{"type":"text","text":"ready"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"first real question"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","id":"a1","content":[{"type":"text","text":"partial"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","id":"a1","content":[{"type":"text","text":"partial complete"},{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","id":"a2","content":[{"type":"text","text":"   "}]}}"#,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"sub","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"subagent says"}]}}}}"#,
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"sub","message":{"type":"user","message":{"role":"user","content":[{"type":"image","source":{}}]}}}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"final answer"}]}}"#,
        ];
        let content = lines.join("\n");

        let conversation = crate::history::parser::process_conversation_reader(
            std::path::PathBuf::from("parity.jsonl"),
            std::io::Cursor::new(content.clone()),
            None,
            None,
        )
        .unwrap()
        .unwrap();
        let transcript = crate::agent::transcript::AgentTranscript::from_reader(
            std::path::PathBuf::from("parity.jsonl"),
            std::io::Cursor::new(content),
        )
        .unwrap();

        let ordinals: Vec<usize> = transcript.messages.iter().map(|m| m.ordinal).collect();
        assert_eq!(ordinals, (1..=5).collect::<Vec<_>>());
        assert_eq!(conversation.message_count, transcript.messages.len());
        // Streamed duplicate replaced m2 in place rather than taking a slot.
        assert_eq!(transcript.messages[1].parts.len(), 2);
        for range in &conversation.semantic_turn_ranges {
            assert!(range.end <= conversation.message_count, "{range:?}");
        }
    }

    #[test]
    fn is_clear_metadata_message_detects_patterns() {
        assert!(is_clear_metadata_message(""));
        assert!(is_clear_metadata_message(
            "Caveat: The messages below were generated by the user while running local commands."
        ));
        assert!(is_clear_metadata_message(
            "<command-name>clear</command-name>"
        ));
        assert!(is_clear_metadata_message(
            "Base directory for this skill: /x"
        ));
        assert!(!is_clear_metadata_message(
            "<command-name>/consult</command-name>"
        ));
        assert!(!is_clear_metadata_message("hello"));
    }

    #[test]
    fn extract_skill_preview_handles_args_and_clear() {
        assert_eq!(
            extract_skill_preview(
                "<command-name>/consult</command-name><command-args>how?</command-args>"
            )
            .as_deref(),
            Some("/consult how?")
        );
        assert_eq!(
            extract_skill_preview("<command-name>/consult</command-name>").as_deref(),
            Some("/consult")
        );
        assert!(extract_skill_preview("<command-name>/clear</command-name>").is_none());
        assert!(extract_skill_preview("plain text").is_none());
    }
}
