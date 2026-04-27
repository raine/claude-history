//! Conversation loading and project discovery.
//!
//! This module handles loading conversations from Claude project directories,
//! both synchronously and via streaming for the TUI.

use super::cache;
use super::parser::process_conversation_file;
use super::path::{
    decode_project_dir_name, decode_project_dir_name_to_path, format_short_name_from_path,
};
use super::{Conversation, LoaderMessage, Project};
use crate::claude::HistoryEntry;
use crate::cli::DebugLevel;
use crate::debug;
use crate::error::{AppError, Result};
use crate::tui::search::normalize_for_search;
use chrono::{DateTime, Local, TimeZone};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs::read_dir;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::SystemTime;

/// Load conversations from ALL projects globally
#[allow(dead_code)]
pub fn load_all_conversations(
    show_last: bool,
    debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    let root = super::get_claude_projects_root()?;
    let projects = list_projects(&root)?;

    debug::info(
        debug_level,
        &format!("Loading global history from {} projects", projects.len()),
    );

    // Load conversations from all projects in parallel
    let mut all_conversations: Vec<Conversation> = projects
        .par_iter()
        .flat_map(|project| {
            let project_dir = root.join(&project.name);
            match load_conversations(&project_dir, show_last, &project.name, debug_level) {
                Ok(mut convs) => {
                    // Fallback path for old JSONL files without cwd field
                    let fallback_path = decode_project_dir_name_to_path(&project.name);

                    // Inject project info into each conversation
                    for conv in &mut convs {
                        // Prefer the cwd extracted from the JSONL file (accurate), fall back to decoded path
                        let project_path =
                            conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
                        conv.project_name = Some(format_short_name_from_path(&project_path));
                        conv.project_path = Some(project_path);
                    }
                    convs
                }
                Err(e) => {
                    debug::warn(
                        debug_level,
                        &format!("Failed to load project {}: {}", project.display_name, e),
                    );
                    Vec::new()
                }
            }
        })
        .collect();

    // Global sort by timestamp (newest first)
    all_conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Re-index for fzf selection logic
    for (idx, conv) in all_conversations.iter_mut().enumerate() {
        conv.index = idx;
    }

    debug::info(
        debug_level,
        &format!(
            "Total global conversations loaded: {}",
            all_conversations.len()
        ),
    );

    Ok(all_conversations)
}

/// Start loading all conversations in the background
/// Returns a receiver that will receive LoaderMessage updates
pub fn load_all_conversations_streaming(
    show_last: bool,
    debug_level: Option<DebugLevel>,
) -> Receiver<LoaderMessage> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        load_all_streaming_inner(tx, show_last, debug_level);
    });

    rx
}

fn load_all_streaming_inner(
    tx: Sender<LoaderMessage>,
    show_last: bool,
    debug_level: Option<DebugLevel>,
) {
    // First, validate that the projects root exists (fatal if not)
    let root = match super::get_claude_projects_root() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(LoaderMessage::Fatal(e));
            return;
        }
    };

    if !root.exists() {
        let _ = tx.send(LoaderMessage::Fatal(AppError::ProjectsDirNotFound(
            root.display().to_string(),
        )));
        return;
    }

    // List projects (fatal if this fails)
    let projects = match list_projects(&root) {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(LoaderMessage::Fatal(e));
            return;
        }
    };

    debug::info(
        debug_level,
        &format!("Loading global history from {} projects", projects.len()),
    );

    // Process projects in parallel and send batches as they complete
    projects.par_iter().for_each(|project| {
        let project_dir = root.join(&project.name);

        match load_conversations(&project_dir, show_last, &project.name, debug_level) {
            Ok(mut convs) => {
                if convs.is_empty() {
                    return;
                }

                let fallback_path = decode_project_dir_name_to_path(&project.name);

                for conv in &mut convs {
                    let project_path = conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
                    conv.project_name = Some(format_short_name_from_path(&project_path));
                    conv.project_path = Some(project_path);
                }

                // Send batch, ignore error if receiver dropped
                let _ = tx.send(LoaderMessage::Batch(convs));
            }
            Err(e) => {
                debug::warn(
                    debug_level,
                    &format!("Failed to load project {}: {}", project.display_name, e),
                );
                let _ = tx.send(LoaderMessage::ProjectError);
            }
        }
    });

    let _ = tx.send(LoaderMessage::Done);
}

