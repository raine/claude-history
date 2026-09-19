use crate::agent::transcript::bounded_tool_result_text;
use crate::claude::{
    AssistantMessage, ContentBlock, LogEntry, TokenUsage, UserContent, UserMessage,
};
use crate::error::{AppError, Result};
use crate::history::Source;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const MAX_SUPPORTED_VERSION: u64 = 3;

#[derive(Clone, Debug)]
pub struct PiHeader {
    #[allow(dead_code)]
    pub version: u64,
    pub id: String,
    pub timestamp: String,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug)]
pub struct PiProjection {
    pub source: Source,
    pub header: PiHeader,
    pub title: Option<String>,
    pub entries: Vec<(usize, LogEntry)>,
    pub leaf_id: Option<String>,
    pub malformed_lines: Vec<usize>,
}

#[derive(Debug)]
struct RawEntry {
    line: usize,
    id: String,
    parent_id: Option<String>,
    value: Value,
}

pub fn parse_file(path: &Path) -> Result<Option<PiProjection>> {
    let file = File::open(path)?;
    parse_reader(BufReader::new(file), None)
}

pub fn parse_omp_file(path: &Path) -> Result<Option<PiProjection>> {
    let file = File::open(path)?;
    parse_reader(BufReader::new(file), Some(Source::Omp))
}

pub fn is_pi_file(path: &Path) -> bool {
    parse_file(path)
        .ok()
        .flatten()
        .is_some_and(|projection| projection.source == Source::Pi)
}

pub fn is_omp_file(path: &Path) -> bool {
    parse_omp_file(path).ok().flatten().is_some()
}

fn parse_reader(
    reader: impl BufRead,
    expected_source: Option<Source>,
) -> Result<Option<PiProjection>> {
    let mut parsed = Vec::new();
    let mut malformed_lines = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(value) => parsed.push((index + 1, value)),
            Err(_) => malformed_lines.push(index + 1),
        }
    }

    let Some((_, first_value)) = parsed.first() else {
        return Ok(None);
    };
    let title_slot = first_value
        .as_object()
        .filter(|object| object.get("type").and_then(Value::as_str) == Some("title"));
    let header_index = usize::from(title_slot.is_some());
    let Some((_, header_value)) = parsed.get(header_index) else {
        return Ok(None);
    };
    let Some(header_object) = header_value.as_object() else {
        return Ok(None);
    };
    if header_object.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(None);
    }
    let source = if title_slot.is_some() {
        Source::Omp
    } else {
        expected_source.unwrap_or(Source::Pi)
    };
    if expected_source == Some(Source::Omp) && source != Source::Omp {
        return Ok(None);
    }
    let title = title_slot
        .and_then(|slot| slot.get("title"))
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty())
        .or_else(|| header_object.get("title").and_then(Value::as_str))
        .filter(|title| !title.is_empty())
        .map(str::to_owned);
    let Some(id) = header_object
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let Some(timestamp) = header_object
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let Some(cwd) = header_object
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let version = header_object
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if !(1..=MAX_SUPPORTED_VERSION).contains(&version) {
        return Ok(None);
    }

    let mut raw_entries = Vec::new();
    let mut previous_id: Option<String> = None;
    for (line, value) in parsed.into_iter().skip(header_index + 1) {
        let Some(object) = value.as_object() else {
            malformed_lines.push(line);
            continue;
        };
        let entry_id = if version == 1 {
            format!("v1-{line}")
        } else if let Some(id) = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            id.to_owned()
        } else {
            malformed_lines.push(line);
            continue;
        };
        let parent_id = if version == 1 {
            previous_id.clone()
        } else {
            object
                .get("parentId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        previous_id = Some(entry_id.clone());
        raw_entries.push(RawEntry {
            line,
            id: entry_id,
            parent_id,
            value,
        });
    }

    let leaf_id = raw_entries.last().map(|entry| entry.id.clone());
    let by_id = raw_entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut active_indices = Vec::new();
    let mut current = raw_entries.len().checked_sub(1);
    let mut visited = HashSet::new();
    while let Some(index) = current {
        let entry = &raw_entries[index];
        if !visited.insert(entry.id.clone()) {
            break;
        }
        active_indices.push(index);
        current = entry
            .parent_id
            .as_deref()
            .and_then(|parent| by_id.get(parent).copied());
    }
    active_indices.reverse();

    let mut entries = Vec::new();
    if let Some(title) = title.as_ref() {
        entries.push((
            1,
            LogEntry::CustomTitle {
                custom_title: title.clone(),
            },
        ));
    }
    entries.extend(active_indices.into_iter().filter_map(|index| {
        let raw = &raw_entries[index];
        normalize_entry(&raw.value, source).map(|entry| (raw.line, entry))
    }));

    Ok(Some(PiProjection {
        source,
        header: PiHeader {
            version,
            id,
            timestamp,
            cwd: PathBuf::from(cwd),
        },
        title,
        entries,
        leaf_id,
        malformed_lines,
    }))
}

