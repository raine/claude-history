//! Pi and OMP session files: a session header, then records linked into a
//! tree by `parentId`, whose active branch ends at the last record. OMP
//! files may start with a fixed-size title slot.

use crate::agent::transcript::bounded_tool_result_text;
use crate::error::{AppError, Result};
use crate::history::model::{
    Event, EventKind, Message, Origin, Part, Role, SourceKind, TitleKind, Usage, Visibility,
};
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

/// A decoded Pi or OMP session.
#[derive(Clone, Debug)]
pub struct PiSession {
    pub source: SourceKind,
    pub header: PiHeader,
    /// The title of an OMP title slot, else of the header.
    pub title: Option<String>,
    /// The events of the active branch, in order, each at the line of the
    /// record it was decoded from. The title, when there is one, comes
    /// first.
    pub events: Vec<Event>,
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

pub fn parse_file(path: &Path) -> Result<Option<PiSession>> {
    let file = File::open(path)?;
    parse_reader(BufReader::new(file), None)
}

/// Detect a supported Pi or OMP session without buffering the full file.
///
/// This reads only the first one or two non-empty, valid JSON records needed
/// to apply the same source recognition rules as `parse_reader`, while
/// skipping malformed records in the leading prefix.
pub fn detect_source(path: &Path) -> Result<Option<SourceKind>> {
    detect_source_reader(BufReader::new(File::open(path)?))
}

fn detect_source_reader(reader: impl BufRead) -> Result<Option<SourceKind>> {
    let parsed = leading_records(reader)?;
    Ok(detect_source_in_values(&parsed, None).map(|(_, source)| source))
}

/// The source and header of the session at `path`, read from its leading
/// records only: `None` when `parse_reader` would not decode it as a session
/// (of `expected`, when given).
pub fn read_header(
    path: &Path,
    expected: Option<SourceKind>,
) -> Result<Option<(SourceKind, PiHeader)>> {
    let file = File::open(path)?;
    let parsed = leading_records(BufReader::new(file))?;
    Ok(session_header(&parsed, expected).map(|(_, source, header)| (source, header)))
}

pub fn parse_omp_file(path: &Path) -> Result<Option<PiSession>> {
    let file = File::open(path)?;
    parse_reader(BufReader::new(file), Some(SourceKind::Omp))
}

pub fn is_pi_file(path: &Path) -> bool {
    read_header(path, None)
        .ok()
        .flatten()
        .is_some_and(|(source, _)| source == SourceKind::Pi)
}

pub fn is_omp_file(path: &Path) -> bool {
    read_header(path, Some(SourceKind::Omp))
        .ok()
        .flatten()
        .is_some()
}

pub(crate) fn parse_reader(
    reader: impl BufRead,
    expected_source: Option<SourceKind>,
) -> Result<Option<PiSession>> {
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

    let Some((header_index, source, header)) = session_header(&parsed, expected_source) else {
        return Ok(None);
    };
    let title_slot = leading_title_slot(&parsed);
    let slot_title = title_slot
        .and_then(|slot| slot.get("title"))
        .and_then(Value::as_str)
        .filter(|title| !title.is_empty());
    let (title_line, title) = match slot_title {
        Some(title) => (parsed[0].0, Some(title)),
        None => (
            parsed[header_index].0,
            parsed[header_index]
                .1
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.is_empty()),
        ),
    };
    let title = title.map(str::to_owned);
    let version = header.version;

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

    let mut events = Vec::new();
    if let Some(title) = title.as_ref() {
        events.push(event(
            title_line,
            None,
            EventKind::Title {
                kind: TitleKind::Custom,
                text: title.clone(),
            },
        ));
    }
    events.extend(active_indices.into_iter().filter_map(|index| {
        let raw = &raw_entries[index];
        let (timestamp, kind) = decode_entry(&raw.value)?;
        Some(event(raw.line, timestamp, kind))
    }));

    Ok(Some(PiSession {
        source,
        header,
        title,
        events,
        leaf_id,
        malformed_lines,
    }))
}