/// Find a session JSONL file by UUID across all projects.
/// Returns the path to the `.jsonl` file if found.
pub fn find_jsonl_by_uuid(uuid: &str) -> Result<Option<PathBuf>> {
    let matches = find_all_jsonl_by_uuid(uuid)?;
    Ok(matches.into_iter().next())
}

/// Find all session JSONL files by UUID across all projects.
/// A session may exist in multiple project directories due to cross-project forking.
fn find_all_jsonl_by_uuid(uuid: &str) -> Result<Vec<PathBuf>> {
    let root = super::get_claude_projects_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }

    let filename = format!("{}.jsonl", uuid);
    let mut matches = Vec::new();

    for entry in read_dir(&root)? {
        let entry = entry?;
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        let candidate = project_dir.join(&filename);
        if candidate.exists() {
            matches.push(candidate);
        }
    }

    Ok(matches)
}

/// Delete a session by UUID across all projects.
/// Removes both the .jsonl file and the session subdirectory (tool-results/, subagents/).
/// Returns the number of files deleted.
pub fn delete_session_by_uuid(uuid: &str) -> Result<usize> {
    // Validate format to prevent path traversal
    if uuid.is_empty() || !uuid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(AppError::SessionNotFound(uuid.to_owned()));
    }

    let matches = find_all_jsonl_by_uuid(uuid)?;
    if matches.is_empty() {
        return Err(AppError::SessionNotFound(uuid.to_owned()));
    }

    let count = matches.len();
    for jsonl_path in &matches {
        std::fs::remove_file(jsonl_path)?;

        // Also remove the session subdirectory if it exists
        if let Some(project_dir) = jsonl_path.parent() {
            let session_dir = project_dir.join(uuid);
            if session_dir.is_dir() {
                std::fs::remove_dir_all(&session_dir)?;
            }
        }
    }

    Ok(count)
}

/// List all projects that contain conversation files
pub fn list_projects(root: &Path) -> Result<Vec<Project>> {
    let entries = read_dir(root)?;

    let mut projects: Vec<Project> = entries
        .par_bridge()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();

            if !path.is_dir() {
                return None;
            }

            // Check if project has any non-agent .jsonl files
            let has_conversations = read_dir(&path).ok()?.any(|e| {
                e.ok()
                    .map(|e| {
                        let path = e.path();
                        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                        path.extension().map(|s| s == "jsonl").unwrap_or(false)
                            && !name.starts_with("agent-")
                    })
                    .unwrap_or(false)
            });

            if !has_conversations {
                return None;
            }

            let name = path.file_name()?.to_string_lossy().to_string();
            // Heuristic decode: convert encoded directory name back to readable path
            // The encoding replaces non-alphanumeric chars (except -) with -
            // So / becomes -, but _ also becomes -, and __ becomes --
            // We convert single dashes to / but preserve double dashes as _
            let display_name = decode_project_dir_name(&name);
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);

            Some(Project {
                name,
                display_name,
                modified,
            })
        })
        .collect();

    // Sort by recently modified
    projects.sort_by(|a, b| b.modified.cmp(&a.modified));

    Ok(projects)
}