fn normalize_entry(value: &Value, source: Source) -> Option<LogEntry> {
    let object = value.as_object()?;
    let entry_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let timestamp = object
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match entry_type {
        "message" => normalize_message(object.get("message")?, timestamp, source),
        "compaction" => metadata_with_usage(
            "Compaction",
            object.get("summary").and_then(Value::as_str).unwrap_or(""),
            timestamp,
            false,
            object.get("usage").and_then(pi_usage),
        ),
        "branch_summary" => metadata_with_usage(
            "Branch summary",
            object.get("summary").and_then(Value::as_str).unwrap_or(""),
            timestamp,
            false,
            object.get("usage").and_then(pi_usage),
        ),
        "session_info" | "title_change" => Some(LogEntry::CustomTitle {
            custom_title: object
                .get(if entry_type == "title_change" {
                    "title"
                } else {
                    "name"
                })
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        "model_change" => {
            let model = object
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "{}/{}",
                        object
                            .get("provider")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown"),
                        object
                            .get("modelId")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                    )
                });
            metadata("Model", &model, timestamp, false)
        }
        "thinking_level_change" | "label" | "custom" => None,
        "custom_message" => {
            if object.get("display").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            let text = content_text(object.get("content"));
            metadata(
                object
                    .get("customType")
                    .and_then(Value::as_str)
                    .unwrap_or("Custom"),
                &text,
                timestamp,
                true,
            )
        }
        _ => None,
    }
}

fn normalize_message(
    message: &Value,
    timestamp: Option<String>,
    source: Source,
) -> Option<LogEntry> {
    let object = message.as_object()?;
    let role = object.get("role").and_then(Value::as_str)?;
    let timestamp = timestamp.or_else(|| millis_timestamp(object.get("timestamp")));
    match role {
        "user" => Some(LogEntry::User {
            message: UserMessage {
                role: "user".to_owned(),
                content: user_content(object.get("content")),
            },
            timestamp,
            uuid: None,
            cwd: None,
            parent_tool_use_id: None,
            usage: None,
        }),
        "assistant" => Some(LogEntry::Assistant {
            agent: Some(
                match source {
                    Source::Omp => "OMP",
                    _ => "Pi",
                }
                .to_owned(),
            ),
            message: AssistantMessage {
                role: "assistant".to_owned(),
                content: assistant_content(object.get("content")),
                model: object
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                usage: object.get("usage").and_then(pi_usage),
                id: None,
            },
            timestamp,
            uuid: None,
            parent_tool_use_id: None,
        }),
        "toolResult" => Some(LogEntry::User {
            message: UserMessage {
                role: "user".to_owned(),
                content: UserContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: object
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    content: Some(tool_result_content(object)),
                }]),
            },
            timestamp: None,
            uuid: None,
            cwd: None,
            parent_tool_use_id: None,
            usage: object.get("usage").and_then(pi_usage),
        }),
        "bashExecution" => Some(LogEntry::User {
            message: UserMessage {
                role: "user".to_owned(),
                content: UserContent::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "bashExecution".to_owned(),
                    content: Some(json!(bash_text(object))),
                }]),
            },
            timestamp: None,
            uuid: None,
            cwd: None,
            parent_tool_use_id: None,
            usage: None,
        }),
        "custom" | "hookMessage" => {
            if object.get("display").and_then(Value::as_bool) == Some(false) {
                None
            } else {
                let label = object
                    .get("customType")
                    .and_then(Value::as_str)
                    .unwrap_or("Custom");
                let text = content_text(object.get("content"));
                metadata(label, &text, timestamp, true)
            }
        }
        _ => None,
    }
}