fn event(line: usize, timestamp: Option<DateTime<Utc>>, kind: EventKind) -> Event {
    Event {
        origin: Origin {
            segment: 0,
            line: u32::try_from(line).unwrap_or(u32::MAX),
        },
        timestamp,
        visibility: Visibility::Dialogue,
        kind,
    }
}

/// The first one or two non-empty, valid JSON records of `reader`: enough
/// to recognize the session's source and read its header.
fn leading_records(reader: impl BufRead) -> Result<Vec<(usize, Value)>> {
    let mut parsed = Vec::with_capacity(2);
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            parsed.push((index + 1, value));
            if parsed.len() == 1 && leading_title_slot(&parsed).is_none() {
                break;
            }
            if parsed.len() == 2 {
                break;
            }
        }
    }
    Ok(parsed)
}

/// The position, source and header of the session `parsed` starts, when its
/// header is valid.
fn session_header(
    parsed: &[(usize, Value)],
    expected_source: Option<SourceKind>,
) -> Option<(usize, SourceKind, PiHeader)> {
    let (header_index, source) = detect_source_in_values(parsed, expected_source)?;
    let header_object = parsed.get(header_index)?.1.as_object()?;
    let string = |key: &str| header_object.get(key).and_then(Value::as_str);
    let id = string("id")?.to_owned();
    let timestamp = string("timestamp")?.to_owned();
    let cwd = PathBuf::from(string("cwd")?);
    let version = header_object
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or(1);
    if !(1..=MAX_SUPPORTED_VERSION).contains(&version) {
        return None;
    }
    Some((
        header_index,
        source,
        PiHeader {
            version,
            id,
            timestamp,
            cwd,
        },
    ))
}

fn leading_title_slot(parsed: &[(usize, Value)]) -> Option<&Map<String, Value>> {
    parsed
        .first()?
        .1
        .as_object()
        .filter(|object| object.get("type").and_then(Value::as_str) == Some("title"))
}

fn detect_source_in_values(
    parsed: &[(usize, Value)],
    expected_source: Option<SourceKind>,
) -> Option<(usize, SourceKind)> {
    let title_slot = leading_title_slot(parsed);
    let header_index = usize::from(title_slot.is_some());
    let header_object = parsed.get(header_index)?.1.as_object()?;
    if header_object.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let source = if title_slot.is_some() {
        SourceKind::Omp
    } else {
        expected_source.unwrap_or(SourceKind::Pi)
    };
    if expected_source == Some(SourceKind::Omp) && source != SourceKind::Omp {
        return None;
    }
    Some((header_index, source))
}

/// The event a record of the active branch decodes into, with its time.
/// `None` for records that are not part of the conversation.
fn decode_entry(value: &Value) -> Option<(Option<DateTime<Utc>>, EventKind)> {
    let object = value.as_object()?;
    let entry_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let timestamp = object.get("timestamp").and_then(Value::as_str);
    let at = timestamp.and_then(parse_timestamp);
    let kind = match entry_type {
        "message" => return decode_message(object.get("message")?, timestamp),
        "compaction" => note(
            "Compaction",
            object.get("summary").and_then(Value::as_str).unwrap_or(""),
            false,
            object.get("usage").and_then(pi_usage),
        ),
        "branch_summary" => note(
            "Branch summary",
            object.get("summary").and_then(Value::as_str).unwrap_or(""),
            false,
            object.get("usage").and_then(pi_usage),
        ),
        "session_info" | "title_change" => EventKind::Title {
            kind: TitleKind::Custom,
            text: object
                .get(if entry_type == "title_change" {
                    "title"
                } else {
                    "name"
                })
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        },
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
            note("Model", &model, false, None)
        }
        "custom_message" => {
            if generated_custom_message(object)
                || object.get("display").and_then(Value::as_bool) == Some(false)
            {
                return None;
            }
            let text = content_text(object.get("content"));
            note(
                object
                    .get("customType")
                    .and_then(Value::as_str)
                    .unwrap_or("Custom"),
                &text,
                true,
                None,
            )
        }
        _ => return None,
    };
    Some((at, kind))
}