/// Find and process all conversation files in one pass, using per-project cache
pub fn load_conversations(
    projects_dir: &Path,
    show_last: bool,
    project_dir_name: &str,
    debug_level: Option<DebugLevel>,
) -> Result<Vec<Conversation>> {
    // Load existing cache for this project
    let cached_entries = cache::read_project_cache(project_dir_name).unwrap_or_default();

    // Find all JSONL files and capture metadata in one pass
    let mut files_with_meta = Vec::new();
    let mut skipped_agent_files = 0;

    for entry in read_dir(projects_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            if let Some(filename) = path.file_name().and_then(|f| f.to_str())
                && filename.starts_with("agent-")
            {
                skipped_agent_files += 1;
                debug::debug(debug_level, &format!("Skipping agent file: {}", filename));
                continue;
            }

            let metadata = entry.metadata().ok();
            let modified = metadata.as_ref().and_then(|m| m.modified().ok());
            let file_size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);

            files_with_meta.push((path, modified, file_size));
        }
    }

    debug::info(
        debug_level,
        &format!(
            "Found {} conversation files ({} agent files skipped)",
            files_with_meta.len(),
            skipped_agent_files
        ),
    );

    // Sort by modification time (newest first)
    files_with_meta.sort_by_key(|(_, modified, _)| modified.unwrap_or(SystemTime::UNIX_EPOCH));
    files_with_meta.reverse();

    // Partition into cache hits and misses
    let mut dirty = false;
    let mut conversations: Vec<Conversation> = Vec::with_capacity(files_with_meta.len());
    let mut files_to_parse: Vec<(PathBuf, Option<SystemTime>, u64)> = Vec::new();

    for (path, modified, file_size) in &files_with_meta {
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown");

        if let Some(mtime) = modified
            && let Some(entry) = cached_entries.get(filename)
            && cache::entry_matches(entry, *file_size, *mtime)
        {
            if entry.is_empty {
                // Negative cache hit — file was previously parsed and yielded nothing
                debug::debug(debug_level, &format!("Cache hit (empty) {}", filename));
            } else {
                let conv = cache::conversation_from_entry(entry, path.clone(), show_last);
                debug::debug(
                    debug_level,
                    &format!("Cache hit {}: {}", filename, conv.preview),
                );
                conversations.push(conv);
            }
        } else {
            dirty = true;
            files_to_parse.push((path.clone(), *modified, *file_size));
        }
    }

    if !dirty && files_with_meta.len() != cached_entries.len() {
        // Files were deleted — need to rewrite cache to remove stale entries
        dirty = true;
    }

    debug::info(
        debug_level,
        &format!(
            "Cache: {} hits, {} misses",
            conversations.len(),
            files_to_parse.len()
        ),
    );

    // Parse only cache misses in parallel
    // Returns (Option<Conversation>, filename, file_size, mtime) — None for empty/filtered files
    let parse_results: Vec<(Option<Conversation>, String, u64, Option<SystemTime>)> =
        files_to_parse
            .into_par_iter()
            .map(|(path, modified, file_size)| {
                let filename = path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("unknown")
                    .to_owned();

                match process_conversation_file(path, modified, debug_level) {
                    Ok(Some(mut conversation)) => {
                        conversation.preview = if show_last {
                            conversation.preview_last.clone()
                        } else {
                            conversation.preview_first.clone()
                        };
                        debug::debug(
                            debug_level,
                            &format!("Parsed {}: {}", filename, conversation.preview),
                        );
                        (Some(conversation), filename, file_size, modified)
                    }
                    Ok(None) => (None, filename, file_size, modified),
                    Err(e) => {
                        debug::warn(
                            debug_level,
                            &format!("Error processing {}: {}", filename, e),
                        );
                        (None, filename, file_size, modified)
                    }
                }
            })
            .collect();

    // Separate conversations from empty results (for negative caching)
    for (conv, _, _, _) in &parse_results {
        if let Some(conv) = conv {
            conversations.push(conv.clone());
        }
    }

    // Ensure deterministic ordering after parallel processing
    conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    // Inject project info into each conversation
    let fallback_path = projects_dir
        .file_name()
        .map(|n| decode_project_dir_name_to_path(&n.to_string_lossy()))
        .unwrap_or_default();

    for (idx, conv) in conversations.iter_mut().enumerate() {
        conv.index = idx;

        // Prefer the cwd extracted from the JSONL file, fall back to decoded path
        let project_path = conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
        conv.project_name = Some(format_short_name_from_path(&project_path));
        conv.project_path = Some(project_path);
    }

    // Write updated cache if anything changed
    if dirty {
        let mut new_cache: HashMap<String, cache::CacheEntry> = HashMap::new();

        // Add existing conversations (both cache hits and fresh parses)
        for conv in &conversations {
            let filename = conv
                .path
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("unknown");

            if let Some((_, modified, file_size)) = files_with_meta
                .iter()
                .find(|(p, _, _)| p.file_name() == conv.path.file_name())
                && let Some(mtime) = modified
            {
                new_cache.insert(
                    filename.to_owned(),
                    cache::entry_from_conversation(conv, *file_size, *mtime),
                );
            }
        }

        // Add negative cache entries for files that parsed to nothing
        for (conv, filename, file_size, modified) in &parse_results {
            if conv.is_none()
                && let Some(mtime) = modified
            {
                new_cache.insert(filename.to_owned(), cache::empty_entry(*file_size, *mtime));
            }
        }

        cache::write_project_cache(project_dir_name, new_cache);
    }

    debug::info(
        debug_level,
        &format!("Total conversations loaded: {}", conversations.len()),
    );

    Ok(conversations)
}