fn metadata(
    label: &str,
    text: &str,
    timestamp: Option<String>,
    searchable: bool,
) -> Option<LogEntry> {
    metadata_with_usage(label, text, timestamp, searchable, None)
}

fn metadata_with_usage(
    label: &str,
    text: &str,
    timestamp: Option<String>,
    searchable: bool,
    usage: Option<TokenUsage>,
) -> Option<LogEntry> {
    Some(LogEntry::PiMetadata {
        label: label.to_owned(),
        text: text.to_owned(),
        timestamp,
        searchable,
        usage,
    })
}

fn millis_timestamp(value: Option<&Value>) -> Option<String> {
    let millis = value?.as_i64()?;
    DateTime::<Utc>::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn user_content(value: Option<&Value>) -> UserContent {
    if let Some(text) = value.and_then(Value::as_str) {
        return UserContent::String(text.to_owned());
    }
    UserContent::Blocks(content_blocks(value, false))
}

fn assistant_content(value: Option<&Value>) -> Vec<ContentBlock> {
    content_blocks(value, true)
}

fn content_blocks(value: Option<&Value>, include_thinking: bool) -> Vec<ContentBlock> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|block| {
            let object = block.as_object()?;
            match object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
            {
                "text" => Some(ContentBlock::Text {
                    text: object
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                }),
                "thinking" if include_thinking => Some(ContentBlock::Thinking {
                    thinking: object
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    signature: object
                        .get("thinkingSignature")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                }),
                "toolCall" if include_thinking => Some(ContentBlock::ToolUse {
                    id: object
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    name: object
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    input: object
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                }),
                "image" => Some(ContentBlock::Text {
                    text: format!(
                        "[Image: {}]",
                        object
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown type")
                    ),
                }),
                _ => None,
            }
        })
        .collect()
}

