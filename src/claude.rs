use crate::agent::transcript::bounded_tool_result_text;
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
    #[serde(rename = "ai-title")]
    AiTitle {
        #[serde(rename = "aiTitle")]
        ai_title: String,
    },
    #[serde(rename = "agent-name")]
    AgentName {
        #[allow(dead_code)]
        #[serde(rename = "agentName")]
        agent_name: String,
    },
    #[serde(rename = "permission-mode")]
    PermissionMode {
        #[allow(dead_code)]
        #[serde(flatten)]
        extra: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UserMessage {
    #[allow(dead_code)]
    pub role: String,
    pub content: UserContent,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum UserContent {
    String(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Deserialize, Clone)]
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

#[derive(Debug, Deserialize, Clone)]
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
    /// Unknown content block type.
    #[serde(other)]
    Other,
}

/// Extract only Text blocks (for previews and user-facing display)
pub fn extract_text_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract Text blocks plus ToolResult content (for search indexing)
pub fn extract_search_text_from_blocks(blocks: &[ContentBlock]) -> String {
    let mut parts = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text { text } => {
                parts.push(text.clone());
            }
            ContentBlock::ToolResult {
                content: Some(content),
                ..
            } => {
                if let Some(text) = bounded_tool_result_text(content) {
                    parts.push(text);
                }
            }
            _ => {}
        }
    }

    parts.join(" ")
}

pub fn extract_text_from_user(message: &UserMessage) -> String {
    match &message.content {
        UserContent::String(text) => text.clone(),
        UserContent::Blocks(blocks) => extract_text_from_blocks(blocks),
    }
}

pub fn extract_search_text_from_user(message: &UserMessage) -> String {
    match &message.content {
        UserContent::String(text) => text.clone(),
        UserContent::Blocks(blocks) => extract_search_text_from_blocks(blocks),
    }
}

pub fn extract_text_from_assistant(message: &AssistantMessage) -> String {
    extract_text_from_blocks(&message.content)
}

pub fn extract_search_text_from_assistant(message: &AssistantMessage) -> String {
    extract_search_text_from_blocks(&message.content)
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
    use serde_json::json;

    #[test]
    fn extract_text_from_blocks_only_text() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "id".into(),
                content: Some(json!("tool output")),
            },
        ];
        assert_eq!(extract_text_from_blocks(&blocks), "hello");
    }

    #[test]
    fn extract_search_text_includes_tool_result_string() {
        let blocks = vec![
            ContentBlock::Text {
                text: "hello".into(),
            },
            ContentBlock::ToolResult {
                tool_use_id: "id".into(),
                content: Some(json!("tool output here")),
            },
        ];
        let result = extract_search_text_from_blocks(&blocks);
        assert!(result.contains("hello"));
        assert!(result.contains("tool output here"));
    }

    #[test]
    fn extract_search_text_includes_tool_result_array() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "id".into(),
            content: Some(json!([
                {"type": "text", "text": "line one"},
                {"type": "text", "text": "line two"}
            ])),
        }];
        let result = extract_search_text_from_blocks(&blocks);
        assert!(result.contains("line one"));
        assert!(result.contains("line two"));
    }

    #[test]
    fn extract_search_text_ignores_non_text_blocks_in_array() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "id".into(),
            content: Some(json!([
                {"type": "text", "text": "visible"},
                {"type": "image", "source": {"data": "base64..."}}
            ])),
        }];
        let result = extract_search_text_from_blocks(&blocks);
        assert!(result.contains("visible"));
        assert!(!result.contains("base64"));
    }

    #[test]
    fn extract_search_text_handles_none_content() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "id".into(),
            content: None,
        }];
        assert_eq!(extract_search_text_from_blocks(&blocks), "");
    }

    #[test]
    fn extract_search_text_handles_empty_string_content() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "id".into(),
            content: Some(json!("")),
        }];
        assert_eq!(extract_search_text_from_blocks(&blocks), "");
    }

    #[test]
    fn bounded_tool_result_text_array_with_plain_strings() {
        let content = json!(["line one", "line two"]);
        let result = bounded_tool_result_text(&content);
        assert_eq!(result, Some("line one\nline two".into()));
    }

    #[test]
    fn bounded_tool_result_text_object_without_type() {
        let content = json!([{"text": "no type field"}]);
        let result = bounded_tool_result_text(&content);
        assert_eq!(result, Some("no type field".into()));
    }

    #[test]
    fn deserializes_user_message_with_document_block() {
        // Unknown block types parse as `Other` without hiding text blocks.
        let message: UserMessage = serde_json::from_value(json!({
            "role": "user",
            "content": [
                {
                    "type": "document",
                    "source": {
                        "type": "base64",
                        "media_type": "application/pdf",
                        "data": "JVBERi0xLjcK..."
                    }
                },
                { "type": "text", "text": "summarize this" }
            ]
        }))
        .expect("document block should deserialize as Other");

        match &message.content {
            UserContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert!(matches!(blocks[0], ContentBlock::Other));
            }
            other => panic!("expected blocks, got {other:?}"),
        }
        assert_eq!(extract_text_from_user(&message), "summarize this");
    }
}