fn decode_message(
    message: &Value,
    timestamp: Option<&str>,
) -> Option<(Option<DateTime<Utc>>, EventKind)> {
    let object = message.as_object()?;
    let role = object.get("role").and_then(Value::as_str)?;
    let at = match timestamp {
        Some(timestamp) => parse_timestamp(timestamp),
        None => millis_timestamp(object.get("timestamp")),
    };
    let usage = || object.get("usage").and_then(pi_usage);
    Some(match role {
        "user" => {
            let mut parts = user_parts(object.get("content"));
            parts.retain(|part| {
                !matches!(part, Part::Text(text) if text.trim() == "Compaction completed. Continue.")
            });
            if parts.is_empty() {
                return None;
            }
            (at, message_kind(Role::User, parts, None, None))
        }
        "assistant" => (
            at,
            message_kind(
                Role::Assistant,
                content_parts(object.get("content"), true),
                object
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                usage(),
            ),
        ),
        "toolResult" => (
            None,
            message_kind(
                Role::User,
                vec![Part::ToolResult {
                    call_id: object
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    output: Some(tool_result_content(object)),
                }],
                None,
                usage(),
            ),
        ),
        "bashExecution" => (
            None,
            message_kind(
                Role::User,
                vec![Part::ToolResult {
                    call_id: "bashExecution".to_owned(),
                    output: Some(json!(bash_text(object))),
                }],
                None,
                None,
            ),
        ),
        "custom" | "hookMessage" => {
            if generated_custom_message(object)
                || object.get("display").and_then(Value::as_bool) == Some(false)
            {
                return None;
            }
            let label = object
                .get("customType")
                .and_then(Value::as_str)
                .unwrap_or("Custom");
            let text = content_text(object.get("content"));
            (at, note(label, &text, true, None))
        }
        _ => return None,
    })
}

/// Extension notifications and injected setup are not human prompts.
fn generated_custom_message(object: &Map<String, Value>) -> bool {
    matches!(
        object.get("customType").and_then(Value::as_str),
        Some(
            "suba-result"
                | "subagent_result"
                | "aven-prime"
                | "ad-process:notification"
                | "web-search-content-ready"
        )
    )
}

fn message_kind(
    role: Role,
    parts: Vec<Part>,
    model: Option<String>,
    usage: Option<Usage>,
) -> EventKind {
    EventKind::Message(Message {
        role,
        id: None,
        parts,
        model,
        usage,
        parent_tool_use_id: None,
    })
}

fn note(label: &str, text: &str, searchable: bool, usage: Option<Usage>) -> EventKind {
    EventKind::Note {
        label: label.to_owned(),
        text: text.to_owned(),
        searchable,
        usage,
    }
}

fn parse_timestamp(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn millis_timestamp(value: Option<&Value>) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value?.as_i64()?)
}

fn user_parts(value: Option<&Value>) -> Vec<Part> {
    if let Some(text) = value.and_then(Value::as_str) {
        return vec![Part::Text(text.to_owned())];
    }
    content_parts(value, false)
}

/// The parts of a content array: text, images as a text placeholder, and
/// for assistant messages (`assistant`) thinking and tool calls.
fn content_parts(value: Option<&Value>, assistant: bool) -> Vec<Part> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|block| {
            let object = block.as_object()?;
            let string = |key: &str| object.get(key).and_then(Value::as_str);
            match string("type").unwrap_or("unknown") {
                "text" => Some(Part::Text(string("text").unwrap_or("").to_owned())),
                "thinking" if assistant => {
                    Some(Part::Thinking(string("thinking").unwrap_or("").to_owned()))
                }
                "toolCall" if assistant => Some(Part::ToolCall {
                    id: string("id").unwrap_or("unknown").to_owned(),
                    name: string("name").unwrap_or("unknown").to_owned(),
                    input: object
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                }),
                "image" => Some(Part::Text(format!(
                    "[Image: {}]",
                    string("mimeType").unwrap_or("unknown type")
                ))),
                _ => None,
            }
        })
        .collect()
}

fn text_parts(value: Option<&Value>) -> Vec<String> {
    content_parts(value, false)
        .into_iter()
        .filter_map(|part| match part {
            Part::Text(text) => Some(text),
            _ => None,
        })
        .collect()
}

fn content_text(value: Option<&Value>) -> String {
    if let Some(text) = value.and_then(Value::as_str) {
        return text.to_owned();
    }
    text_parts(value).join("\n")
}

