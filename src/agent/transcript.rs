use crate::agent::diagnostic::{AgentError, AgentErrorKind};
use crate::agent::refs::ResolvedConversation;
use crate::agent::sanitize::sanitize_agent_text;
use crate::agent::visibility::ContentVisibility;
use crate::claude::{
    AgentContent, AgentMessage as ProgressMessage, AgentProgressData, AssistantMessage,
    ContentBlock, LogEntry, UserContent, UserMessage, parse_agent_progress,
};
use crate::error::Result;
use crate::history::{MessageOrdinals, Placement, extract_skill_preview};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct AgentTranscript {
    pub path: PathBuf,
    pub messages: Vec<AgentMessage>,
    pub malformed_lines: Vec<usize>,
    pub summary: Option<String>,
    pub custom_title: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
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
pub enum AgentMessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq)]
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
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let reference = path.to_string_lossy();
        if let Some(projection) = crate::history::pi::parse_file(path).map_err(|error| {
            AgentError::malformed_transcript(Some(&reference), error.to_string())
        })? {
            let normalized = projection
                .entries
                .iter()
                .filter_map(|(_, entry)| serde_json::to_string(entry).ok())
                .collect::<Vec<_>>()
                .join("\n");
            let mut transcript =
                Self::from_reader(path.to_path_buf(), std::io::Cursor::new(normalized))?;
            transcript.malformed_lines = projection.malformed_lines;
            return Ok(transcript);
        }
        let file = File::open(path).map_err(|error| {
            AgentError::io(
                Some(&reference),
                format!("failed to open transcript: {error}"),
            )
        })?;
        Self::from_reader(path.to_path_buf(), BufReader::new(file)).map_err(|error| match error {
            crate::error::AppError::Io(error) => AgentError::io(
                Some(&reference),
                format!("failed to read transcript: {error}"),
            )
            .into(),
            crate::error::AppError::Json(error) => AgentError::malformed_transcript(
                Some(&reference),
                format!("failed to parse transcript JSONL: {error}"),
            )
            .into(),
            error => error,
        })
    }

    pub(crate) fn from_reader(path: PathBuf, reader: impl BufRead) -> Result<Self> {
        let mut messages = Vec::new();
        let mut malformed_lines = Vec::new();
        let mut valid_records = 0usize;
        let mut summary = None;
        let mut custom_title = None;
        let mut ordinals = MessageOrdinals::new();
        for (line_index, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let jsonl_line = line_index + 1;
            let entry = match serde_json::from_str::<LogEntry>(&line) {
                Ok(entry) => {
                    valid_records += 1;
                    entry
                }
                Err(_) => {
                    malformed_lines.push(jsonl_line);
                    continue;
                }
            };
            let placement = ordinals.place(&entry);
            match entry {
                LogEntry::User {
                    message,
                    timestamp,
                    parent_tool_use_id,
                    ..
                } => {
                    let Placement::Message(ordinal) = placement else {
                        continue;
                    };
                    messages.push(user_message_to_agent(
                        message,
                        timestamp,
                        jsonl_line,
                        parent_tool_use_id,
                        ordinal,
                    ));
                }
                LogEntry::Assistant {
                    message,
                    timestamp,
                    parent_tool_use_id,
                    ..
                } => {
                    let ordinal = match placement {
                        Placement::Message(ordinal) | Placement::Replaces(ordinal) => ordinal,
                        Placement::Control => continue,
                    };
                    let agent_message = assistant_message_to_agent(
                        message,
                        timestamp,
                        jsonl_line,
                        parent_tool_use_id,
                        ordinal,
                    );
                    if placement == Placement::Replaces(ordinal)
                        && let Some(existing) = messages
                            .iter_mut()
                            .find(|message| message.ordinal == ordinal)
                    {
                        *existing = agent_message;
                    } else {
                        messages.push(agent_message);
                    }
                }
                LogEntry::PiMetadata {
                    label,
                    text,
                    timestamp,
                    ..
                } => {
                    let Placement::Message(ordinal) = placement else {
                        continue;
                    };
                    let rendered = if text.is_empty() {
                        format!("[{label}]")
                    } else {
                        format!("[{label}] {text}")
                    };
                    messages.push(AgentMessage {
                        ordinal,
                        role: AgentMessageRole::User,
                        timestamp: timestamp.clone(),
                        jsonl_line,
                        assistant_message_id: None,
                        parent_tool_use_id: None,
                        parts: vec![AgentMessagePart::Text {
                            text: rendered,
                            source: AgentPartSource {
                                role: AgentMessageRole::User,
                                timestamp,
                                jsonl_line,
                                part_index: 0,
                                assistant_message_id: None,
                                parent_tool_use_id: None,
                                tool_name: None,
                            },
                        }],
                    });
                }
                LogEntry::Progress { data, .. } => {
                    if let Some(progress) = parse_agent_progress(&data)
                        && let Placement::Message(ordinal) = ordinals.place_subagent(&progress)
                    {
                        messages.push(progress_message_to_agent(progress, jsonl_line, ordinal));
                    }
                }
                LogEntry::Summary { summary: value } => {
                    if summary.is_none() && !value.trim().is_empty() {
                        summary = Some(value);
                    }
                }
                LogEntry::AiTitle { ai_title } => {
                    if !ai_title.trim().is_empty() {
                        summary = Some(ai_title);
                    }
                }
                LogEntry::CustomTitle {
                    custom_title: value,
                } => {
                    custom_title = (!value.trim().is_empty()).then_some(value);
                }
                LogEntry::FileHistorySnapshot { .. }
                | LogEntry::System { .. }
                | LogEntry::AgentName { .. }
                | LogEntry::PermissionMode { .. }
                | LogEntry::Unknown => {}
            }
        }

        if valid_records == 0 && !malformed_lines.is_empty() {
            return Err(AgentError::malformed_transcript(
                Some(&path.to_string_lossy()),
                format!(
                    "transcript has no valid JSONL records; malformed lines: {}",
                    line_number_list(&malformed_lines)
                ),
            )
            .into());
        }

        Ok(Self {
            path,
            messages,
            malformed_lines,
            summary,
            custom_title,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn malformed_warning_detail(&self) -> Option<String> {
        (!self.malformed_lines.is_empty()).then(|| {
            format!(
                "skipped {} malformed JSONL record(s) at lines {}",
                self.malformed_lines.len(),
                line_number_list(&self.malformed_lines)
            )
        })
    }

    pub fn message_anchor(
        &self,
        resolved: &ResolvedConversation,
        message: &AgentMessage,
    ) -> String {
        message_anchor(resolved, message)
    }

    pub fn resolve_anchor(&self, resolved: &ResolvedConversation, anchor: &str) -> Result<usize> {
        validate_anchor(anchor)?;
        let matches = self
            .messages
            .iter()
            .filter(|message| self.message_anchor(resolved, message) == anchor)
            .map(|message| message.ordinal)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [ordinal] => Ok(*ordinal),
            [] => Err(AgentError::new(
                AgentErrorKind::NotFound,
                Some(anchor),
                format!("message anchor {anchor} was not found in the conversation"),
            )
            .into()),
            _ => Err(AgentError::new(
                AgentErrorKind::AmbiguousRef,
                Some(anchor),
                format!("message anchor {anchor} matches {} messages", matches.len()),
            )
            .into()),
        }
    }
}

fn message_anchor(resolved: &ResolvedConversation, message: &AgentMessage) -> String {
    let mut identity = String::new();
    identity.push_str(&resolved.reference.full_ref());
    identity.push('|');
    identity.push_str(match message.role {
        AgentMessageRole::User => "user",
        AgentMessageRole::Assistant => "assistant",
    });
    for part in &message.parts {
        identity.push('|');
        match part {
            AgentMessagePart::Text { text, .. } => {
                identity.push_str("text:");
                identity.push_str(&normalized_anchor_text(text));
            }
            AgentMessagePart::ToolUse { name, input, .. } => {
                identity.push_str("tool:");
                identity.push_str(name);
                identity.push(':');
                identity.push_str(&normalized_anchor_text(&input.to_string()));
            }
            AgentMessagePart::ToolResult { content, .. } => {
                identity.push_str("result:");
                if let Some(content) = content {
                    identity.push_str(&normalized_anchor_text(&content.to_string()));
                }
            }
            AgentMessagePart::Thinking { thinking, .. } => {
                identity.push_str("thinking:");
                identity.push_str(&normalized_anchor_text(thinking));
            }
        }
    }
    format!("ma_{}", &blake3::hash(identity.as_bytes()).to_hex()[..16])
}

fn normalized_anchor_text(text: &str) -> String {
    sanitize_agent_text(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn validate_anchor(anchor: &str) -> Result<()> {
    let valid = anchor
        .strip_prefix("ma_")
        .is_some_and(|digest| digest.len() == 16 && digest.chars().all(|c| c.is_ascii_hexdigit()));
    if valid {
        Ok(())
    } else {
        Err(AgentError::invalid_ref(
            anchor,
            "message anchor must use ma_ followed by 16 hexadecimal characters",
        )
        .into())
    }
}

fn line_number_list(lines: &[usize]) -> String {
    const SHOWN: usize = 8;
    let mut output = lines
        .iter()
        .take(SHOWN)
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if lines.len() > SHOWN {
        output.push_str(",...");
    }
    output
}

fn user_message_to_agent(
    message: UserMessage,
    timestamp: Option<String>,
    jsonl_line: usize,
    parent_tool_use_id: Option<String>,
    ordinal: usize,
) -> AgentMessage {
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
    AgentMessage {
        ordinal,
        role: AgentMessageRole::User,
        timestamp,
        jsonl_line,
        assistant_message_id: None,
        parent_tool_use_id,
        parts,
    }
}

fn assistant_message_to_agent(
    message: AssistantMessage,
    timestamp: Option<String>,
    jsonl_line: usize,
    parent_tool_use_id: Option<String>,
    ordinal: usize,
) -> AgentMessage {
    let assistant_message_id = message.id;
    let parts = blocks_to_parts(
        AgentMessageRole::Assistant,
        message.content,
        timestamp.clone(),
        jsonl_line,
        assistant_message_id.clone(),
        parent_tool_use_id.clone(),
    );
    AgentMessage {
        ordinal,
        role: AgentMessageRole::Assistant,
        timestamp,
        jsonl_line,
        assistant_message_id,
        parent_tool_use_id,
        parts,
    }
}

fn progress_message_to_agent(
    progress: AgentProgressData,
    jsonl_line: usize,
    ordinal: usize,
) -> AgentMessage {
    let role = match progress.message.message_type.as_str() {
        "user" => AgentMessageRole::User,
        _ => AgentMessageRole::Assistant,
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
    AgentMessage {
        ordinal,
        role,
        timestamp: None,
        jsonl_line,
        assistant_message_id: None,
        parent_tool_use_id,
        parts,
    }
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

pub(crate) fn agent_part_search_text(part: &AgentMessagePart) -> Option<String> {
    let text = match part {
        AgentMessagePart::Text { text, .. } => text.clone(),
        AgentMessagePart::ToolUse { name, input, .. } => {
            bounded_tool_summary(name, input, MAX_AGENT_SEGMENT_CHARS)
        }
        AgentMessagePart::ToolResult { content, .. } => {
            content.as_ref().and_then(bounded_tool_result_text)?
        }
        AgentMessagePart::Thinking { thinking, .. } => thinking.clone(),
    };
    non_empty_text(&truncate_chars(
        &sanitize_agent_text(&text),
        MAX_AGENT_SEGMENT_CHARS,
    ))
}

pub(crate) const MAX_AGENT_SEGMENT_CHARS: usize = 16 * 1024;

pub(crate) fn agent_search_text_from_blocks(
    role: AgentMessageRole,
    blocks: &[ContentBlock],
) -> String {
    let mut acc = BoundedHeadTail::new(MAX_AGENT_SEGMENT_CHARS * blocks.len().max(1));
    let visibility = ContentVisibility::SEARCH;
    for block in blocks {
        let visible = match block {
            ContentBlock::Text { .. } => true,
            ContentBlock::ToolUse { .. } => visibility.tools,
            ContentBlock::ToolResult { .. } => visibility.tool_results,
            ContentBlock::Thinking { .. } => visibility.thinking,
            ContentBlock::Image { .. } | ContentBlock::Other => false,
        };
        if visible && let Some(text) = agent_search_text_from_block(role, block) {
            acc.push_separator(' ');
            acc.push_str(&sanitize_agent_text(&text));
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
    bounded_tool_result_text_with_limit(content, MAX_AGENT_SEGMENT_CHARS)
}

pub(crate) fn bounded_tool_result_text_with_limit(
    content: &Value,
    max_chars: usize,
) -> Option<String> {
    let mut acc = BoundedHeadTail::new(max_chars);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::test_support::{assistant_jsonl_line as assistant, user_jsonl_line as user};
    use std::io::Cursor;

    fn parse(content: &str) -> AgentTranscript {
        AgentTranscript::from_reader(PathBuf::from("test.jsonl"), Cursor::new(content))
            .expect("transcript should parse")
    }

    fn resolved() -> ResolvedConversation {
        let key = crate::agent::refs::AgentConversationKey::new(
            "project-a",
            "test.jsonl",
            PathBuf::from("test.jsonl"),
        );
        ResolvedConversation {
            reference: key.conversation_ref(),
            key,
        }
    }

    #[test]
    fn message_anchor_survives_unrelated_prepended_messages() {
        let target = user("durable target");
        let original = parse(&target);
        let prepended = parse(&[user("unrelated"), target].join("\n"));
        let resolved = resolved();

        assert_eq!(
            original.message_anchor(&resolved, &original.messages[0]),
            prepended.message_anchor(&resolved, &prepended.messages[1])
        );
    }

    #[test]
    fn duplicate_content_anchor_is_ambiguous() {
        let transcript = parse(&[user("duplicate"), user("duplicate")].join("\n"));
        let resolved = resolved();
        let anchor = transcript.message_anchor(&resolved, &transcript.messages[0]);

        let error = transcript.resolve_anchor(&resolved, &anchor).unwrap_err();

        assert!(matches!(
            error,
            crate::error::AppError::Agent(AgentError {
                kind: AgentErrorKind::AmbiguousRef,
                ..
            })
        ));
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
    fn malformed_lines_are_skipped_without_consuming_ordinals() {
        let content = [
            user("first"),
            "{malformed".to_string(),
            assistant("second"),
            "not json".to_string(),
            user("third"),
        ]
        .join("\n");

        let transcript = parse(&content);

        assert_eq!(transcript.malformed_lines, vec![2, 4]);
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(|message| message.ordinal)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(
            transcript.malformed_warning_detail().as_deref(),
            Some("skipped 2 malformed JSONL record(s) at lines 2,4")
        );
    }

    #[test]
    fn wholly_malformed_transcript_is_rejected() {
        let error = AgentTranscript::from_reader(
            PathBuf::from("test.jsonl"),
            Cursor::new("{malformed\nnot json"),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            crate::error::AppError::Agent(AgentError {
                kind: crate::agent::diagnostic::AgentErrorKind::MalformedTranscript,
                ..
            })
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
    fn bounded_tool_result_limit_preserves_head_and_tail() {
        let content = Value::String(format!("HEAD{}TAIL", "x".repeat(200)));

        let text = bounded_tool_result_text_with_limit(&content, 32).unwrap();

        assert_eq!(text.chars().count(), 32);
        assert!(text.starts_with("HEAD"));
        assert!(text.ends_with("TAIL"));
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
