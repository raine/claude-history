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
use crate::ccs::{self, CcsInfo};
use crate::claude::{LogEntry, extract_search_text_from_user, parse_agent_progress};
use crate::cli::DebugLevel;
use crate::debug;
use crate::error::{AppError, Result};
use crate::time_filter::TimeFilter;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
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

/// First-line signature of ICM (persistent-memory) background sessions. These are
/// machine-generated `claude -p` jobs spawned by ICM hooks on nearly every tool
/// call to distill memory; each embeds the full system context (~85 KB) and they
/// can number in the hundreds of thousands, dwarfing real conversations. We skip
/// them up front so they never get parsed, cached, or held in memory.
pub const ICM_SESSION_MARKER: &str =
    "extract durable facts that an AI agent should remember across sessions";

/// Read the first chunk of a file and report whether it contains any of the given
/// marker substrings. Reads only a small head (markers appear within the first
/// queue-operation line), so this is far cheaper than parsing the whole file.
fn head_contains_marker(path: &Path, markers: &[String]) -> bool {
    use std::io::Read;
    if markers.is_empty() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return false;
    }
    let head = &buf[..n];
    markers.iter().any(|m| {
        let needle = m.as_bytes();
        !needle.is_empty()
            && needle.len() <= head.len()
            && head.windows(needle.len()).any(|w| w == needle)
    })
}

