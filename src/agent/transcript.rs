use crate::claude::{
    AgentContent, AgentMessage as ProgressMessage, AgentProgressData, AssistantMessage,
    ContentBlock, LogEntry, UserContent, UserMessage, parse_agent_progress,
};
use crate::error::Result;
use crate::history::{extract_skill_preview, is_clear_metadata_message};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct AgentTranscript {
    pub path: PathBuf,
    pub messages: Vec<AgentMessage>,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub struct AgentMessage {
    pub ordinal: usize,
    pub role: AgentMessageRole,
    pub timestamp: Option<String>,
    pub jsonl_line: usize,
    pub assistant_message_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub parts: Vec<AgentMessagePart>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub enum AgentMessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub enum AgentMessagePart {
    Text {
        text: String,
        source: AgentPartSource,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        source: AgentPartSource,
    },
    ToolResult {
        tool_use_id: String,
        content: Option<serde_json::Value>,
        source: AgentPartSource,
    },
    Thinking {
        thinking: String,
        source: AgentPartSource,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub struct AgentPartSource {
    pub role: AgentMessageRole,
    pub timestamp: Option<String>,
    pub jsonl_line: usize,
    pub part_index: usize,
    pub assistant_message_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub tool_name: Option<String>,
}

impl AgentTranscript {
    #[allow(dead_code)]
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        Self::from_reader(path.to_path_buf(), BufReader::new(file))
    }

    pub(crate) fn from_reader(path: PathBuf, reader: impl BufRead) -> Result<Self> {
        let mut messages = Vec::new();
        let mut assistant_id_ordinals = HashMap::new();
        let mut seen_real_user_message = false;
        for (line_index, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let jsonl_line = line_index + 1;
            let entry = serde_json::from_str::<LogEntry>(&line)?;
            match entry {
                LogEntry::User {
                    message,
                    timestamp,
                    parent_tool_use_id,
                    ..
                } => {
                    let Some(agent_message) = user_message_to_agent(
                        message,
                        timestamp,
                        jsonl_line,
                        parent_tool_use_id,
                        messages.len() + 1,
                    ) else {
                        continue;
                    };

                    let effective_text = first_user_text(&agent_message);
                    if effective_text
                        .as_deref()
                        .is_some_and(is_clear_metadata_message)
                    {
                        continue;
                    }

                    if !seen_real_user_message
                        && effective_text
                            .as_deref()
                            .is_some_and(|text| text.trim() == "Warmup")
                    {
                        continue;
                    }

                    seen_real_user_message = true;
                    messages.push(agent_message);
                }
                LogEntry::Assistant {
                    message,
                    timestamp,
                    parent_tool_use_id,
                    ..
                } => {
                    if !seen_real_user_message {
                        continue;
                    }
                    let message_id = message.id.clone();
                    let ordinal = message_id
                        .as_ref()
                        .and_then(|id| assistant_id_ordinals.get(id).copied())
                        .unwrap_or(messages.len() + 1);
                    let Some(agent_message) = assistant_message_to_agent(
                        message,
                        timestamp,
                        jsonl_line,
                        parent_tool_use_id,
                        ordinal,
                    ) else {
                        continue;
                    };
                    if let Some(id) = message_id {
                        if let Some(existing_ordinal) = assistant_id_ordinals.insert(id, ordinal) {
                            if let Some(existing) = messages
                                .iter_mut()
                                .find(|message| message.ordinal == existing_ordinal)
                            {
                                *existing = agent_message;
                            }
                        } else {
                            messages.push(agent_message);
                        }
                    } else {
                        messages.push(agent_message);
                    }
                }
                LogEntry::Progress { data, .. } => {
                    if let Some(progress) = parse_agent_progress(&data)
                        && let Some(agent_message) =
                            progress_message_to_agent(progress, jsonl_line, messages.len() + 1)
                    {
                        messages.push(agent_message);
                    }
                }
                LogEntry::Summary { .. }
                | LogEntry::FileHistorySnapshot { .. }
                | LogEntry::System { .. }
                | LogEntry::CustomTitle { .. }
                | LogEntry::AiTitle { .. }
                | LogEntry::AgentName { .. }
                | LogEntry::PermissionMode { .. }
                | LogEntry::Unknown => {}
            }
        }

        for (index, message) in messages.iter_mut().enumerate() {
            message.ordinal = index + 1;
        }

        Ok(Self { path, messages })
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

fn user_message_to_agent(
    message: UserMessage,
    timestamp: Option<String>,
    jsonl_line: usize,
    parent_tool_use_id: Option<String>,
    ordinal: usize,
) -> Option<AgentMessage> {
    let parts = match message.content {
        UserContent::String(text) => {
            let text = extract_skill_preview(&text).unwrap_or(text);
            if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![AgentMessagePart::Text {
                    text,
                    source: source(
                        AgentMessageRole::User,
                        timestamp.clone(),
                        jsonl_line,
                        0,
                        None,
                        parent_tool_use_id.clone(),
                        None,
                    ),
                }]
            }
        }
        UserContent::Blocks(blocks) => blocks_to_parts(
            AgentMessageRole::User,
            blocks,
            timestamp.clone(),
            jsonl_line,
            None,
            parent_tool_use_id.clone(),
        ),
    };
    non_empty_message(AgentMessage {
        ordinal,
        role: AgentMessageRole::User,
        timestamp,
        jsonl_line,
        assistant_message_id: None,
        parent_tool_use_id,
        parts,
    })
}

fn assistant_message_to_agent(
    message: AssistantMessage,
    timestamp: Option<String>,
    jsonl_line: usize,
    parent_tool_use_id: Option<String>,
    ordinal: usize,
) -> Option<AgentMessage> {
    let assistant_message_id = message.id;
    let parts = blocks_to_parts(
        AgentMessageRole::Assistant,
        message.content,
        timestamp.clone(),
        jsonl_line,
        assistant_message_id.clone(),
        parent_tool_use_id.clone(),
    );
    non_empty_message(AgentMessage {
        ordinal,
        role: AgentMessageRole::Assistant,
        timestamp,
        jsonl_line,
        assistant_message_id,
        parent_tool_use_id,
        parts,
    })
}

fn progress_message_to_agent(
    progress: AgentProgressData,
    jsonl_line: usize,
    ordinal: usize,
) -> Option<AgentMessage> {
    let role = match progress.message.message_type.as_str() {
        "user" => AgentMessageRole::User,
        "assistant" => AgentMessageRole::Assistant,
        _ => return None,
    };
    let ProgressMessage { message, .. } = progress.message;
    let AgentContent::Blocks(blocks) = message.content;
    let parent_tool_use_id = Some(progress.agent_id);
    let parts = blocks_to_parts(
        role,
        blocks,
        None,
        jsonl_line,
        None,
        parent_tool_use_id.clone(),
    );
    non_empty_message(AgentMessage {
        ordinal,
        role,
        timestamp: None,
        jsonl_line,
        assistant_message_id: None,
        parent_tool_use_id,
        parts,
    })
}

fn blocks_to_parts(
    role: AgentMessageRole,
    blocks: Vec<ContentBlock>,
    timestamp: Option<String>,
    jsonl_line: usize,
    assistant_message_id: Option<String>,
    parent_tool_use_id: Option<String>,
) -> Vec<AgentMessagePart> {
    blocks
        .into_iter()
        .enumerate()
        .filter_map(|(part_index, block)| match block {
            ContentBlock::Text { text } => {
                let text = if role == AgentMessageRole::User {
                    extract_skill_preview(&text).unwrap_or(text)
                } else {
                    text
                };
                (!text.trim().is_empty()).then(|| AgentMessagePart::Text {
                    text,
                    source: source(
                        role,
                        timestamp.clone(),
                        jsonl_line,
                        part_index,
                        assistant_message_id.clone(),
                        parent_tool_use_id.clone(),
                        None,
                    ),
                })
            }
            ContentBlock::ToolUse { id, name, input } => Some(AgentMessagePart::ToolUse {
                id,
                name: name.clone(),
                input,
                source: source(
                    role,
                    timestamp.clone(),
                    jsonl_line,
                    part_index,
                    assistant_message_id.clone(),
                    parent_tool_use_id.clone(),
                    Some(name),
                ),
            }),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => Some(AgentMessagePart::ToolResult {
                tool_use_id,
                content,
                source: source(
                    role,
                    timestamp.clone(),
                    jsonl_line,
                    part_index,
                    assistant_message_id.clone(),
                    parent_tool_use_id.clone(),
                    None,
                ),
            }),
            ContentBlock::Thinking { thinking, .. } => {
                (!thinking.trim().is_empty()).then(|| AgentMessagePart::Thinking {
                    thinking,
                    source: source(
                        role,
                        timestamp.clone(),
                        jsonl_line,
                        part_index,
                        assistant_message_id.clone(),
                        parent_tool_use_id.clone(),
                        None,
                    ),
                })
            }
            ContentBlock::Image { .. } | ContentBlock::Other => None,
        })
        .collect()
}

pub(crate) fn content_blocks_count_as_agent_message(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|block| match block {
        ContentBlock::Text { text } => !text.trim().is_empty(),
        ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => true,
        ContentBlock::Thinking { thinking, .. } => !thinking.trim().is_empty(),
        ContentBlock::Image { .. } | ContentBlock::Other => false,
    })
}

pub(crate) const MAX_AGENT_SEGMENT_CHARS: usize = 16 * 1024;

pub(crate) fn agent_search_text_from_blocks(
    role: AgentMessageRole,
    blocks: &[ContentBlock],
) -> String {
    let mut acc = BoundedHeadTail::new(MAX_AGENT_SEGMENT_CHARS * blocks.len().max(1));
    for block in blocks {
        if let Some(text) = agent_search_text_from_block(role, block) {
            acc.push_separator(' ');
            acc.push_str(&text);
        }
    }
    acc.finish()
}

fn agent_search_text_from_block(role: AgentMessageRole, block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text { text } => {
            if role == AgentMessageRole::User
                && let Some(preview) = extract_skill_preview(text)
            {
                return non_empty_text(&truncate_chars(&preview, MAX_AGENT_SEGMENT_CHARS));
            }
            non_empty_text(&truncate_chars(text, MAX_AGENT_SEGMENT_CHARS))
        }
        ContentBlock::ToolUse { name, input, .. } => {
            non_empty_text(&format_tool_summary(name, input, MAX_AGENT_SEGMENT_CHARS))
        }
        ContentBlock::ToolResult { content, .. } => {
            content.as_ref().and_then(bounded_tool_result_text)
        }
        ContentBlock::Thinking { thinking, .. } => {
            non_empty_text(&truncate_chars(thinking, MAX_AGENT_SEGMENT_CHARS))
        }
        ContentBlock::Image { .. } | ContentBlock::Other => None,
    }
}

fn non_empty_text(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| text.to_string())
}

pub(crate) fn bounded_tool_summary(name: &str, input: &Value, max_chars: usize) -> String {
    format_tool_summary(name, input, max_chars)
}

fn format_tool_summary(name: &str, input: &Value, max_chars: usize) -> String {
    let mut acc = BoundedHeadTail::new(max_chars);
    acc.push_str("tool ");
    acc.push_str(name);
    if let Value::Object(map) = input {
        let prefix_len = acc.len_chars();
        acc.push_str(" input_keys=");
        let mut wrote_key = false;
        for key in map.keys() {
            if acc.head_is_full() && wrote_key {
                break;
            }
            if wrote_key {
                acc.push_str(",");
            }
            acc.push_str(key);
            wrote_key = true;
        }
        if !wrote_key {
            acc.truncate_to(prefix_len);
        }
    }
    acc.finish()
}

pub(crate) fn bounded_tool_result_text(content: &Value) -> Option<String> {
    let mut acc = BoundedHeadTail::new(MAX_AGENT_SEGMENT_CHARS);
    match content {
        Value::String(text) => acc.push_str(text),
        Value::Array(items) => {
            for item in items {
                let text = match item {
                    Value::String(text) => Some(text.as_str()),
                    Value::Object(map) => {
                        let ty = map.get("type").and_then(|value| value.as_str());
                        if ty.is_none() || ty == Some("text") {
                            map.get("text").and_then(|value| value.as_str())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(text) = text {
                    acc.push_separator('\n');
                    acc.push_str(text);
                }
            }
        }
        _ => return None,
    }
    non_empty_text(&acc.finish())
}

pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[derive(Debug)]
pub(crate) struct BoundedHeadTail {
    max_chars: usize,
    head_chars: usize,
    tail_chars: usize,
    head: String,
    tail: std::collections::VecDeque<char>,
    seen_chars: usize,
    head_seen_chars: usize,
}

impl BoundedHeadTail {
    pub(crate) fn new(max_chars: usize) -> Self {
        let head_chars = max_chars.saturating_sub(max_chars / 4);
        let tail_chars = max_chars.saturating_sub(head_chars);
        Self {
            max_chars,
            head_chars,
            tail_chars,
            head: String::new(),
            tail: std::collections::VecDeque::new(),
            seen_chars: 0,
            head_seen_chars: 0,
        }
    }

    pub(crate) fn push_str(&mut self, text: &str) {
        for ch in text.chars() {
            self.push_char(ch);
        }
    }

    pub(crate) fn push_separator(&mut self, separator: char) {
        if self.seen_chars > 0 {
            self.push_char(separator);
        }
    }

    fn push_char(&mut self, ch: char) {
        if self.max_chars == 0 {
            self.seen_chars += 1;
            return;
        }
        if self.head_seen_chars < self.head_chars {
            self.head.push(ch);
            self.head_seen_chars += 1;
        } else if self.tail_chars > 0 {
            if self.tail.len() == self.tail_chars {
                self.tail.pop_front();
            }
            self.tail.push_back(ch);
        }
        self.seen_chars += 1;
    }

    fn finish(self) -> String {
        if self.seen_chars <= self.max_chars {
            let mut output = self.head;
            output.extend(self.tail);
            return output;
        }
        if self.max_chars == 0 {
            return String::new();
        }
        let head_len = self.head.chars().count();
        let mut tail = self.tail;
        let mut include_separator = !tail.is_empty();
        while head_len + usize::from(include_separator) + tail.len() > self.max_chars {
            if include_separator && head_len + tail.len() <= self.max_chars {
                include_separator = false;
                break;
            }
            if tail.pop_front().is_none() {
                include_separator = false;
                break;
            }
        }
        if tail.is_empty() {
            include_separator = false;
        }
        let mut output = self.head;
        if include_separator {
            output.push(' ');
        }
        output.extend(tail);
        output
    }

    fn len_chars(&self) -> usize {
        self.seen_chars
    }

    fn head_is_full(&self) -> bool {
        self.head_seen_chars >= self.head_chars
    }

    fn truncate_to(&mut self, len: usize) {
        if len >= self.seen_chars {
            return;
        }
        let current = self.clone_string();
        self.head.clear();
        self.tail.clear();
        self.seen_chars = 0;
        self.head_seen_chars = 0;
        self.push_str(&current.chars().take(len).collect::<String>());
    }

    fn clone_string(&self) -> String {
        let mut output = self.head.clone();
        output.extend(self.tail.iter().copied());
        output
    }
}

fn source(
    role: AgentMessageRole,
    timestamp: Option<String>,
    jsonl_line: usize,
    part_index: usize,
    assistant_message_id: Option<String>,
    parent_tool_use_id: Option<String>,
    tool_name: Option<String>,
) -> AgentPartSource {
    AgentPartSource {
        role,
        timestamp,
        jsonl_line,
        part_index,
        assistant_message_id,
        parent_tool_use_id,
        tool_name,
    }
}

fn non_empty_message(message: AgentMessage) -> Option<AgentMessage> {
    (!message.parts.is_empty()).then_some(message)
}

fn first_user_text(message: &AgentMessage) -> Option<String> {
    message.parts.iter().find_map(|part| match part {
        AgentMessagePart::Text { text, .. } => Some(text.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(content: &str) -> AgentTranscript {
        AgentTranscript::from_reader(PathBuf::from("test.jsonl"), Cursor::new(content))
            .expect("transcript should parse")
    }

    fn user(text: &str) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2024-01-01T00:00:00Z",
            "message": {"role": "user", "content": text}
        })
        .to_string()
    }

    fn assistant(text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2024-01-01T00:00:01Z",
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    #[test]
    fn canonical_ordinals_ignore_metadata_warmup_clear_and_progress() {
        let content = [
            r#"{"type":"summary","summary":"summary"}"#.to_string(),
            user("Warmup"),
            assistant("Ready"),
            user("Caveat: The messages below were generated by the user while running local commands."),
            user("<command-name>/clear</command-name>"),
            user("<local-command-stdout></local-command-stdout>"),
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"a1"}}"#.to_string(),
            user("real question"),
            assistant("real answer"),
            user("<command-message>consult</command-message><command-name>/consult</command-name><command-args>topic</command-args>"),
        ]
        .join("\n");

        let transcript = parse(&content);
        assert_eq!(transcript.messages.len(), 3);
        assert_eq!(transcript.messages[0].ordinal, 1);
        assert_eq!(transcript.messages[1].ordinal, 2);
        assert_eq!(transcript.messages[2].ordinal, 3);
        assert!(matches!(
            &transcript.messages[2].parts[0],
            AgentMessagePart::Text { text, .. } if text == "/consult topic"
        ));
    }

    #[test]
    fn agent_progress_entries_use_subagent_visibility() {
        let content = [
            user("question"),
            r#"{"type":"progress","data":{"type":"agent_progress","agentId":"agent-abcdef","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"subagent hidden text"}]}}}}"#.to_string(),
            assistant("answer"),
        ]
        .join("\n");

        let transcript = parse(&content);

        assert_eq!(transcript.messages.len(), 3);
        assert_eq!(transcript.messages[1].ordinal, 2);
        assert_eq!(
            transcript.messages[1].parent_tool_use_id.as_deref(),
            Some("agent-abcdef")
        );
        assert!(matches!(
            &transcript.messages[1].parts[0],
            AgentMessagePart::Text { text, .. } if text == "subagent hidden text"
        ));
        assert_eq!(transcript.messages[2].ordinal, 3);
    }

    #[test]
    fn duplicate_assistant_ids_preserve_ordinal_and_use_latest_source() {
        let content = [
            user("question"),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:00:01Z",
                "message": {"id": "msg_1", "role": "assistant", "content": [{"type": "text", "text": "draft"}]}
            })
            .to_string(),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:00:02Z",
                "message": {"id": "msg_1", "role": "assistant", "content": [{"type": "text", "text": "final"}]}
            })
            .to_string(),
            user("next"),
        ]
        .join("\n");

        let transcript = parse(&content);
        assert_eq!(transcript.messages.len(), 3);
        assert_eq!(transcript.messages[1].ordinal, 2);
        assert_eq!(transcript.messages[1].jsonl_line, 3);
        assert_eq!(
            transcript.messages[1].assistant_message_id.as_deref(),
            Some("msg_1")
        );
        assert!(matches!(
            &transcript.messages[1].parts[0],
            AgentMessagePart::Text { text, source } if text == "final" && source.jsonl_line == 3
        ));
    }

    #[test]
    fn agent_search_text_ignores_non_text_tool_result_json() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".to_string(),
            content: Some(serde_json::json!({"secret":"object_needle"})),
        }];

        let text = agent_search_text_from_blocks(AgentMessageRole::User, &blocks);

        assert!(text.is_empty());
    }

    #[test]
    fn agent_search_text_caps_long_tool_use_summaries() {
        let mut input = serde_json::Map::new();
        for index in 0..MAX_AGENT_SEGMENT_CHARS {
            input.insert(format!("long_key_{index}"), Value::Bool(true));
        }
        let blocks = vec![ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "Bash".to_string(),
            input: Value::Object(input),
        }];

        let text = agent_search_text_from_blocks(AgentMessageRole::Assistant, &blocks);

        assert!(text.chars().count() <= MAX_AGENT_SEGMENT_CHARS);
        assert!(text.starts_with("tool Bash input_keys="));
    }

    #[test]
    fn agent_search_text_caps_long_tool_results_with_head_and_tail() {
        let long = format!("HEAD{}TAIL", "x".repeat(MAX_AGENT_SEGMENT_CHARS * 2));
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".to_string(),
            content: Some(Value::String(long.clone())),
        }];

        let text = agent_search_text_from_blocks(AgentMessageRole::User, &blocks);

        assert!(text.len() < long.len());
        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
        assert!(!text.contains(&"x".repeat(MAX_AGENT_SEGMENT_CHARS + 1)));
    }

    #[test]
    fn agent_search_text_caps_tool_result_arrays_without_joining_full_payload() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_1".to_string(),
            content: Some(Value::Array(vec![
                Value::String("HEAD".to_string()),
                Value::String("x".repeat(MAX_AGENT_SEGMENT_CHARS * 2)),
                serde_json::json!({"type":"text","text":"TAIL"}),
            ])),
        }];

        let text = agent_search_text_from_blocks(AgentMessageRole::User, &blocks);

        assert!(text.chars().count() <= MAX_AGENT_SEGMENT_CHARS);
        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
        assert!(!text.contains(&"x".repeat(MAX_AGENT_SEGMENT_CHARS + 1)));
    }

    #[test]
    fn bounded_tool_summary_stops_before_late_keys() {
        let mut input = serde_json::Map::new();
        for index in 0..MAX_AGENT_SEGMENT_CHARS {
            input.insert(format!("key_{index:05}"), Value::Bool(true));
        }

        let text = bounded_tool_summary("Bash", &Value::Object(input), 128);

        assert!(text.chars().count() <= 128);
        assert!(text.starts_with("tool Bash input_keys=key_00000"));
        assert!(!text.contains("key_10000"));
    }

    #[test]
    fn bounded_head_tail_preserves_exact_limit_text() {
        let mut acc = BoundedHeadTail::new(4);
        acc.push_str("abcd");

        assert_eq!(acc.finish(), "abcd");
    }

    #[test]
    fn bounded_head_tail_handles_zero_limit() {
        let mut acc = BoundedHeadTail::new(0);
        acc.push_str("abcd");

        assert!(acc.finish().is_empty());
    }

    #[test]
    fn bounded_head_tail_respects_small_limits() {
        for max in 0..=8 {
            let mut acc = BoundedHeadTail::new(max);
            acc.push_str("αβγδεζηθ");

            let output = acc.finish();

            assert!(
                output.chars().count() <= max,
                "max {max} produced {output:?}"
            );
            assert!(
                !output.ends_with(' '),
                "max {max} produced dangling separator in {output:?}"
            );
            if max >= 4 {
                assert!(
                    output.ends_with('θ'),
                    "max {max} lost tail evidence in {output:?}"
                );
            }
        }
    }

    #[test]
    fn preserves_part_level_metadata_for_mixed_messages() {
        let content = [
            user("question"),
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2024-01-01T00:00:01Z",
                "parent_tool_use_id": "toolu_parent",
                "message": {
                    "id": "msg_2",
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "plan", "signature": "sig"},
                        {"type": "text", "text": "answer"},
                        {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "ls"}}
                    ]
                }
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "timestamp": "2024-01-01T00:00:02Z",
                "parent_tool_use_id": "toolu_parent",
                "message": {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "tool response"},
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}
                    ]
                }
            })
            .to_string(),
        ]
        .join("\n");

        let transcript = parse(&content);
        let assistant = &transcript.messages[1];
        assert_eq!(assistant.role, AgentMessageRole::Assistant);
        assert_eq!(assistant.timestamp.as_deref(), Some("2024-01-01T00:00:01Z"));
        assert_eq!(
            assistant.parent_tool_use_id.as_deref(),
            Some("toolu_parent")
        );
        assert!(matches!(
            &assistant.parts[0],
            AgentMessagePart::Thinking { thinking, source }
                if thinking == "plan"
                    && source.part_index == 0
                    && source.assistant_message_id.as_deref() == Some("msg_2")
        ));
        assert!(matches!(
            &assistant.parts[2],
            AgentMessagePart::ToolUse { id, name, source, .. }
                if id == "toolu_1"
                    && name == "Bash"
                    && source.tool_name.as_deref() == Some("Bash")
                    && source.parent_tool_use_id.as_deref() == Some("toolu_parent")
        ));

        let user = &transcript.messages[2];
        assert!(matches!(
            &user.parts[1],
            AgentMessagePart::ToolResult { tool_use_id, content, source }
                if tool_use_id == "toolu_1"
                    && content.as_ref().and_then(|v| v.as_str()) == Some("ok")
                    && source.role == AgentMessageRole::User
                    && source.jsonl_line == 3
        ));
    }
}