fn tool_result_content(object: &Map<String, Value>) -> Value {
    let blocks = text_parts(object.get("content"))
        .into_iter()
        .map(|text| json!({"type": "text", "text": text}))
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

fn pi_usage(value: &Value) -> Option<Usage> {
    let object = value.as_object()?;
    let count = |key: &str| object.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        input: count("input"),
        output: count("output"),
        cache_creation: count("cacheWrite"),
        cache_read: count("cacheRead"),
    })
}

pub fn append_session_rename(path: &Path, title: &str) -> Result<()> {
    let session = parse_file(path)?.ok_or_else(|| {
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
                session.header.id,
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
        "parentId": session.leaf_id,
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "name": title.replace(['\r', '\n'], " ").trim(),
    });
    let mut file = OpenOptions::new().append(true).open(path)?;
    writeln!(file, "{entry}")?;
    Ok(())
}

pub fn append_omp_session_rename(path: &Path, title: &str) -> Result<()> {
    const TITLE_SLOT_BYTES: usize = 256;

    let session = parse_omp_file(path)?.ok_or_else(|| {
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
                session.header.id,
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
        "parentId": session.leaf_id,
        "timestamp": timestamp,
        "title": title,
        "previousTitle": session.title,
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
    use crate::history::model::SourceKind;
    use crate::history::parser::process_conversation_file;

    #[test]
    fn extension_notifications_are_not_dialogue() {
        for kind in [
            "suba-result",
            "subagent_result",
            "aven-prime",
            "ad-process:notification",
            "web-search-content-ready",
        ] {
            assert!(decode_entry(&json!({
                "type":"custom_message", "customType":kind, "display":true, "content":"generated"
            })).is_none());
            assert!(
                decode_message(
                    &json!({
                        "role":"custom", "customType":kind, "display":true, "content":"generated"
                    }),
                    None
                )
                .is_none()
            );
        }
        assert!(decode_entry(&json!({
            "type":"custom_message", "customType":"unknown-extension", "display":true, "content":"keep"
        })).is_some());
    }

    #[test]
    fn hidden_notifications_preserve_the_active_branch_and_message_ranges() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pi.jsonl");
        let records = [
            json!({"type":"session","version":3,"id":"test","cwd":"/test","timestamp":"2026-10-15T14:00:00Z"}),
            json!({"type":"message","id":"u","parentId":null,"message":{"role":"user","content":"human question"}}),
            json!({"type":"custom_message","id":"n","parentId":"u","customType":"suba-result","display":true,"content":"notification sentinel"}),
            json!({"type":"message","id":"c","parentId":"n","message":{"role":"user","content":"Compaction completed. Continue."}}),
            json!({"type":"message","id":"a","parentId":"c","message":{"role":"assistant","content":[{"type":"text","text":"visible answer"}]}}),
        ];
        std::fs::write(
            &path,
            records
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let indexed = process_conversation_file(path.clone(), None, None)
            .unwrap()
            .unwrap();
        assert!(indexed.full_text.contains("visible answer"));
        assert!(!indexed.full_text.contains("notification sentinel"));
        assert!(!indexed.full_text.contains("Compaction completed"));
        assert_eq!(indexed.prompt_count, 1);
        assert_eq!(
            indexed.semantic_turn_ranges,
            vec![
                crate::agent::refs::MessageRange::single(1),
                crate::agent::refs::MessageRange::single(2),
            ]
        );
        let agent = AgentTranscript::load(&path).unwrap();
        assert_eq!(agent.messages.len(), 2);
        assert_eq!(agent.messages[1].ordinal, 2);
    }

    #[test]
    fn only_exact_compaction_continuation_text_is_removed() {
        let continuation = "Compaction completed. Continue.";
        for content in [
            json!(continuation),
            json!([{"type":"text","text":continuation}]),
        ] {
            assert!(decode_message(&json!({"role":"user","content":content}), None).is_none());
        }
        for text in [
            "explain Compaction completed. Continue.",
            "Compaction completed. Continue. with the tests",
            "human prompt",
        ] {
            assert!(decode_message(&json!({"role":"user","content":text}), None).is_some());
        }
        assert!(
            decode_message(
                &json!({"role":"toolResult","toolCallId":"t1","content":continuation}),
                None
            )
            .is_some()
        );
        let (_, kind) = decode_message(
            &json!({"role":"user","content":[
                {"type":"text","text":continuation}, {"type":"text","text":"human prompt"}
            ]}),
            None,
        )
        .unwrap();
        let EventKind::Message(message) = kind else {
            panic!("message")
        };
        assert_eq!(message.parts, vec![Part::Text("human prompt".to_owned())]);
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi")
            .join(name)
    }

    #[test]
    fn detects_supported_sources_from_leading_records() {
        let pi = concat!(
            "\n",
            "not-json\n",
            "{\"type\":\"session\"}\n",
            "{\"type\":\"message\"}\n"
        );
        assert_eq!(
            detect_source_reader(std::io::Cursor::new(pi)).unwrap(),
            Some(SourceKind::Pi)
        );

        let omp = concat!(
            "not-json\n",
            "{\"type\":\"title\",\"title\":\"OMP\"}\n",
            "\n",
            "{\"type\":\"session\"}\n"
        );
        assert_eq!(
            detect_source_reader(std::io::Cursor::new(omp)).unwrap(),
            Some(SourceKind::Omp)
        );
    }

    #[test]
    fn returns_none_for_unknown_first_record() {
        let claude = concat!("{\"type\":\"user\",\"message\":{}}\n", "not-json\n");
        assert_eq!(
            detect_source_reader(std::io::Cursor::new(claude)).unwrap(),
            None
        );
    }

    #[test]
    fn detects_fixture_sources() {
        for name in ["v1.jsonl", "v2.jsonl", "v3-branched.jsonl"] {
            assert_eq!(detect_source(&fixture(name)).unwrap(), Some(SourceKind::Pi));
        }
        let omp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp/v3.jsonl");
        assert_eq!(detect_source(&omp).unwrap(), Some(SourceKind::Omp));
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
        let session = parse_file(&fixture("v3-branched.jsonl")).unwrap().unwrap();
        let json = session
            .events
            .iter()
            .map(|event| format!("{:?}", event.kind))
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

        assert_eq!(conversation.source, SourceKind::Pi);
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
                .map(crate::agent::refs::MessageRange::single)
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
        assert_eq!(projection.events.len(), 1);
        assert!(matches!(
            &projection.events[0].kind,
            EventKind::Note { text, .. } if text == "leaf suffix"
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
        assert_eq!(projection.events.len(), 1);
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
        // Hook messages and tool results are not the person's own text.
        assert_eq!(conversation.own_text.as_ref(), "question");
        assert_eq!(conversation.total_tokens, 77);
        let projection = parse_reader(std::io::Cursor::new(content), None)
            .unwrap()
            .unwrap();
        assert!(matches!(
            &projection.events[1].kind,
            EventKind::Note { label, .. } if label == "extension notice"
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
        assert_eq!(projection.source, SourceKind::Omp);
        assert_eq!(projection.header.id, "omp_session_custom_id");
        assert_eq!(projection.title.as_deref(), Some("OMP fixture title"));
        let conversation = process_conversation_file(path, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(conversation.source, SourceKind::Omp);
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

    #[test]
    fn events_originate_at_their_source_lines() {
        let omp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/omp/v3.jsonl");
        for path in [fixture("v1.jsonl"), fixture("v3-branched.jsonl"), omp] {
            let lines: Vec<String> = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect();
            let session = parse_file(&path).unwrap().unwrap();
            let mut checked = 0;
            for event in &session.events {
                let EventKind::Message(message) = &event.kind else {
                    continue;
                };
                let Some(text) = message.parts.iter().find_map(|part| match part {
                    Part::Text(text) => Some(text),
                    _ => None,
                }) else {
                    continue;
                };
                let line = &lines[event.origin.line as usize - 1];
                assert!(
                    line.contains(text.as_str()),
                    "{}: line {} does not hold {text:?}",
                    path.display(),
                    event.origin.line
                );
                checked += 1;
            }
            assert!(checked >= 2, "{}", path.display());
        }
    }
}
