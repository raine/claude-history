use crate::claude::LogEntry;
use crate::error::{AppError, Result};
use crate::history::{Conversation, LoaderMessage, ProviderKind};
use chrono::{DateTime, Local, TimeZone, Utc};
use rayon::prelude::*;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};

const EXTENSION_ID: &str = "mnemonai.mnemonai-bridge";

/// Bubble type constants from Cursor's data model
const BUBBLE_TYPE_USER: i64 = 1;
const BUBBLE_TYPE_ASSISTANT: i64 = 2;

pub struct CursorProvider {
    /// Path to the global state database: ~/Library/Application Support/Cursor/User/globalStorage/state.vscdb
    global_db_path: PathBuf,
    /// Path to workspace storage: ~/Library/Application Support/Cursor/User/workspaceStorage/
    workspace_storage_path: PathBuf,
}

#[derive(Deserialize)]
struct ConversationIndexEntry {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    timestamp: i64,
}

/// Workspace information extracted from per-workspace composer data.
struct WorkspaceInfo {
    /// Directory path for display/filtering (parent dir for .code-workspace files)
    path: PathBuf,
    /// Path to pass to `cursor` CLI for opening (may be a .code-workspace file)
    open_path: PathBuf,
    title: Option<String>,
    /// Timestamp in millis from composerData (lastUpdatedAt or createdAt)
    timestamp_millis: Option<i64>,
}