/// Load orphan conversations from history.jsonl.
///
/// Reads the global prompt log, groups entries by sessionId,
/// excludes sessions that already exist in the provided set of known IDs,
/// and returns thin Conversation objects for the orphans.
pub fn load_orphan_conversations(known_session_ids: &HashSet<String>) -> Result<Vec<Conversation>> {
    let history_path = super::get_history_jsonl_path()?;
    if !history_path.exists() {
        return Ok(Vec::new());
    }

    let file = std::fs::File::open(&history_path)?;
    let reader = std::io::BufReader::new(file);

    // Group entries by sessionId
    let mut sessions: HashMap<String, Vec<HistoryEntry>> = HashMap::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(trimmed) {
            Ok(entry) => {
                if !known_session_ids.contains(&entry.session_id) {
                    sessions
                        .entry(entry.session_id.clone())
                        .or_default()
                        .push(entry);
                }
            }
            Err(_) => continue,
        }
    }

    let mut conversations: Vec<Conversation> = sessions
        .into_values()
        .map(|mut entries| {
            // Sort by timestamp ascending
            entries.sort_by_key(|e| e.timestamp);

            let last_ts = entries.last().map(|e| e.timestamp).unwrap_or(0);
            let timestamp: DateTime<Local> = Local
                .timestamp_millis_opt(last_ts as i64)
                .single()
                .unwrap_or_else(Local::now);

            let project_path_str = &entries[0].project;
            let project_path = PathBuf::from(project_path_str);
            let project_name = project_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| format_short_name_from_path(&project_path));

            let prompts: Vec<&str> = entries.iter().map(|e| e.display.as_str()).collect();
            let all_prompts = prompts.join("\n");
            let search_text_lower = normalize_for_search(&format!(
                "{} {}",
                project_name, all_prompts
            ));

            Conversation {
                path: PathBuf::new(), // empty = orphan sentinel
                index: 0,
                timestamp,
                preview: all_prompts.clone(),
                preview_first: all_prompts.clone(),
                preview_last: all_prompts.clone(),
                full_text: all_prompts,
                search_text_lower,
                project_name: Some(project_name),
                project_path: Some(project_path),
                cwd: None,
                message_count: entries.len(),
                parse_errors: vec![],
                summary: None,
                custom_title: None,
                model: None,
                total_tokens: 0,
                duration_minutes: None,
            }
        })
        .collect();

    // Sort by timestamp (newest first)
    conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    Ok(conversations)
}
