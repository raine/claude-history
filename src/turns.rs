//! The reader's view of a transcript.
//!
//! Text renderers (post-selection display, exports, clipboard) do not walk
//! `LogEntry`s themselves. They ask for the transcript as a sequence of
//! [`Turn`]s with visibility already applied and format each [`Part`] in
//! their own medium. The TUI viewer keeps its own walk because it also
//! coalesces tool-only turns into summaries and tracks click targets.

use crate::claude::{
    AgentContent, AgentProgressData, ContentBlock, LogEntry, UserContent, parse_agent_progress,
    short_parent_id,
};
use crate::command_tags::user_text;
use crate::error::Result;
use serde_json::Value;
use std::path::Path;

/// Which optional content the reader wants to see. Subagent turns ride on
/// `thinking`, as they do in the viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Visibility {
    pub tools: bool,
    pub thinking: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Speaker {
    User,
    /// `name` is the agent label for Pi/OMP transcripts, `None` for Claude.
    Assistant {
        name: Option<String>,
    },
    /// Pi/OMP metadata the reader should see, labelled by kind.
    Metadata {
        label: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Text(String),
    ToolCall { name: String, input: Value },
    ToolResult(Option<Value>),
    Thinking(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    /// Index into `normalized_log_entries` (the viewer's `entry_index`).
    pub entry_index: usize,
    pub speaker: Speaker,
    /// Short id of the subagent this turn belongs to, if any.
    pub subagent: Option<String>,
    pub timestamp: Option<String>,
    pub parts: Vec<Part>,
}

/// Text of a tool result when it is prose: a string, or an array of text
/// blocks joined by blank lines. `None` for structures that should be shown
/// as JSON.
pub fn tool_result_prose(content: Option<&Value>) -> Option<String> {
    match content {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => {
            let texts: Vec<&str> = items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n\n"))
        }
        _ => None,
    }
}

/// A tool result as pretty-printed JSON, or a placeholder when absent.
pub fn tool_result_json(content: Option<&Value>) -> String {
    match content {
        Some(value) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "<invalid content>".to_owned())
        }
        None => "<no content>".to_owned(),
    }
}

/// What a reader should see for a tool result: prose when it is prose,
/// JSON otherwise.
pub fn tool_result_text(content: Option<&Value>) -> String {
    tool_result_prose(content).unwrap_or_else(|| tool_result_json(content))
}

/// Reads a transcript of any supported source and projects its visible turns.
pub fn project_file(path: &Path, visibility: Visibility) -> Result<Vec<Turn>> {
    let entries = crate::history::normalized_log_entries(path)?;
    Ok(project(
        entries.into_iter().map(|(_, entry)| entry),
        visibility,
    ))
}

/// Projects normalized entries into visible turns; `entry_index` counts every
/// entry given, visible or not, so it matches the viewer's numbering.
pub fn project(entries: impl IntoIterator<Item = LogEntry>, visibility: Visibility) -> Vec<Turn> {
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(entry_index, entry)| turn_for_entry(entry_index, entry, visibility))
        .filter(|turn| !turn.parts.is_empty())
        .collect()
}

fn turn_for_entry(entry_index: usize, entry: LogEntry, visibility: Visibility) -> Option<Turn> {
    match entry {
        LogEntry::User {
            message,
            timestamp,
            parent_tool_use_id,
            ..
        } => {
            if parent_tool_use_id.is_some() && !visibility.thinking {
                return None;
            }
            let parts = match message.content {
                UserContent::String(text) => user_text(&text).map(Part::Text).into_iter().collect(),
                UserContent::Blocks(blocks) => user_parts(blocks, visibility),
            };
            Some(Turn {
                entry_index,
                speaker: Speaker::User,
                subagent: parent_tool_use_id.as_deref().map(short_parent_id),
                timestamp,
                parts,
            })
        }
        LogEntry::Assistant {
            message,
            agent,
            timestamp,
            parent_tool_use_id,
            ..
        } => {
            if parent_tool_use_id.is_some() && !visibility.thinking {
                return None;
            }
            let subagent = parent_tool_use_id.as_deref().map(short_parent_id);
            let parts = assistant_parts(message.content, visibility, subagent.is_some());
            Some(Turn {
                entry_index,
                speaker: Speaker::Assistant { name: agent },
                subagent,
                timestamp,
                parts,
            })
        }
        LogEntry::PiMetadata {
            label,
            text,
            timestamp,
            searchable: true,
            ..
        } => Some(Turn {
            entry_index,
            speaker: Speaker::Metadata { label },
            subagent: None,
            timestamp,
            parts: vec![Part::Text(text)],
        }),
        LogEntry::Progress { data, .. } => {
            if !visibility.thinking {
                return None;
            }
            let progress = parse_agent_progress(&data)?;
            subagent_turn(entry_index, progress, visibility)
        }
        _ => None,
    }
}

fn user_parts(blocks: Vec<ContentBlock>, visibility: Visibility) -> Vec<Part> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => user_text(&text).map(Part::Text),
            ContentBlock::ToolResult { content, .. } if visibility.tools => {
                Some(Part::ToolResult(content))
            }
            _ => None,
        })
        .collect()
}