fn content_text(value: Option<&Value>) -> String {
    if let Some(text) = value.and_then(Value::as_str) {
        return text.to_owned();
    }
    content_blocks(value, false)
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_content(object: &Map<String, Value>) -> Value {
    let blocks = content_blocks(object.get("content"), false)
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(json!({"type": "text", "text": text})),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut text = bounded_tool_result_text(&Value::Array(blocks)).unwrap_or_default();
    if object.get("isError").and_then(Value::as_bool) == Some(true) {
        text = format!("Error: {text}");
    }
    json!(text)
}

fn bash_text(object: &Map<String, Value>) -> String {
    let command = object.get("command").and_then(Value::as_str).unwrap_or("");
    let output = object.get("output").and_then(Value::as_str).unwrap_or("");
    let mut text = format!("$ {command}\n{output}");
    if let Some(code) = object.get("exitCode").and_then(Value::as_i64)
        && code != 0
    {
        text.push_str(&format!("\nExited with code {code}"));
    }
    if object.get("cancelled").and_then(Value::as_bool) == Some(true) {
        text.push_str("\nCommand cancelled");
    }
    bounded_tool_result_text(&json!(text)).unwrap_or_default()
}

fn pi_usage(value: &Value) -> Option<TokenUsage> {
    let object = value.as_object()?;
    Some(TokenUsage {
        input_tokens: object.get("input").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: object.get("output").and_then(Value::as_u64).unwrap_or(0),
        cache_creation_input_tokens: object
            .get("cacheWrite")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: object.get("cacheRead").and_then(Value::as_u64).unwrap_or(0),
    })
}

pub fn append_session_rename(path: &Path, title: &str) -> Result<()> {
    let projection = parse_file(path)?.ok_or_else(|| {
        AppError::ConfigError(format!("{} is not a valid Pi session", path.display()))
    })?;
    let mut ids = HashSet::new();
    let file = File::open(path)?;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { continue };
        if let Ok(value) = serde_json::from_str::<Value>(&line)
            && let Some(id) = value.get("id").and_then(Value::as_str)
        {
            ids.insert(id.to_owned());
        }
    }
    let id = (0u64..)
        .map(|counter| {
            let seed = format!(
                "{}:{}:{}",
                projection.header.id,
                Utc::now().timestamp_nanos_opt().unwrap_or(0),
                counter
            );
            blake3::hash(seed.as_bytes()).to_hex()[..8].to_owned()
        })
        .find(|id| !ids.contains(id))
        .expect("unbounded ID sequence has a free value");
    let entry = json!({
        "type": "session_info",
        "id": id,
        "parentId": projection.leaf_id,
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "name": title.replace(['\r', '\n'], " ").trim(),
    });
    let mut file = OpenOptions::new().append(true).open(path)?;
    writeln!(file, "{entry}")?;
    Ok(())
}

pub fn append_omp_session_rename(path: &Path, title: &str) -> Result<()> {
    const TITLE_SLOT_BYTES: usize = 256;

    let projection = parse_omp_file(path)?.ok_or_else(|| {
        AppError::ConfigError(format!("{} is not a valid OMP session", path.display()))
    })?;
    let title = title.replace(['\r', '\n'], " ").trim().to_owned();
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let mut ids = HashSet::new();
    let contents = std::fs::read_to_string(path)?;
    for line in contents.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line)
            && let Some(id) = value.get("id").and_then(Value::as_str)
        {
            ids.insert(id.to_owned());
        }
    }
    let id = (0u64..)
        .map(|counter| {
            let seed = format!(
                "{}:{}:{}",
                projection.header.id,
                Utc::now().timestamp_nanos_opt().unwrap_or(0),
                counter
            );
            blake3::hash(seed.as_bytes()).to_hex()[..8].to_owned()
        })
        .find(|id| !ids.contains(id))
        .expect("unbounded ID sequence has a free value");
    let entry = json!({
        "type": "title_change",
        "id": id,
        "parentId": projection.leaf_id,
        "timestamp": timestamp,
        "title": title,
        "previousTitle": projection.title,
        "source": "user",
    });
    let slot = omp_title_slot(&title, &timestamp, TITLE_SLOT_BYTES)?;
    let mut lines = contents.split_inclusive('\n');
    let first = lines.next().unwrap_or_default();
    let mut rewritten = String::new();
    if serde_json::from_str::<Value>(first.trim_end_matches('\n'))
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .as_deref()
        == Some("title")
    {
        rewritten.push_str(&slot);
    } else {
        rewritten.push_str(&slot);
        rewritten.push_str(first);
    }
    rewritten.extend(lines);
    if !rewritten.ends_with('\n') {
        rewritten.push('\n');
    }
    rewritten.push_str(&entry.to_string());
    rewritten.push('\n');

    let parent = path.parent().ok_or_else(|| {
        AppError::ConfigError(format!("{} has no parent directory", path.display()))
    })?;
    let permissions = std::fs::metadata(path)?.permissions();
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file_mut().set_permissions(permissions)?;
    temp.write_all(rewritten.as_bytes())?;
    temp.as_file_mut().sync_all()?;
    temp.persist(path)
        .map_err(|error| AppError::Io(error.error))?;
    Ok(())
}

