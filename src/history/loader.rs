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
use crate::agent::transcript::content_blocks_count_as_agent_message;
use crate::claude::{LogEntry, extract_search_text_from_user, parse_agent_progress};
use crate::cli::DebugLevel;
use crate::debug;
use crate::error::{AppError, Result};
use crate::time_filter::TimeFilter;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::{File, read_dir};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DeleteEmptyScope {
    All,
    Local,
}

#[derive(Debug, Clone)]
pub struct EmptyTranscript {
    pub path: PathBuf,
    pub session_id: String,
    pub project_name: String,
    pub user_messages: usize,
    pub line_count: usize,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DeleteEmptySummary {
    pub candidates: Vec<EmptyTranscript>,
    pub deleted: usize,
}

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
    time: TimeFilter,
) -> Receiver<LoaderMessage> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        load_all_streaming_inner(tx, show_last, debug_level, time);
    });

    rx
}

fn load_all_streaming_inner(
    tx: Sender<LoaderMessage>,
    show_last: bool,
    debug_level: Option<DebugLevel>,
    time: TimeFilter,
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

                // Filtered here rather than inside load_conversations, whose
                // per-project cache is rebuilt from the vec it returns —
                // dropping conversations earlier would evict their cache
                // entries and force a re-parse on every later run.
                if time.is_active() {
                    convs.retain(|conv| time.matches(conv.timestamp));
                    if convs.is_empty() {
                        return;
                    }
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

pub fn delete_empty_transcripts(
    scope: DeleteEmptyScope,
    delete: bool,
) -> Result<DeleteEmptySummary> {
    let candidates = find_empty_transcripts(scope)?;
    let mut deleted = 0;

    if delete {
        for transcript in &candidates {
            std::fs::remove_file(&transcript.path)?;
            if let Some(project_dir) = transcript.path.parent() {
                let session_dir = project_dir.join(&transcript.session_id);
                if session_dir.is_dir() {
                    std::fs::remove_dir_all(session_dir)?;
                }
            }
            deleted += 1;
        }
    }

    Ok(DeleteEmptySummary {
        candidates,
        deleted,
    })
}

fn find_empty_transcripts(scope: DeleteEmptyScope) -> Result<Vec<EmptyTranscript>> {
    let root = super::get_claude_projects_root()?;
    if !root.exists() {
        return Err(AppError::ProjectsDirNotFound(root.display().to_string()));
    }

    let projects = match scope {
        DeleteEmptyScope::All => list_projects(&root)?,
        DeleteEmptyScope::Local => {
            let current_dir = std::env::current_dir()?;
            let project_dir_name = super::convert_path_to_project_dir_name(&current_dir);
            let project_dir = root.join(&project_dir_name);
            if !project_dir.exists() {
                return Ok(Vec::new());
            }
            vec![Project {
                name: project_dir_name,
                display_name: current_dir.display().to_string(),
                modified: SystemTime::UNIX_EPOCH,
            }]
        }
    };

    let mut candidates: Vec<EmptyTranscript> = projects
        .par_iter()
        .flat_map(|project| {
            let project_dir = root.join(&project.name);
            let entries = match read_dir(project_dir) {
                Ok(entries) => entries,
                Err(_) => return Vec::new(),
            };

            entries
                .filter_map(|entry| {
                    let path = entry.ok()?.path();
                    let filename = path.file_name()?.to_str()?;
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl")
                        || filename.starts_with("agent-")
                    {
                        return None;
                    }

                    empty_transcript_from_path(&path, &project.display_name)
                        .ok()
                        .flatten()
                })
                .collect::<Vec<_>>()
        })
        .collect();

    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(candidates)
}

fn empty_transcript_from_path(path: &Path, project_name: &str) -> Result<Option<EmptyTranscript>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut line_count = 0;
    let mut user_messages = 0;
    let mut assistant_messages = 0;
    let mut preview = None;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        line_count += 1;

        let entry = match serde_json::from_str::<LogEntry>(&line) {
            Ok(entry) => entry,
            Err(_) => continue,
        };

        match entry {
            LogEntry::User { message, .. } => {
                let text = extract_search_text_from_user(&message);
                if !text.trim().is_empty() {
                    user_messages += 1;
                    if preview.is_none() {
                        preview = Some(super::parser::normalize_whitespace(&text));
                    }
                }
            }
            LogEntry::Assistant { message, .. } => {
                if content_blocks_count_as_agent_message(&message.content) {
                    assistant_messages += 1;
                }
            }
            LogEntry::Progress { data, .. } => {
                if let Some(progress) = parse_agent_progress(&data)
                    && progress.message.message_type == "assistant"
                {
                    let crate::claude::AgentContent::Blocks(blocks) =
                        progress.message.message.content;
                    if content_blocks_count_as_agent_message(&blocks) {
                        assistant_messages += 1;
                    }
                }
            }
            _ => {}
        }
    }

    if assistant_messages > 0 {
        return Ok(None);
    }

    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();

    Ok(Some(EmptyTranscript {
        path: path.to_owned(),
        session_id,
        project_name: project_name.to_owned(),
        user_messages,
        line_count,
        preview,
    }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_transcript(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut file = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    #[test]
    fn empty_transcript_detects_user_only_command_session() {
        let file = write_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/status</command-name>"}}"#,
        ]);

        let transcript = empty_transcript_from_path(file.path(), "project")
            .unwrap()
            .expect("user-only transcript should be empty");

        assert_eq!(transcript.user_messages, 1);
        assert_eq!(transcript.line_count, 1);
        assert_eq!(transcript.project_name, "project");
        assert_eq!(
            transcript.preview.as_deref(),
            Some("<command-name>/status</command-name>")
        );
    }

    #[test]
    fn empty_transcript_ignores_transcript_with_assistant_message() {
        let file = write_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"}]}}"#,
        ]);

        let transcript = empty_transcript_from_path(file.path(), "project").unwrap();

        assert!(transcript.is_none());
    }

    #[test]
    fn empty_transcript_includes_metadata_only_file() {
        let file = write_transcript(&[r#"{"type":"summary","summary":"Only metadata"}"#]);

        let transcript = empty_transcript_from_path(file.path(), "project")
            .unwrap()
            .expect("metadata-only transcript should be empty");

        assert_eq!(transcript.user_messages, 0);
        assert_eq!(transcript.line_count, 1);
        assert_eq!(transcript.preview, None);
    }
}