/// Assistant parts in reading order: prose, then tool calls, then thinking.
/// Thinking is never shown for subagents.
fn assistant_parts(blocks: Vec<ContentBlock>, visibility: Visibility, subagent: bool) -> Vec<Part> {
    let mut text = Vec::new();
    let mut tools = Vec::new();
    let mut thinking = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text: body } if !body.trim().is_empty() => {
                text.push(Part::Text(body));
            }
            ContentBlock::ToolUse { name, input, .. } if visibility.tools => {
                tools.push(Part::ToolCall { name, input });
            }
            ContentBlock::Thinking { thinking: body, .. }
                if visibility.thinking && !subagent && !body.trim().is_empty() =>
            {
                thinking.push(Part::Thinking(body));
            }
            _ => {}
        }
    }
    text.into_iter().chain(tools).chain(thinking).collect()
}

fn subagent_turn(
    entry_index: usize,
    progress: AgentProgressData,
    visibility: Visibility,
) -> Option<Turn> {
    let subagent = Some(short_parent_id(&progress.agent_id));
    let AgentContent::Blocks(blocks) = progress.message.message.content;
    let (speaker, parts) = match progress.message.message_type.as_str() {
        "user" => (Speaker::User, user_parts(blocks, visibility)),
        "assistant" => (
            Speaker::Assistant { name: None },
            assistant_parts(blocks, visibility, true),
        ),
        _ => return None,
    };
    Some(Turn {
        entry_index,
        speaker,
        subagent,
        timestamp: None,
        parts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entries(values: &[Value]) -> Vec<LogEntry> {
        values
            .iter()
            .map(|value| serde_json::from_value(value.clone()).expect("valid entry"))
            .collect()
    }

    const ALL: Visibility = Visibility {
        tools: true,
        thinking: true,
    };

    fn text(turn: &Turn) -> String {
        turn.parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn assistant_parts_are_ordered_text_tools_thinking() {
        let turns = project(
            entries(&[json!({
                "type": "assistant",
                "message": {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "hmm", "signature": ""},
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
                    {"type": "text", "text": "answer"}
                ]}
            })]),
            ALL,
        );
        assert_eq!(turns.len(), 1);
        assert!(matches!(turns[0].parts[0], Part::Text(_)));
        assert!(matches!(turns[0].parts[1], Part::ToolCall { .. }));
        assert!(matches!(turns[0].parts[2], Part::Thinking(_)));
    }

    #[test]
    fn hidden_tools_and_thinking_drop_their_parts_and_empty_turns() {
        let turns = project(
            entries(&[
                json!({"type": "assistant", "message": {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
                ]}}),
                json!({"type": "user", "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "out"}
                ]}}),
                json!({"type": "assistant", "message": {"role": "assistant", "content": [
                    {"type": "text", "text": "done"}
                ]}}),
            ]),
            Visibility::default(),
        );
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].entry_index, 2);
        assert_eq!(text(&turns[0]), "done");
    }

    #[test]
    fn subagent_turns_ride_on_thinking_visibility() {
        let values = [
            json!({"type": "user", "parent_tool_use_id": "toolu_ABCDEFGHIJ", "message": {"role": "user", "content": "sub prompt"}}),
            json!({"type": "progress", "data": {"type": "agent_progress", "agentId": "agent1234567", "message": {"type": "assistant", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "sub reply"},
                {"type": "thinking", "thinking": "never shown", "signature": ""}
            ]}}}}),
        ];
        assert!(project(entries(&values), Visibility::default()).is_empty());
        let turns = project(entries(&values), ALL);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].subagent.as_deref(), Some("ABCDEFG"));
        assert_eq!(turns[1].subagent.as_deref(), Some("agent12"));
        assert_eq!(turns[1].parts, vec![Part::Text("sub reply".to_owned())]);
    }

    #[test]
    fn command_wrappers_are_condensed_or_dropped() {
        let turns = project(
            entries(&[
                json!({"type": "user", "message": {"role": "user", "content": "<command-name>/clear</command-name>"}}),
                json!({"type": "user", "message": {"role": "user", "content": "<command-name>/help</command-name><command-args>me</command-args>"}}),
            ]),
            ALL,
        );
        assert_eq!(turns.len(), 1);
        assert_eq!(text(&turns[0]), "/help me");
    }

    #[test]
    fn pi_metadata_is_a_labelled_turn_only_when_searchable() {
        let turns = project(
            vec![
                LogEntry::PiMetadata {
                    label: "Model".to_owned(),
                    text: "claude".to_owned(),
                    timestamp: None,
                    searchable: false,
                    usage: None,
                },
                LogEntry::PiMetadata {
                    label: "Compaction".to_owned(),
                    text: "summary".to_owned(),
                    timestamp: None,
                    searchable: true,
                    usage: None,
                },
            ],
            ALL,
        );
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].speaker,
            Speaker::Metadata {
                label: "Compaction".to_owned()
            }
        );
    }

    #[test]
    fn project_file_reads_pi_transcripts() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/v3-branched.jsonl");
        let turns = project_file(&path, Visibility::default()).unwrap();
        assert!(turns.iter().any(|turn| {
            turn.speaker
                == Speaker::Assistant {
                    name: Some("Pi".to_owned()),
                }
                && text(turn).contains("root answer")
        }));
    }
}