impl CursorProvider {
    pub fn new() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let cursor_user = home
            .join("Library")
            .join("Application Support")
            .join("Cursor")
            .join("User");
        Self {
            global_db_path: cursor_user.join("globalStorage").join("state.vscdb"),
            workspace_storage_path: cursor_user.join("workspaceStorage"),
        }
    }

    /// Read workspace.json from a workspace directory to get the project paths.
    /// Returns (display_path, open_path) where display_path is the directory
    /// (for filtering/display) and open_path is what to pass to `cursor` CLI
    /// (may be a .code-workspace file).
    fn resolve_workspace_path(workspace_dir: &Path) -> Option<(PathBuf, PathBuf)> {
        let workspace_json = workspace_dir.join("workspace.json");
        let content = std::fs::read_to_string(&workspace_json).ok()?;
        let json: Value = serde_json::from_str(&content).ok()?;
        // workspace.json may have "folder" or "workspace" key, both are file:// URIs
        let uri = json
            .get("folder")
            .or_else(|| json.get("workspace"))
            .and_then(|v| v.as_str())?;
        let path_str = uri.strip_prefix("file://")?;
        let open_path = PathBuf::from(path_str);
        // For .code-workspace files, use parent dir for display/filtering
        let display_path = if open_path
            .extension()
            .is_some_and(|ext| ext == "code-workspace")
        {
            open_path.parent()?.to_path_buf()
        } else {
            open_path.clone()
        };
        Some((display_path, open_path))
    }

    /// Build a map from conversation ID → (workspace path, optional title).
    /// Scans per-workspace state.vscdb files for composer.composerData entries
    /// which contain allComposers[].composerId and name values.
    fn build_workspace_map(&self) -> HashMap<String, WorkspaceInfo> {
        let workspace_dirs: Vec<_> = std::fs::read_dir(&self.workspace_storage_path)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .collect();

        // Process workspaces in parallel
        let maps: Vec<Vec<(String, WorkspaceInfo)>> = workspace_dirs
            .par_iter()
            .filter_map(|entry| {
                let dir = entry.path();
                let (display_path, open_path) = Self::resolve_workspace_path(&dir)?;
                let db_path = dir.join("state.vscdb");
                if !db_path.exists() {
                    return None;
                }
                let conn = Connection::open_with_flags(
                    &db_path,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .ok()?;

                // Read composer.composerData to get composerIds for this workspace
                let composer_data: String = conn
                    .query_row(
                        "SELECT value FROM ItemTable WHERE key = 'composer.composerData'",
                        [],
                        |row| row.get(0),
                    )
                    .ok()?;

                let parsed: Value = serde_json::from_str(&composer_data).ok()?;
                let composers = parsed.get("allComposers")?.as_array()?;

                let pairs: Vec<(String, WorkspaceInfo)> = composers
                    .iter()
                    .filter_map(|c| {
                        let id = c.get("composerId")?.as_str()?.to_string();
                        let title = c
                            .get("name")
                            .and_then(|n| n.as_str())
                            .filter(|s| !s.is_empty())
                            .map(String::from);
                        let timestamp_millis = c
                            .get("lastUpdatedAt")
                            .or_else(|| c.get("createdAt"))
                            .and_then(|v| v.as_i64());
                        Some((
                            id,
                            WorkspaceInfo {
                                path: display_path.clone(),
                                open_path: open_path.clone(),
                                title,
                                timestamp_millis,
                            },
                        ))
                    })
                    .collect();

                Some(pairs)
            })
            .collect();

        let mut map = HashMap::new();
        for pairs in maps {
            for (conv_id, info) in pairs {
                map.entry(conv_id).or_insert(info);
            }
        }
        map
    }

    /// Load all conversations from the global Cursor database.
    /// Uses a two-step batch approach for performance:
    /// 1. GROUP BY query to enumerate conversations with counts and first/last keys
    /// 2. Batch IN query to fetch only the bubbles needed for previews
    fn load_from_global_db(&self, show_last: bool) -> Result<Vec<Conversation>> {
        let conn = Connection::open_with_flags(
            &self.global_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        // Step 1: Single GROUP BY to enumerate all conversations with counts and boundary keys
        let order_func = if show_last { "MAX" } else { "MIN" };
        let query = format!(
            "SELECT SUBSTR(key, 10, INSTR(SUBSTR(key, 10), ':') - 1) as conv_id, \
                    COUNT(*) as cnt, \
                    MIN(key) as first_key, \
                    {}(key) as preview_key \
             FROM cursorDiskKV \
             WHERE key >= 'bubbleId:' AND key < 'bubbleId;' \
             GROUP BY conv_id",
            order_func
        );
        let mut stmt = conn.prepare(&query)?;

        struct ConvInfo {
            conv_id: String,
            bubble_count: usize,
            first_key: String,
            preview_key: String,
        }

        let conv_infos: Vec<ConvInfo> = stmt
            .query_map([], |row| {
                Ok(ConvInfo {
                    conv_id: row.get(0)?,
                    bubble_count: row.get::<_, usize>(1)?,
                    first_key: row.get(2)?,
                    preview_key: row.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        let _ = crate::debug_log::log_debug(&format!(
            "Cursor: found {} conversations from GROUP BY",
            conv_infos.len()
        ));

        // Step 2: Batch-fetch the bubbles we need for previews and timestamps.
        // Collect all unique keys we need to fetch (first_key for timestamp, preview_key for text).
        let mut keys_to_fetch: Vec<String> = Vec::with_capacity(conv_infos.len() * 2);
        for info in &conv_infos {
            keys_to_fetch.push(info.first_key.clone());
            if info.first_key != info.preview_key {
                keys_to_fetch.push(info.preview_key.clone());
            }
        }
        keys_to_fetch.sort();
        keys_to_fetch.dedup();

        // Fetch all needed bubbles in batches using IN clauses
        let mut bubble_map: HashMap<String, Value> = HashMap::with_capacity(keys_to_fetch.len());
        for chunk in keys_to_fetch.chunks(200) {
            let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
            let sql = format!(
                "SELECT key, CAST(value AS TEXT) FROM cursorDiskKV WHERE key IN ({})",
                placeholders.join(",")
            );
            let mut batch_stmt = conn.prepare(&sql)?;
            let params: Vec<&dyn rusqlite::types::ToSql> =
                chunk.iter().map(|k| k as &dyn rusqlite::types::ToSql).collect();
            let rows = batch_stmt.query_map(params.as_slice(), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows.flatten() {
                if let Ok(v) = serde_json::from_str::<Value>(&row.1) {
                    bubble_map.insert(row.0, v);
                }
            }
        }

        let _ = crate::debug_log::log_debug(&format!(
            "Cursor: fetched {} bubbles for previews",
            bubble_map.len()
        ));

        // Build timestamp map from conversation index
        let mut index_timestamps: HashMap<String, i64> = HashMap::new();
        if let Ok(index_json) = conn.query_row::<String, _, _>(
            "SELECT value FROM ItemTable WHERE key = 'conversationClassificationScoredConversations'",
            [],
            |row| row.get(0),
        ) {
            if let Ok(index) = serde_json::from_str::<Vec<ConversationIndexEntry>>(&index_json) {
                for entry in index {
                    index_timestamps.insert(entry.conversation_id, entry.timestamp);
                }
            }
        }

        // Build workspace map in parallel
        let workspace_map = self.build_workspace_map();

        let _ = crate::debug_log::log_debug(&format!(
            "Cursor: built workspace map with {} entries",
            workspace_map.len()
        ));

        // Step 3: Build Conversation structs from the collected data
        let mut conversations = Vec::new();

        for info in &conv_infos {
            // Extract preview text from the preview bubble
            let preview_bubble = bubble_map.get(&info.preview_key);
            let preview_text = preview_bubble.and_then(|v| {
                let btype = v.get("type").and_then(|t| t.as_i64()).unwrap_or(0);
                if btype == BUBBLE_TYPE_USER {
                    // Try text field first, fall back to richText
                    let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                    if let Some(rt) = v.get("richText").and_then(|r| r.as_str()) {
                        return extract_text_from_richtext(rt);
                    }
                }
                None
            });

            // If the preview_key bubble wasn't a user message (e.g. show_last=false
            // and first bubble is assistant), try to find user text from first_key
            let preview_text = preview_text.or_else(|| {
                let first_bubble = bubble_map.get(&info.first_key)?;
                let btype = first_bubble.get("type").and_then(|t| t.as_i64()).unwrap_or(0);
                if btype == BUBBLE_TYPE_USER {
                    let text = first_bubble.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        return Some(text.to_string());
                    }
                    if let Some(rt) = first_bubble.get("richText").and_then(|r| r.as_str()) {
                        return extract_text_from_richtext(rt);
                    }
                }
                None
            });

            let preview_text = match preview_text {
                Some(t) if !t.is_empty() => t,
                _ => continue, // Skip conversations with no user text
            };

            // Extract model from whichever bubble has it
            let model = bubble_map
                .get(&info.first_key)
                .and_then(|v| v.get("modelType").and_then(|m| m.as_str()).map(String::from))
                .or_else(|| {
                    bubble_map
                        .get(&info.preview_key)
                        .and_then(|v| v.get("modelType").and_then(|m| m.as_str()).map(String::from))
                });

            let ws_info = workspace_map.get(&info.conv_id);
            let workspace_path = ws_info.map(|i| i.path.clone());
            let workspace_open_path = ws_info.map(|i| i.open_path.clone());
            let title = ws_info.and_then(|i| i.title.clone());

            let project_name = workspace_path
                .as_ref()
                .map(|p| crate::history::format_short_name_from_path(p));

            // Resolve timestamp: index > first bubble createdAt > workspace composerData
            let local_ts = if let Some(&ts_millis) = index_timestamps.get(&info.conv_id) {
                let ts = Utc
                    .timestamp_millis_opt(ts_millis)
                    .single()
                    .unwrap_or_else(Utc::now);
                ts.with_timezone(&Local)
            } else if let Some(ts) = bubble_map.get(&info.first_key).and_then(|v| {
                let created_at = v.get("createdAt")?.as_str()?;
                let dt = DateTime::parse_from_rfc3339(created_at).ok()?;
                Some(dt.with_timezone(&Local))
            }) {
                ts
            } else if let Some(ws_ts) = ws_info.and_then(|i| i.timestamp_millis) {
                let ts = Utc
                    .timestamp_millis_opt(ws_ts)
                    .single()
                    .unwrap_or_else(Utc::now);
                ts.with_timezone(&Local)
            } else {
                continue; // No timestamp from any source
            };

            let fake_path = self
                .global_db_path
                .with_file_name(format!("cursor-{}.jsonl", &info.conv_id));

            let preview = preview_text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let full_text = preview.clone();

            conversations.push(Conversation {
                path: fake_path,
                index: 0,
                provider: ProviderKind::Cursor,
                id: info.conv_id.clone(),
                timestamp: local_ts,
                preview,
                full_text,
                project_name,
                project_path: workspace_path,
                cwd: workspace_open_path,
                message_count: info.bubble_count,
                parse_errors: Vec::new(),
                summary: title.clone(),
                model,
                total_tokens: 0,
                duration_minutes: None,
            });
        }

        let _ = crate::debug_log::log_debug(&format!(
            "Cursor: returning {} conversations",
            conversations.len()
        ));

        Ok(conversations)
    }

    /// Load all bubbles for a conversation from the global database.
    fn load_bubbles(conv_id: &str, global_db_path: &Path) -> Result<Vec<Bubble>> {
        let conn = Connection::open_with_flags(
            global_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        let prefix = format!("bubbleId:{}:", conv_id);
        let prefix_end = format!("bubbleId:{};", conv_id); // ';' is after ':' in ASCII

        let mut stmt = conn.prepare(
            "SELECT CAST(value AS TEXT) FROM cursorDiskKV WHERE key >= ? AND key < ? ORDER BY ROWID",
        )?;

        let bubbles: Vec<Bubble> = stmt
            .query_map(rusqlite::params![&prefix, &prefix_end], |row| {
                row.get::<_, String>(0)
            })?
            .filter_map(|r| r.ok())
            .filter_map(|json_str| parse_bubble(&json_str))
            .collect();

        Ok(bubbles)
    }
}

impl super::Provider for CursorProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Cursor
    }

    fn name(&self) -> &str {
        "Cursor"
    }

    fn detect(&self) -> bool {
        self.global_db_path.exists()
    }

    fn load_conversations(
        &self,
        show_last: bool,
        _debug: Option<crate::cli::DebugLevel>,
    ) -> Result<Vec<Conversation>> {
        let _ = crate::debug_log::log_debug(&format!(
            "Cursor: checking global DB at {}",
            self.global_db_path.display()
        ));

        if !self.global_db_path.exists() {
            let _ = crate::debug_log::log_debug("Cursor: global DB does not exist, skipping");
            return Ok(Vec::new());
        }

        self.load_from_global_db(show_last)
    }

    fn load_conversations_streaming(
        &self,
        show_last: bool,
        _debug: Option<crate::cli::DebugLevel>,
    ) -> Receiver<LoaderMessage> {
        let (tx, rx) = mpsc::channel();
        let global_db_path = self.global_db_path.clone();
        let workspace_storage_path = self.workspace_storage_path.clone();

        std::thread::spawn(move || {
            let _ = crate::debug_log::log_debug(&format!(
                "Cursor streaming: checking {}",
                global_db_path.display()
            ));

            if !global_db_path.exists() {
                let _ =
                    crate::debug_log::log_debug("Cursor streaming: global DB does not exist");
                let _ = tx.send(LoaderMessage::Done);
                return;
            }

            let provider = CursorProvider {
                global_db_path,
                workspace_storage_path,
            };

            match provider.load_from_global_db(show_last) {
                Ok(convs) if !convs.is_empty() => {
                    let _ = crate::debug_log::log_debug(&format!(
                        "Cursor streaming: sending {} conversations",
                        convs.len()
                    ));
                    let _ = tx.send(LoaderMessage::Batch(convs));
                }
                Ok(_) => {
                    let _ =
                        crate::debug_log::log_debug("Cursor streaming: no conversations found");
                }
                Err(e) => {
                    let _ = crate::debug_log::log_debug(&format!(
                        "Cursor streaming: error loading: {}",
                        e
                    ));
                }
            }

            let _ = tx.send(LoaderMessage::Done);
        });

        rx
    }

    fn read_entries(&self, conversation: &Conversation) -> Result<Vec<LogEntry>> {
        let bubbles = Self::load_bubbles(&conversation.id, &self.global_db_path)?;

        let entries: Vec<LogEntry> = bubbles
            .into_iter()
            .filter_map(|bubble| bubble_to_log_entry(&bubble))
            .collect();

        Ok(entries)
    }

    fn resume(&self, conversation: &Conversation, _default_args: &[String]) -> Result<()> {
        // Check if the bridge extension is installed
        if !is_extension_installed() {
            install_extension()?;
        }

        // Open/focus the workspace (reuses existing window if already open)
        // Uses cwd which stores the open_path (may be a .code-workspace file)
        if let Some(ref path) = conversation.cwd {
            Command::new("cursor")
                .arg(&path.to_string_lossy().as_ref())
                .spawn()
                .map_err(|e| {
                    AppError::ClaudeExecutionError(format!("Failed to launch Cursor: {}", e))
                })?;

            // Give Cursor time to open/focus the window and activate the extension
            std::thread::sleep(std::time::Duration::from_secs(3));
        }

        // Open the conversation via URI (routes to the focused window)
        let uri = format!(
            "cursor://{}/open?id={}",
            EXTENSION_ID, conversation.id
        );

        Command::new("open")
            .arg(&uri)
            .status()
            .map_err(|e| {
                AppError::ClaudeExecutionError(format!("Failed to open Cursor URI: {}", e))
            })?;

        Ok(())
    }

    fn delete(&self, conversation: &Conversation) -> Result<()> {
        let conn = Connection::open_with_flags(
            &self.global_db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ).map_err(|e| {
            AppError::ClaudeExecutionError(format!(
                "Failed to open Cursor database for writing: {}",
                e
            ))
        })?;

        let conv_id = &conversation.id;

        // Delete all related keys from cursorDiskKV
        for prefix in &["bubbleId", "checkpointId", "codeBlockDiff"] {
            let key_prefix = format!("{}:{}:", prefix, conv_id);
            let key_prefix_end = format!("{}:{};", prefix, conv_id);
            conn.execute(
                "DELETE FROM cursorDiskKV WHERE key >= ? AND key < ?",
                rusqlite::params![&key_prefix, &key_prefix_end],
            )?;
        }

        // Delete composerData entry
        let composer_key = format!("composerData:{}", conv_id);
        conn.execute(
            "DELETE FROM cursorDiskKV WHERE key = ?",
            rusqlite::params![&composer_key],
        )?;

        // Remove from conversation index in ItemTable
        if let Ok(index_json) = conn.query_row::<String, _, _>(
            "SELECT value FROM ItemTable WHERE key = 'conversationClassificationScoredConversations'",
            [],
            |row| row.get(0),
        ) {
            if let Ok(mut index) = serde_json::from_str::<Vec<Value>>(&index_json) {
                let before = index.len();
                index.retain(|e| {
                    e.get("conversationId")
                        .and_then(|v| v.as_str())
                        != Some(conv_id)
                });
                if index.len() != before {
                    if let Ok(new_json) = serde_json::to_string(&index) {
                        let _ = conn.execute(
                            "UPDATE ItemTable SET value = ? WHERE key = 'conversationClassificationScoredConversations'",
                            rusqlite::params![&new_json],
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

// --- Internal types ---

/// Parsed bubble (message) from Cursor's cursorDiskKV table.
struct Bubble {
    /// 1 = user, 2 = assistant
    bubble_type: i64,
    /// Main text content
    text: String,
    /// Lexical richText JSON (for user messages where text may be empty)
    rich_text: Option<String>,
    /// ISO 8601 timestamp
    created_at: Option<String>,
    /// Model info (e.g. "gpt-5-codex")
    model: Option<String>,
    /// Thinking block text
    thinking: Option<String>,
    /// Thinking block signature
    thinking_signature: Option<String>,
    /// Tool call data
    tool_name: Option<String>,
    tool_args: Option<String>,
    tool_call_id: Option<String>,
    tool_result: Option<String>,
    #[allow(dead_code)]
    tool_status: Option<String>,
}

// --- Parsing helpers ---

fn parse_bubble(json_str: &str) -> Option<Bubble> {
    let v: Value = serde_json::from_str(json_str).ok()?;

    let bubble_type = v.get("type").and_then(|t| t.as_i64())?;

    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    let rich_text = v
        .get("richText")
        .and_then(|r| {
            if r.is_string() {
                r.as_str().map(String::from)
            } else if r.is_object() {
                Some(r.to_string())
            } else {
                None
            }
        });

    let created_at = v
        .get("createdAt")
        .and_then(|t| t.as_str())
        .map(String::from);

    let model = v
        .get("modelInfo")
        .and_then(|m| m.get("modelName"))
        .and_then(|n| n.as_str())
        .map(String::from);

    let thinking = v
        .get("thinking")
        .and_then(|t| t.get("text"))
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    let thinking_signature = v
        .get("thinking")
        .and_then(|t| t.get("signature"))
        .and_then(|t| t.as_str())
        .map(String::from);

    let tool_former = v.get("toolFormerData");
    let tool_name = tool_former
        .and_then(|t| t.get("name"))
        .and_then(|n| n.as_str())
        .map(String::from);
    let tool_args = tool_former
        .and_then(|t| {
            // Try rawArgs first, fall back to params (newer Cursor format)
            let raw = t.get("rawArgs").and_then(|a| {
                if let Some(s) = a.as_str() {
                    if s.is_empty() { None } else { Some(s.to_string()) }
                } else if a.is_object() || a.is_array() {
                    Some(a.to_string())
                } else {
                    None
                }
            });
            raw.or_else(|| {
                t.get("params").and_then(|p| {
                    if let Some(s) = p.as_str() {
                        if s.is_empty() { None } else { Some(s.to_string()) }
                    } else if p.is_object() || p.is_array() {
                        Some(p.to_string())
                    } else {
                        None
                    }
                })
            })
        });
    let tool_call_id = tool_former
        .and_then(|t| t.get("toolCallId"))
        .and_then(|i| i.as_str())
        .map(String::from);
    let tool_result = tool_former
        .and_then(|t| t.get("result"))
        .and_then(|r| r.as_str())
        .map(String::from);
    let tool_status = tool_former
        .and_then(|t| {
            t.get("additionalData")
                .and_then(|a| a.get("status"))
                .or_else(|| t.get("status"))
        })
        .and_then(|s| s.as_str())
        .map(String::from);

    Some(Bubble {
        bubble_type,
        text,
        rich_text,
        created_at,
        model,
        thinking,
        thinking_signature,
        tool_name,
        tool_args,
        tool_call_id,
        tool_result,
        tool_status,
    })
}

/// Extract plain text from a Lexical richText JSON structure.
/// Recursively walks the tree collecting all text node values.
fn extract_text_from_richtext(rich_text_str: &str) -> Option<String> {
    let v: Value = serde_json::from_str(rich_text_str).ok()?;
    let mut parts = Vec::new();
    collect_text_nodes(&v, &mut parts);
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(""))
    }
}

fn collect_text_nodes(node: &Value, parts: &mut Vec<String>) {
    // If this node is a text node, collect its text
    if node.get("type").and_then(|t| t.as_str()) == Some("text") {
        if let Some(text) = node.get("text").and_then(|t| t.as_str()) {
            parts.push(text.to_string());
        }
        return;
    }
    // If this is a paragraph/linebreak, add a newline separator
    if node.get("type").and_then(|t| t.as_str()) == Some("linebreak") {
        parts.push("\n".to_string());
        return;
    }
    // Recurse into children
    if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
        for (i, child) in children.iter().enumerate() {
            collect_text_nodes(child, parts);
            // Add newline between paragraphs
            if i < children.len() - 1
                && child.get("type").and_then(|t| t.as_str()) == Some("paragraph")
            {
                parts.push("\n".to_string());
            }
        }
    }
    // Also check "root" wrapper
    if let Some(root) = node.get("root") {
        collect_text_nodes(root, parts);
    }
}

/// Get the effective text content from a bubble.
fn bubble_text(bubble: &Bubble) -> String {
    if !bubble.text.is_empty() {
        return bubble.text.clone();
    }
    // Fallback to richText for user messages
    if let Some(ref rt) = bubble.rich_text {
        if let Some(text) = extract_text_from_richtext(rt) {
            return text;
        }
    }
    String::new()
}

/// Convert a Cursor bubble to a LogEntry for the viewer.
fn bubble_to_log_entry(bubble: &Bubble) -> Option<LogEntry> {
    let timestamp = bubble
        .created_at
        .clone()
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    match bubble.bubble_type {
        BUBBLE_TYPE_USER => {
            let text = bubble_text(bubble);
            if text.is_empty() {
                return None;
            }
            let json = serde_json::json!({
                "type": "user",
                "timestamp": timestamp,
                "message": {
                    "role": "user",
                    "content": text
                }
            });
            serde_json::from_value(json).ok()
        }
        BUBBLE_TYPE_ASSISTANT => {
            let text = bubble_text(bubble);

            // Build content blocks
            let mut content_blocks = Vec::new();

            // Add thinking block if present
            if let (Some(thinking), Some(signature)) =
                (&bubble.thinking, &bubble.thinking_signature)
            {
                content_blocks.push(serde_json::json!({
                    "type": "thinking",
                    "thinking": thinking,
                    "signature": signature
                }));
            }

            // Add tool use block if present (tool_call_id and tool_args may be absent)
            if let Some(name) = &bubble.tool_name {
                let id = bubble
                    .tool_call_id
                    .as_deref()
                    .unwrap_or("unknown")
                    .to_string();
                let input: Value = bubble
                    .tool_args
                    .as_ref()
                    .and_then(|args| serde_json::from_str(args).ok())
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input
                }));

                // Add tool result if present
                if let Some(ref result) = bubble.tool_result {
                    let truncated = truncate_str(result, 500);
                    content_blocks.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": truncated
                    }));
                }
            }

            // Add text block if present
            if !text.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": text
                }));
            }

            if content_blocks.is_empty() {
                return None;
            }

            let mut message = serde_json::json!({
                "role": "assistant",
                "content": content_blocks
            });

            if let Some(ref model) = bubble.model {
                message
                    .as_object_mut()
                    .unwrap()
                    .insert("model".to_string(), Value::String(model.clone()));
            }

            let json = serde_json::json!({
                "type": "assistant",
                "timestamp": timestamp,
                "message": message
            });

            serde_json::from_value(json).ok()
        }
        _ => None,
    }
}

