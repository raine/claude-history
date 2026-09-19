//! Shared message ordinal assignment for history and agent transcript readers.
//!
//! The history parser and agent transcript reader use the same placement rules
//! so semantic evidence ranges resolve to the same `mN` messages as reads.

use crate::claude::{
    AgentContent, AgentProgressData, AssistantMessage, ContentBlock, LogEntry, UserContent,
    UserMessage,
};
use std::collections::HashMap;

/// Where a transcript record lands in the message sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Placement {
    /// A new message with this ordinal.
    Message(usize),
    /// A streamed assistant record replacing an earlier message.
    Replaces(usize),
    /// A control record or filtered message.
    Control,
}

/// Assigns message ordinals to transcript records in file order.
#[derive(Debug, Default)]
pub(crate) struct MessageOrdinals {
    count: usize,
    seen_real_user: bool,
    assistant_ordinals: HashMap<String, usize>,
}

impl MessageOrdinals {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    /// Places user, assistant, and Pi/OMP metadata records. Progress records
    /// are parsed by each caller and placed with [`Self::place_subagent`].
    pub(crate) fn place(&mut self, entry: &LogEntry) -> Placement {
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

    pub(crate) fn place_subagent(&mut self, progress: &AgentProgressData) -> Placement {
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
        // This mirrors blocks_to_parts in agent/transcript.rs: clear and
        // warmup checks see the first retained text part, not joined raw text.
        let first_text = first_retained_user_text(message);
        if first_text.as_deref().is_some_and(is_clear_metadata_message) {
            return Placement::Control;
        }
        if !self.seen_real_user
            && first_text
                .as_deref()
                .is_some_and(|text| text.trim() == "Warmup")
        {
            return Placement::Control;
        }

        let counts = match &message.content {
            UserContent::String(_) => first_text.is_some(),
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

/// Content blocks give a message substance when any carries visible text, a
/// tool call, a tool result, or non-empty thinking content.
pub(crate) fn blocks_count_as_message(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|block| match block {
        ContentBlock::Text { text } => !text.trim().is_empty(),
        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => true,
        ContentBlock::Thinking { thinking, .. } => !thinking.trim().is_empty(),
        ContentBlock::Image { .. } | ContentBlock::Other => false,
    })
}

pub(crate) fn retained_user_text(text: String) -> Option<String> {
    let text = extract_skill_preview(&text).unwrap_or(text);
    (!text.trim().is_empty()).then_some(text)
}

fn first_retained_user_text(message: &UserMessage) -> Option<String> {
    match &message.content {
        UserContent::String(text) => retained_user_text(text.clone()),
        UserContent::Blocks(blocks) => blocks.iter().find_map(|block| {
            let ContentBlock::Text { text } = block else {
                return None;
            };
            retained_user_text(text.clone())
        }),
    }
}

/// Detects metadata emitted by the /clear command wrapper messages and other
/// system-injected boilerplate that should not appear in previews.
pub(crate) fn is_clear_metadata_message(message: &str) -> bool {
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

/// Extract a clean preview from a skill invocation message.
pub(crate) fn extract_skill_preview(message: &str) -> Option<String> {
    let trimmed = message.trim();

    let start = trimmed.find("<command-name>")?;
    let end = trimmed.find("</command-name>")?;
    let content_start = start + "<command-name>".len();
    if content_start >= end {
        return None;
    }

    let command_name = &trimmed[content_start..end];
    if !command_name.starts_with('/') || command_name == "/clear" {
        return None;
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(value: serde_json::Value) -> LogEntry {
        serde_json::from_value(value).expect("valid log entry")
    }

    fn user(text: &str) -> LogEntry {
        entry(json!({"type":"user","message":{"role":"user","content":text}}))
    }

    fn user_blocks(blocks: serde_json::Value) -> LogEntry {
        entry(json!({"type":"user","message":{"role":"user","content":blocks}}))
    }

    fn assistant(id: Option<&str>, text: &str) -> LogEntry {
        entry(json!({
            "type":"assistant",
            "message":{"role":"assistant","id":id,"content":[{"type":"text","text":text}]}
        }))
    }

    fn placements(entries: &[LogEntry]) -> Vec<Placement> {
        let mut ordinals = MessageOrdinals::new();
        entries.iter().map(|entry| ordinals.place(entry)).collect()
    }

    #[test]
    fn user_and_assistant_take_consecutive_ordinals() {
        assert_eq!(
            placements(&[user("hi"), assistant(None, "hello")]),
            vec![Placement::Message(1), Placement::Message(2)]
        );
    }

    #[test]
    fn leading_assistant_and_warmup_reply_are_control() {
        assert_eq!(
            placements(&[
                assistant(None, "early"),
                user("Warmup"),
                assistant(None, "ready"),
                user("question"),
                assistant(None, "answer"),
            ]),
            vec![
                Placement::Control,
                Placement::Control,
                Placement::Control,
                Placement::Message(1),
                Placement::Message(2),
            ]
        );
    }

    #[test]
    fn warmup_after_real_user_counts() {
        assert_eq!(
            placements(&[user("real"), user("Warmup")]),
            vec![Placement::Message(1), Placement::Message(2)]
        );
    }

    #[test]
    fn clear_metadata_and_skill_invocations_differ() {
        assert_eq!(
            placements(&[
                user("<command-name>/clear</command-name>"),
                user("<local-command-stdout>ok</local-command-stdout>"),
                user("<command-name>/consult</command-name><command-args>topic</command-args>"),
            ]),
            vec![
                Placement::Control,
                Placement::Control,
                Placement::Message(1),
            ]
        );
    }

    #[test]
    fn streamed_assistant_duplicates_replace_their_ordinal() {
        assert_eq!(
            placements(&[
                user("q"),
                assistant(Some("msg_1"), "partial"),
                assistant(Some("msg_1"), "complete"),
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
    fn empty_and_image_only_users_are_control_but_tool_results_count() {
        assert_eq!(
            placements(&[
                user("   "),
                user_blocks(json!([{"type":"image","source":{}}])),
                user_blocks(json!([{"type":"tool_result","tool_use_id":"t1"}])),
            ]),
            vec![
                Placement::Control,
                Placement::Control,
                Placement::Message(1),
            ]
        );
    }

    #[test]
    fn searchable_empty_metadata_counts_without_opening_assistant_gate() {
        let searchable = entry(json!({
            "type":"pi-metadata","label":"Hook","text":"","searchable":true
        }));
        let early_assistant = assistant(None, "early");
        let user = user("question");
        assert_eq!(
            placements(&[searchable, early_assistant, user, assistant(None, "answer")]),
            vec![
                Placement::Message(1),
                Placement::Control,
                Placement::Message(2),
                Placement::Message(3),
            ]
        );
    }

    #[test]
    fn mixed_blank_text_and_tool_result_uses_first_retained_text() {
        let mixed = user_blocks(json!([
            {"type":"text","text":"   "},
            {"type":"tool_result","tool_use_id":"t1"}
        ]));
        assert_eq!(
            placements(&[user("q"), mixed]),
            vec![Placement::Message(1), Placement::Message(2),]
        );
    }

    #[test]
    fn progress_uses_separate_placement() {
        let progress: AgentProgressData = serde_json::from_value(json!({
            "type":"agent_progress","agentId":"a1","message":{
                "type":"assistant","message":{"role":"assistant","content":[
                    {"type":"text","text":"subagent"}
                ]}
            }
        }))
        .unwrap();
        let mut ordinals = MessageOrdinals::new();
        assert_eq!(ordinals.place_subagent(&progress), Placement::Message(1));
        assert_eq!(ordinals.count(), 1);
    }
}