fn omp_title_slot(title: &str, timestamp: &str, bytes: usize) -> Result<String> {
    let mut title = title.to_owned();
    loop {
        let empty = json!({
            "type": "title",
            "v": 1,
            "title": title,
            "source": "user",
            "updatedAt": timestamp,
            "pad": "",
        });
        let base = format!("{empty}\n");
        if base.len() <= bytes {
            let padding = " ".repeat(bytes - base.len());
            let line = json!({
                "type": "title",
                "v": 1,
                "title": title,
                "source": "user",
                "updatedAt": timestamp,
                "pad": padding,
            });
            let serialized = format!("{line}\n");
            if serialized.len() == bytes {
                return Ok(serialized);
            }
        }
        if title.pop().is_none() {
            return Err(AppError::ConfigError(
                "OMP title slot metadata exceeds 256 bytes".to_owned(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::transcript::AgentTranscript;
    use crate::history::{Source, parser::process_conversation_file};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi")
            .join(name)
    }

    #[test]
    fn parses_versions_one_through_three_without_rewriting() {
        for (name, version, id) in [
            ("v1.jsonl", 1, "custom_v1_id"),
            ("v2.jsonl", 2, "custom_id_with_underscores"),
            (
                "v3-branched.jsonl",
                3,
                "01912345-6789-7abc-8def-0123456789ab",
            ),
        ] {
            let before = std::fs::read(fixture(name)).unwrap();
            let projection = parse_file(&fixture(name)).unwrap().unwrap();
            assert_eq!(projection.header.version, version);
            assert_eq!(projection.header.id, id);
            assert_eq!(std::fs::read(fixture(name)).unwrap(), before);
        }
    }

    #[test]
    fn projects_only_the_active_branch_and_sanitizes_images() {
        let projection = parse_file(&fixture("v3-branched.jsonl")).unwrap().unwrap();
        let json = projection
            .entries
            .iter()
            .map(|(_, entry)| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join(" ");

        assert!(json.contains("active root question"));
        assert!(json.contains("branch summary searchable"));
        assert!(json.contains("compaction summary searchable"));
        assert!(json.contains("tool output searchable"));
        assert!(json.contains("bash output searchable"));
        assert!(json.contains("[Image: image/jpeg]"));
        assert!(!json.contains("ABANDONED_BRANCH_SENTINEL"));
        assert!(!json.contains("ABANDONED_ANSWER_SENTINEL"));
        assert!(!json.contains("HIDDEN_CUSTOM_SENTINEL"));
        assert!(!json.contains("BASE64_SECRET"));
    }

    #[test]
    fn extracts_pi_conversation_metadata_from_active_dialogue() {
        let conversation = process_conversation_file(fixture("v3-branched.jsonl"), None, None)
            .unwrap()
            .unwrap();

        assert_eq!(conversation.source, Source::Pi);
        assert_eq!(
            conversation.session_id,
            "01912345-6789-7abc-8def-0123456789ab"
        );
        assert_eq!(conversation.custom_title.as_deref(), Some("Final Pi title"));
        assert_eq!(conversation.model.as_deref(), Some("openai/gpt-5"));
        assert_eq!(conversation.total_tokens, 190);
        assert_eq!(conversation.duration_minutes, Some(1));
        assert_eq!(conversation.timestamp.timestamp(), 1_709_251_320);
        for searchable in [
            "tool output searchable",
            "bash output searchable",
            "visible custom searchable",
        ] {
            assert!(
                conversation.full_text.contains(searchable),
                "missing {searchable:?} from {:?}",
                conversation.full_text
            );
        }
        for hidden_metadata in [
            "compaction summary searchable",
            "branch summary searchable",
            "custom state searchable",
        ] {
            assert!(!conversation.full_text.contains(hidden_metadata));
        }
        assert!(!conversation.full_text.contains("HIDDEN_CUSTOM_SENTINEL"));
        assert!(!conversation.full_text.contains("ABANDONED_BRANCH_SENTINEL"));
        assert!(
            !conversation
                .preview
                .contains("compaction summary searchable")
        );
    }

    #[test]
    fn semantic_ranges_and_agent_ordinals_share_the_pi_projection() {
        let path = fixture("v3-branched.jsonl");
        let conversation = process_conversation_file(path.clone(), None, None)
            .unwrap()
            .unwrap();
        let transcript = AgentTranscript::load(path).unwrap();

        assert_eq!(conversation.message_count, 5);
        assert_eq!(transcript.messages.len(), 5);
        assert_eq!(
            conversation.semantic_turn_ranges,
            [1, 2, 5]
                .into_iter()
                .map(crate::history::MessageRange::single)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            transcript
                .messages
                .iter()
                .map(|message| message.ordinal)
                .collect::<Vec<_>>(),
            (1..=5).collect::<Vec<_>>()
        );
    }

    #[test]
    fn malformed_lines_and_parent_chains_are_bounded_to_the_leaf_suffix() {
        let content = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"message\",\"id\":\"root\",\"parentId\":null,\"timestamp\":\"2024-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"lost root\"}}\n",
            "malformed\n",
            "{\"type\":\"branch_summary\",\"id\":\"leaf\",\"parentId\":\"missing\",\"timestamp\":\"2024-01-01T00:00:02Z\",\"summary\":\"leaf suffix\"}\n"
        );
        let projection = parse_reader(std::io::Cursor::new(content), None)
            .unwrap()
            .unwrap();
        assert_eq!(projection.malformed_lines, vec![3]);
        assert_eq!(projection.entries.len(), 1);
        assert!(matches!(
            &projection.entries[0].1,
            LogEntry::PiMetadata { text, .. } if text == "leaf suffix"
        ));
    }

    #[test]
    fn rejects_header_only_and_invalid_headers_as_conversations() {
        let invalid = std::io::Cursor::new("{\"type\":\"user\",\"id\":\"x\"}\n");
        assert!(parse_reader(invalid, None).unwrap().is_none());

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("header.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
        )
        .unwrap();
        assert!(
            process_conversation_file(path, None, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn empty_session_name_clears_an_active_title() {
        let content = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"message\",\"id\":\"u\",\"parentId\":null,\"timestamp\":\"2024-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"question\"}}\n",
            "{\"type\":\"session_info\",\"id\":\"n1\",\"parentId\":\"u\",\"timestamp\":\"2024-01-01T00:00:02Z\",\"name\":\"title\"}\n",
            "{\"type\":\"session_info\",\"id\":\"n2\",\"parentId\":\"n1\",\"timestamp\":\"2024-01-01T00:00:03Z\",\"name\":\"\"}\n"
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cleared.jsonl");
        std::fs::write(&path, content).unwrap();
        let conversation = process_conversation_file(path, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(conversation.custom_title, None);
    }

    #[test]
    fn skips_v2_and_v3_entries_without_ids_before_resolving_the_leaf() {
        let content = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"s\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"message\",\"id\":\"root\",\"parentId\":null,\"timestamp\":\"2024-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"question\"}}\n",
            "{\"type\":\"message\",\"parentId\":\"root\",\"timestamp\":\"2024-01-01T00:00:02Z\",\"message\":{\"role\":\"user\",\"content\":\"invalid leaf\"}}\n"
        );
        let projection = parse_reader(std::io::Cursor::new(content), None)
            .unwrap()
            .unwrap();

        assert_eq!(projection.leaf_id.as_deref(), Some("root"));
        assert_eq!(projection.malformed_lines, vec![3]);
        assert_eq!(projection.entries.len(), 1);
    }

    #[test]
    fn migrates_v2_hook_messages_in_memory_and_counts_non_assistant_usage() {
        let content = concat!(
            "{\"type\":\"session\",\"version\":2,\"id\":\"s\",\"timestamp\":\"2024-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"message\",\"id\":\"u\",\"parentId\":null,\"timestamp\":\"2024-01-01T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"question\"}}\n",
            "{\"type\":\"message\",\"id\":\"h\",\"parentId\":\"u\",\"timestamp\":\"2024-01-01T00:00:02Z\",\"message\":{\"role\":\"hookMessage\",\"customType\":\"extension notice\",\"content\":\"visible hook\",\"display\":true}}\n",
            "{\"type\":\"compaction\",\"id\":\"c\",\"parentId\":\"h\",\"timestamp\":\"2024-01-01T00:00:03Z\",\"summary\":\"summary\",\"firstKeptEntryId\":\"u\",\"tokensBefore\":1,\"usage\":{\"input\":2,\"output\":3,\"cacheRead\":5,\"cacheWrite\":7}}\n",
            "{\"type\":\"message\",\"id\":\"t\",\"parentId\":\"c\",\"timestamp\":\"2024-01-01T00:00:04Z\",\"message\":{\"role\":\"toolResult\",\"toolCallId\":\"call\",\"content\":[],\"usage\":{\"input\":11,\"output\":13,\"cacheRead\":17,\"cacheWrite\":19}}}\n"
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.jsonl");
        std::fs::write(&path, content).unwrap();

        let conversation = process_conversation_file(path, None, None)
            .unwrap()
            .unwrap();
        assert!(conversation.full_text.contains("visible hook"));
        assert_eq!(conversation.total_tokens, 77);
        let projection = parse_reader(std::io::Cursor::new(content), None)
            .unwrap()
            .unwrap();
        assert!(matches!(
            &projection.entries[1].1,
            LogEntry::PiMetadata { label, .. } if label == "extension notice"
        ));
    }

    #[test]
    fn rename_appends_native_session_info_to_the_leaf() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::copy(fixture("v3-branched.jsonl"), &path).unwrap();

        append_session_rename(&path, "renamed\nPi").unwrap();
        let last = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .last()
            .map(str::to_owned)
            .unwrap();
        let value: Value = serde_json::from_str(&last).unwrap();
        assert_eq!(value["type"], "session_info");
        assert_eq!(value["parentId"], "name2");
        assert_eq!(value["name"], "renamed Pi");
        assert_eq!(value["id"].as_str().unwrap().len(), 8);
    }

    #[test]
    fn parses_omp_title_slot_and_active_branch() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp/v3.jsonl");
        let projection = parse_file(&path).unwrap().unwrap();
        assert_eq!(projection.source, Source::Omp);
        assert_eq!(projection.header.id, "omp_session_custom_id");
        assert_eq!(projection.title.as_deref(), Some("OMP fixture title"));
        let conversation = process_conversation_file(path, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(conversation.source, Source::Omp);
        assert_eq!(
            conversation.custom_title.as_deref(),
            Some("Updated OMP title")
        );
        assert_eq!(conversation.model.as_deref(), Some("openai/gpt-5.2"));
        assert!(conversation.full_text.contains("OMP active question"));
        assert!(!conversation.full_text.contains("OMP_ABANDONED_SENTINEL"));
    }

    #[test]
    fn omp_rename_updates_title_slot_and_appends_audit_entry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp/v3.jsonl"),
            &path,
        )
        .unwrap();

        append_omp_session_rename(&path, "renamed\nOMP").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        let first_line = contents.split_inclusive('\n').next().unwrap();
        assert_eq!(first_line.len(), 256);
        let slot: Value = serde_json::from_str(first_line).unwrap();
        assert_eq!(slot["type"], "title");
        assert_eq!(slot["title"], "renamed OMP");
        let last: Value = serde_json::from_str(contents.lines().last().unwrap()).unwrap();
        assert_eq!(last["type"], "title_change");
        assert_eq!(last["parentId"], "a2");
        assert_eq!(last["title"], "renamed OMP");
        let reparsed = parse_omp_file(&path).unwrap().unwrap();
        assert_eq!(reparsed.title.as_deref(), Some("renamed OMP"));
    }
}