/// Truncate a string to at most `max_bytes` bytes at a valid char boundary.
fn truncate_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Find the last valid char boundary at or before max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

fn is_extension_installed() -> bool {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let extensions_dir = home.join(".cursor").join("extensions");
    let ext_dir = extensions_dir.join("mnemonai.mnemonai-bridge-0.1.0");

    // Check both: directory exists AND registered in extensions.json
    if !ext_dir.join("extension.js").exists() {
        return false;
    }

    let extensions_json_path = extensions_dir.join("extensions.json");
    if !extensions_json_path.exists() {
        return false;
    }

    let content = match std::fs::read_to_string(&extensions_json_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let extensions: Vec<Value> = match serde_json::from_str(&content) {
        Ok(e) => e,
        Err(_) => return false,
    };

    extensions.iter().any(|e| {
        e.get("identifier")
            .and_then(|id| id.get("id"))
            .and_then(|id| id.as_str())
            == Some(EXTENSION_ID)
    })
}

fn install_extension() -> Result<()> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let extensions_dir = home.join(".cursor").join("extensions");
    let ext_dir = extensions_dir.join("mnemonai.mnemonai-bridge-0.1.0");

    // Copy extension files
    std::fs::create_dir_all(&ext_dir).map_err(AppError::Io)?;

    let package_json = include_str!("../../extension/package.json");
    std::fs::write(ext_dir.join("package.json"), package_json).map_err(AppError::Io)?;

    let extension_js = include_str!("../../extension/extension.js");
    std::fs::write(ext_dir.join("extension.js"), extension_js).map_err(AppError::Io)?;

    // Register in extensions.json so Cursor discovers it
    let extensions_json_path = extensions_dir.join("extensions.json");
    let mut extensions: Vec<Value> = if extensions_json_path.exists() {
        let content = std::fs::read_to_string(&extensions_json_path).map_err(AppError::Io)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Check if already registered
    let already_registered = extensions.iter().any(|e| {
        e.get("identifier")
            .and_then(|id| id.get("id"))
            .and_then(|id| id.as_str())
            == Some(EXTENSION_ID)
    });

    if !already_registered {
        let entry = serde_json::json!({
            "identifier": { "id": EXTENSION_ID },
            "version": "0.1.0",
            "location": {
                "$mid": 1,
                "fsPath": ext_dir.to_string_lossy(),
                "external": format!("file://{}", ext_dir.to_string_lossy()),
                "path": ext_dir.to_string_lossy(),
                "scheme": "file"
            },
            "relativeLocation": "mnemonai.mnemonai-bridge-0.1.0"
        });
        extensions.push(entry);
        let json = serde_json::to_string(&extensions).map_err(|e| {
            AppError::ClaudeExecutionError(format!("Failed to serialize extensions.json: {}", e))
        })?;
        std::fs::write(&extensions_json_path, json).map_err(AppError::Io)?;
    }

    eprintln!("Installed mnemonai-bridge extension.");
    eprintln!("Please restart Cursor for the extension to take effect.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_text_from_richtext() {
        let rt = r#"{"root":{"children":[{"children":[{"detail":0,"format":0,"mode":"normal","style":"","text":"Hello world","type":"text","version":1}],"direction":"ltr","format":"","indent":0,"type":"paragraph","version":1}],"direction":"ltr","format":"","indent":0,"type":"root","version":1}}"#;
        assert_eq!(
            extract_text_from_richtext(rt),
            Some("Hello world".to_string())
        );
    }

    #[test]
    fn test_extract_text_from_richtext_multiline() {
        let rt = r#"{"root":{"children":[{"children":[{"text":"Line 1","type":"text"}],"type":"paragraph"},{"children":[{"text":"Line 2","type":"text"}],"type":"paragraph"}],"type":"root"}}"#;
        assert_eq!(
            extract_text_from_richtext(rt),
            Some("Line 1\nLine 2".to_string())
        );
    }

    #[test]
    fn test_parse_bubble_user_message() {
        let json = r#"{"_v":3,"type":1,"text":"Hello","createdAt":"2025-01-01T00:00:00Z","bubbleId":"test-id"}"#;
        let bubble = parse_bubble(json).unwrap();
        assert_eq!(bubble.bubble_type, 1);
        assert_eq!(bubble.text, "Hello");
        assert_eq!(bubble.created_at, Some("2025-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_parse_bubble_assistant_with_model() {
        let json = r#"{"_v":3,"type":2,"text":"Sure!","createdAt":"2025-01-01T00:00:01Z","bubbleId":"test-id","modelInfo":{"modelName":"claude-sonnet-4"}}"#;
        let bubble = parse_bubble(json).unwrap();
        assert_eq!(bubble.bubble_type, 2);
        assert_eq!(bubble.text, "Sure!");
        assert_eq!(bubble.model, Some("claude-sonnet-4".to_string()));
    }

    #[test]
    fn test_parse_bubble_with_thinking() {
        let json = r#"{"_v":3,"type":2,"text":"","createdAt":"2025-01-01T00:00:01Z","bubbleId":"test-id","thinking":{"text":"Let me think about this...","signature":"sig123"}}"#;
        let bubble = parse_bubble(json).unwrap();
        assert_eq!(
            bubble.thinking,
            Some("Let me think about this...".to_string())
        );
        assert_eq!(bubble.thinking_signature, Some("sig123".to_string()));
    }

    #[test]
    fn test_parse_bubble_with_tool() {
        let json = r#"{"_v":3,"type":2,"text":"","createdAt":"2025-01-01T00:00:01Z","bubbleId":"test-id","toolFormerData":{"name":"read_file","rawArgs":"{\"path\":\"test.rs\"}","toolCallId":"call_123","status":"completed","result":"file contents"}}"#;
        let bubble = parse_bubble(json).unwrap();
        assert_eq!(bubble.tool_name, Some("read_file".to_string()));
        assert_eq!(bubble.tool_call_id, Some("call_123".to_string()));
    }

    #[test]
    fn test_bubble_to_log_entry_user() {
        let bubble = Bubble {
            bubble_type: BUBBLE_TYPE_USER,
            text: "Hello".to_string(),
            rich_text: None,
            created_at: Some("2025-01-01T00:00:00Z".to_string()),
            model: None,
            thinking: None,
            thinking_signature: None,
            tool_name: None,
            tool_args: None,
            tool_call_id: None,
            tool_result: None,
            tool_status: None,
        };
        let entry = bubble_to_log_entry(&bubble).unwrap();
        match entry {
            crate::claude::LogEntry::User { message, .. } => {
                assert_eq!(
                    crate::claude::extract_text_from_user(&message),
                    "Hello"
                );
            }
            _ => panic!("Expected User entry"),
        }
    }

    #[test]
    fn test_bubble_to_log_entry_assistant() {
        let bubble = Bubble {
            bubble_type: BUBBLE_TYPE_ASSISTANT,
            text: "Sure, I can help!".to_string(),
            rich_text: None,
            created_at: Some("2025-01-01T00:00:01Z".to_string()),
            model: Some("claude-sonnet-4".to_string()),
            thinking: None,
            thinking_signature: None,
            tool_name: None,
            tool_args: None,
            tool_call_id: None,
            tool_result: None,
            tool_status: None,
        };
        let entry = bubble_to_log_entry(&bubble).unwrap();
        match entry {
            crate::claude::LogEntry::Assistant { message, .. } => {
                assert_eq!(
                    crate::claude::extract_text_from_assistant(&message),
                    "Sure, I can help!"
                );
            }
            _ => panic!("Expected Assistant entry"),
        }
    }

    #[test]
    fn test_bubble_to_log_entry_empty_skipped() {
        let bubble = Bubble {
            bubble_type: BUBBLE_TYPE_ASSISTANT,
            text: "".to_string(),
            rich_text: None,
            created_at: Some("2025-01-01T00:00:01Z".to_string()),
            model: None,
            thinking: None,
            thinking_signature: None,
            tool_name: None,
            tool_args: None,
            tool_call_id: None,
            tool_result: None,
            tool_status: None,
        };
        assert!(bubble_to_log_entry(&bubble).is_none());
    }

    #[test]
    fn test_cursor_provider_loads() {
        use crate::providers::Provider;

        let provider = CursorProvider::new();
        if !provider.global_db_path.exists() {
            eprintln!("Cursor DB not found, skipping live test");
            return;
        }

        let convs = provider
            .load_conversations(false, None)
            .expect("Failed to load");

        eprintln!("Loaded {} Cursor conversations", convs.len());
        assert!(!convs.is_empty(), "Expected conversations");

        let with_project = convs.iter().filter(|c| c.project_name.is_some()).count();
        let with_summary = convs.iter().filter(|c| c.summary.is_some()).count();
        eprintln!("{} have project names, {} have titles", with_project, with_summary);

        for conv in convs.iter().take(3) {
            eprintln!(
                "  [{}] {} | {} | {:?}",
                &conv.id[..8],
                conv.project_name.as_deref().unwrap_or("(none)"),
                conv.summary.as_deref().unwrap_or("(no title)"),
                &conv.preview[..conv.preview.len().min(60)]
            );
        }

        // Test reading entries for first conversation
        let entries = provider
            .read_entries(&convs[0])
            .expect("Failed to read entries");
        eprintln!("First conversation has {} log entries", entries.len());
    }
}
