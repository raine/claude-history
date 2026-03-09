use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum LogEntry {
    Summary {
        summary: String,
    },
    User {
        message: UserMessage,
        /// ISO 8601 timestamp when this message was sent
        #[serde(default)]
        timestamp: Option<String>,
        /// UUID for linking with turn_duration entries
        #[allow(dead_code)]
        uuid: Option<String>,
        /// The working directory when this message was sent
        cwd: Option<String>,
        /// When set, this message is part of a subagent conversation
        /// spawned by the Task tool call with this ID
        #[serde(default, rename = "parent_tool_use_id")]
        parent_tool_use_id: Option<String>,
    },
    Assistant {
        message: AssistantMessage,
        /// ISO 8601 timestamp when this message was sent
        #[serde(default)]
        timestamp: Option<String>,
        /// UUID for linking with turn_duration entries
        #[allow(dead_code)]
        uuid: Option<String>,
        /// When set, this message is part of a subagent conversation
        /// spawned by the Task tool call with this ID
        #[serde(default, rename = "parent_tool_use_id")]
        parent_tool_use_id: Option<String>,
    },
    #[serde(rename = "file-history-snapshot")]
    #[allow(dead_code)]
    FileHistorySnapshot {
        #[serde(rename = "messageId")]
        message_id: String,
        snapshot: serde_json::Value,
        #[serde(rename = "isSnapshotUpdate")]
        is_snapshot_update: bool,
    },
    Progress {
        data: serde_json::Value,
        #[allow(dead_code)]
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[allow(dead_code)]
    System {
        subtype: String,
        level: Option<String>,
        /// Duration in milliseconds for turn_duration entries
        #[serde(rename = "durationMs")]
        duration_ms: Option<u64>,
        /// Parent UUID for linking turn_duration to preceding message
        #[serde(rename = "parentUuid")]
        parent_uuid: Option<String>,
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[serde(rename = "custom-title")]
    CustomTitle {
        #[serde(rename = "customTitle")]
        custom_title: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct UserMessage {
    #[allow(dead_code)]
    pub role: String,
    pub content: UserContent,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    String(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    #[allow(dead_code)]
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: Option<String>,
    pub usage: Option<TokenUsage>,
    /// Unique message ID to deduplicate streaming entries
    pub id: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct TokenUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        #[allow(dead_code)]
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        #[allow(dead_code)]
        tool_use_id: String,
        #[serde(default)]
        content: Option<serde_json::Value>, // Optional in some user tool result entries
    },
    Thinking {
        thinking: String,
        #[allow(dead_code)]
        signature: String,
    },
    #[allow(dead_code)]
    Image {
        source: serde_json::Value,
    },
}

/// Extract text from content blocks, used for both user and assistant messages.
/// Includes text from Text blocks and ToolResult content to make tool outputs searchable.
pub fn extract_text_from_blocks(blocks: &[ContentBlock]) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text } => parts.push(text.as_str()),
            ContentBlock::ToolResult { content, .. } => {
                if let Some(content) = content {
                    extract_tool_result_text(content, &mut parts);
                }
            }
            _ => {}
        }
    }
    parts.join(" ")
}

/// Extract searchable text from a tool result's content value.
/// Content can be a plain string or an array of content blocks.
fn extract_tool_result_text<'a>(value: &'a serde_json::Value, parts: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(s) => parts.push(s.as_str()),
        serde_json::Value::Array(arr) => {
            for item in arr {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                        parts.push(text);
                    }
                }
            }
        }
        _ => {}
    }
}

pub fn extract_text_from_user(message: &UserMessage) -> String {
    match &message.content {
        UserContent::String(text) => text.clone(),
        UserContent::Blocks(blocks) => extract_text_from_blocks(blocks),
    }
}

pub fn extract_text_from_assistant(message: &AssistantMessage) -> String {
    extract_text_from_blocks(&message.content)
}

/// Agent progress data from subagent conversations
#[derive(Debug, Deserialize)]
pub struct AgentProgressData {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    pub progress_type: String,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub message: AgentMessage,
    #[allow(dead_code)]
    pub prompt: Option<String>,
}

/// Individual message within an agent conversation
#[derive(Debug, Deserialize)]
pub struct AgentMessage {
    #[serde(rename = "type")]
    pub message_type: String, // "user" or "assistant"
    pub message: AgentMessageContent,
}

/// Content of an agent message (mirrors UserMessage/AssistantMessage structure)
#[derive(Debug, Deserialize)]
pub struct AgentMessageContent {
    #[allow(dead_code)]
    pub role: String,
    pub content: AgentContent,
}

/// Agent message content is always an array of content blocks
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AgentContent {
    Blocks(Vec<ContentBlock>),
}

/// Format a parent_tool_use_id into a short display ID.
/// Strips the "toolu_" prefix and takes the first 7 characters.
pub fn short_parent_id(parent_tool_use_id: &str) -> String {
    let stripped = parent_tool_use_id
        .strip_prefix("toolu_")
        .unwrap_or(parent_tool_use_id);
    stripped[..stripped.len().min(7)].to_string()
}

/// Attempt to parse agent progress data from a Progress entry
pub fn parse_agent_progress(data: &serde_json::Value) -> Option<AgentProgressData> {
    // Check if this is an agent_progress type
    if data.get("type").and_then(|t| t.as_str()) != Some("agent_progress") {
        return None;
    }
    serde_json::from_value(data.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_text_includes_tool_result_string() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "id1".into(),
                content: Some(serde_json::Value::String(
                    "build completed in 180ms".into(),
                )),
            },
        ];
        let text = extract_text_from_blocks(&blocks);
        assert!(text.contains("180ms"), "got: {text}");
        assert!(text.contains("hello"), "got: {text}");
    }

    #[test]
    fn extract_text_includes_tool_result_array() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "id1".into(),
            content: Some(serde_json::json!([
                {"type": "text", "text": "latency p99: 42ms"},
                {"type": "image", "source": {}}
            ])),
        }];
        let text = extract_text_from_blocks(&blocks);
        assert!(text.contains("42ms"), "got: {text}");
    }

    #[test]
    fn extract_text_skips_tool_result_without_content() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "id1".into(),
            content: None,
        }];
        let text = extract_text_from_blocks(&blocks);
        assert!(text.is_empty(), "got: {text}");
    }
}