/// Load conversations from ALL projects globally (across all CCS roots)
#[allow(dead_code)]
pub fn load_all_conversations(
    show_last: bool,
    debug_level: Option<DebugLevel>,
    ccs_info: Option<&CcsInfo>,
    exclude_markers: &[String],
) -> Result<Vec<Conversation>> {
    let roots = ccs::get_all_project_roots(ccs_info)?;

    // Build session UUID → profile name mapping from CCS session-env directories
    let session_profile_map = ccs_info
        .map(|info| info.build_session_profile_map())
        .unwrap_or_default();

    // Collect all projects from all roots, deduplicating by canonical project dir
    let mut seen_canonical = HashSet::new();
    let mut all_projects: Vec<(PathBuf, Project, String)> = Vec::new();

    for root in &roots {
        if !root.path.exists() {
            continue;
        }
        if let Ok(projects) = list_projects(&root.path) {
            for project in projects {
                let project_dir = root.path.join(&project.name);
                let canonical =
                    std::fs::canonicalize(&project_dir).unwrap_or_else(|_| project_dir.clone());
                if seen_canonical.insert(canonical) {
                    all_projects.push((root.path.clone(), project, root.cache_key.clone()));
                }
            }
        }
    }

    debug::info(
        debug_level,
        &format!(
            "Loading global history from {} projects across {} roots",
            all_projects.len(),
            roots.len()
        ),
    );

    // Load conversations from all projects in parallel
    let mut all_conversations: Vec<Conversation> = all_projects
        .par_iter()
        .flat_map(|(root, project, cache_key)| {
            let project_dir = root.join(&project.name);
            match load_conversations_keyed(
                &project_dir,
                show_last,
                &project.name,
                debug_level,
                cache_key,
                exclude_markers,
            ) {
                Ok(mut convs) => {
                    let fallback_path = decode_project_dir_name_to_path(&project.name);
                    for conv in &mut convs {
                        let project_path =
                            conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
                        conv.project_name = Some(format_short_name_from_path(&project_path));
                        conv.project_path = Some(project_path);
                        // Look up session UUID in CCS session-env mapping
                        if let Some(uuid) = conv.path.file_stem().and_then(|s| s.to_str()) {
                            conv.source_label = session_profile_map.get(uuid).cloned();
                        }
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

/// Start loading all conversations in the background (across all CCS roots)
/// Returns a receiver that will receive LoaderMessage updates
pub fn load_all_conversations_streaming(
    show_last: bool,
    debug_level: Option<DebugLevel>,
    time: TimeFilter,
    ccs_info: Option<CcsInfo>,
    exclude_markers: Vec<String>,
) -> Receiver<LoaderMessage> {
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        load_all_streaming_inner(
            tx,
            show_last,
            debug_level,
            time,
            ccs_info.as_ref(),
            &exclude_markers,
        );
    });

    rx
}

fn load_all_streaming_inner(
    tx: Sender<LoaderMessage>,
    show_last: bool,
    debug_level: Option<DebugLevel>,
    time: TimeFilter,
    ccs_info: Option<&CcsInfo>,
    exclude_markers: &[String],
) {
    let roots = match ccs::get_all_project_roots(ccs_info) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(LoaderMessage::Fatal(e));
            return;
        }
    };

    // Build session UUID → profile name mapping from CCS session-env directories
    let session_profile_map = ccs_info
        .map(|info| info.build_session_profile_map())
        .unwrap_or_default();

    // Collect all projects from all roots, deduplicating by canonical project dir
    let mut seen_canonical = HashSet::new();
    let mut all_projects: Vec<(PathBuf, Project, String)> = Vec::new();

    for root in &roots {
        if !root.path.exists() {
            debug::warn(
                debug_level,
                &format!("Projects root does not exist: {}", root.path.display()),
            );
            continue;
        }

        match list_projects(&root.path) {
            Ok(projects) => {
                for project in projects {
                    let project_dir = root.path.join(&project.name);
                    let canonical =
                        std::fs::canonicalize(&project_dir).unwrap_or_else(|_| project_dir.clone());
                    if seen_canonical.insert(canonical) {
                        all_projects.push((root.path.clone(), project, root.cache_key.clone()));
                    }
                }
            }
            Err(e) => {
                debug::warn(
                    debug_level,
                    &format!("Failed to list projects in {}: {}", root.path.display(), e),
                );
            }
        }
    }

    if all_projects.is_empty() {
        let root_paths: Vec<String> = roots.iter().map(|r| r.path.display().to_string()).collect();
        let _ = tx.send(LoaderMessage::Fatal(AppError::ProjectsDirNotFound(
            root_paths.join(", "),
        )));
        return;
    }

    debug::info(
        debug_level,
        &format!(
            "Loading global history from {} projects across {} roots",
            all_projects.len(),
            roots.len()
        ),
    );

    // Process projects in parallel and send batches as they complete
    all_projects
        .par_iter()
        .for_each(|(root, project, cache_key)| {
            let project_dir = root.join(&project.name);

            match load_conversations_keyed(
                &project_dir,
                show_last,
                &project.name,
                debug_level,
                cache_key,
                exclude_markers,
            ) {
                Ok(mut convs) => {
                    if convs.is_empty() {
                        return;
                    }

                    let fallback_path = decode_project_dir_name_to_path(&project.name);

                    for conv in &mut convs {
                        let project_path =
                            conv.cwd.clone().unwrap_or_else(|| fallback_path.clone());
                        conv.project_name = Some(format_short_name_from_path(&project_path));
                        conv.project_path = Some(project_path);
                        if let Some(uuid) = conv.path.file_stem().and_then(|s| s.to_str()) {
                            conv.source_label = session_profile_map.get(uuid).cloned();
                        }
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

/// Find a session JSONL file by UUID across all projects and all roots.
/// Returns the path to the `.jsonl` file if found.
pub fn find_jsonl_by_uuid(uuid: &str) -> Result<Option<PathBuf>> {
    let matches = find_all_jsonl_by_uuid(uuid)?;
    Ok(matches.into_iter().next())
}

/// Find all session JSONL files by UUID across all projects and all roots.
/// A session may exist in multiple project directories due to cross-project forking.
fn find_all_jsonl_by_uuid(uuid: &str) -> Result<Vec<PathBuf>> {
    let ccs_info = ccs::discover_ccs();
    let roots = ccs::get_all_project_roots(ccs_info.as_ref())?;
    let filename = format!("{}.jsonl", uuid);
    let mut matches = Vec::new();
    let mut seen_canonical = HashSet::new();

    for root in &roots {
        if !root.path.exists() {
            continue;
        }
        let Ok(entries) = read_dir(&root.path) else {
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let candidate = project_dir.join(&filename);
            if candidate.exists() {
                let canonical =
                    std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
                if seen_canonical.insert(canonical) {
                    matches.push(candidate);
                }
            }
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
#[allow(dead_code)]
pub fn load_conversations(
    projects_dir: &Path,
    show_last: bool,
    project_dir_name: &str,
    debug_level: Option<DebugLevel>,
    exclude_markers: &[String],
) -> Result<Vec<Conversation>> {
    load_conversations_keyed(
        projects_dir,
        show_last,
        project_dir_name,
        debug_level,
        "default",
        exclude_markers,
    )
}

/// Find and process all conversation files in one pass, with explicit cache key
fn load_conversations_keyed(
    projects_dir: &Path,
    show_last: bool,
    project_dir_name: &str,
    debug_level: Option<DebugLevel>,
    cache_key: &str,
    exclude_markers: &[String],
) -> Result<Vec<Conversation>> {
    // Load existing cache for this project
    let cached_entries =
        cache::read_project_cache_keyed(project_dir_name, Some(cache_key)).unwrap_or_default();

    // Find all JSONL files and capture metadata in one pass
    let mut files_with_meta = Vec::new();
    let mut skipped_agent_files = 0;
    let mut skipped_excluded_files = 0;

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

            // Skip machine-generated sessions (e.g. ICM memory jobs) identified by a
            // marker in their head, before parsing/caching/holding them in memory.
            if head_contains_marker(&path, exclude_markers) {
                skipped_excluded_files += 1;
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
            "Found {} conversation files ({} agent files skipped, {} excluded by marker)",
            files_with_meta.len(),
            skipped_agent_files,
            skipped_excluded_files
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

        cache::write_project_cache_keyed(project_dir_name, new_cache, Some(cache_key));
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
