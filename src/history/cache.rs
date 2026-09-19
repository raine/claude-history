//! Per-project binary cache for parsed conversation metadata.
//!
//! Stores parsed conversation data in bincode format, keyed by session filename
//! and validated by mtime + file size. Eliminates redundant JSONL parsing and
//! search text normalization on startup for unchanged files.

use super::{Conversation, ParseError};
use crate::history::MessageRange;
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_MAGIC: [u8; 8] = *b"CLHIST01";
const PI_CACHE_MAGIC: [u8; 8] = *b"PIHIST01";
const OMP_CACHE_MAGIC: [u8; 8] = *b"OMHIST01";
const SCHEMA_VERSION: u32 = 13;
const PI_SCHEMA_VERSION: u32 = 3;
const OMP_SCHEMA_VERSION: u32 = 3;

#[derive(Serialize, Deserialize)]
struct PiCache {
    magic: [u8; 8],
    schema_version: u32,
    entries: HashMap<String, PiCacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PiCacheEntry {
    pub metadata: CacheEntry,
    pub session_id: String,
    pub project_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct ProjectCache {
    magic: [u8; 8],
    schema_version: u32,
    entries: HashMap<String, CacheEntry>,
}

/// Cached conversation data — a dedicated DTO separate from Conversation
/// to avoid schema churn from UI/runtime field changes.
#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub file_size: u64,
    pub mtime_secs: u64,
    pub mtime_nsecs: u32,
    /// If true, this file was parsed but yielded no conversation (empty/clear-only).
    /// Avoids re-parsing known-empty files on every startup.
    #[serde(default)]
    pub is_empty: bool,
    pub preview_first: String,
    pub preview_last: String,
    pub full_text: String,
    #[serde(default)]
    pub agent_search_text: String,
    pub semantic_route_text: String,
    #[serde(default)]
    pub semantic_turns: Vec<String>,
    #[serde(default)]
    pub semantic_turn_ranges: Vec<MessageRange>,
    pub search_text_lower: String,
    pub dialogue_text_lower: String,
    pub cwd: Option<PathBuf>,
    pub message_count: usize,
    pub parse_errors: Vec<CachedParseError>,
    pub summary: Option<String>,
    pub custom_title: Option<String>,
    pub model: Option<String>,
    pub total_tokens: u64,
    pub duration_minutes: Option<u64>,
    pub timestamp_epoch_ms: i64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CachedParseError {
    pub line_number: usize,
    pub line_content: String,
    pub error_message: String,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Get the cache directory for per-project cache files.
/// Respects CLAUDE_CONFIG_DIR to namespace caches per config root.
fn cache_dir() -> Option<PathBuf> {
    let base = home::home_dir()?.join(".cache").join("claude-history");
    if let Ok(config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        // Namespace by config dir to avoid cross-config cache collisions
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&config_dir, &mut hasher);
        let hash = std::hash::Hasher::finish(&hasher);
        Some(base.join(format!("config-{:016x}", hash)).join("projects"))
    } else {
        Some(base.join("projects"))
    }
}

/// Get the cache file path for a specific project
fn cache_path_for_project(project_dir_name: &str) -> Option<PathBuf> {
    cache_dir().map(|d| d.join(format!("{}.bin", project_dir_name)))
}

/// Read a project's cache file, returning entries keyed by session filename.
/// Returns None on any failure (missing, corrupt, version mismatch).
pub fn read_project_cache(project_dir_name: &str) -> Option<HashMap<String, CacheEntry>> {
    let path = cache_path_for_project(project_dir_name)?;
    let data = std::fs::read(&path).ok()?;
    if data.len() < 12 {
        return None;
    }
    if data[..8] != CACHE_MAGIC {
        return None;
    }
    let cache: ProjectCache = bincode::deserialize(&data).ok()?;
    if cache.schema_version != SCHEMA_VERSION {
        return None;
    }
    Some(cache.entries)
}

/// Write a project's cache file atomically (temp file + rename).
/// Uses tempfile for safe concurrent writes. Silently ignores failures.
pub fn write_project_cache(project_dir_name: &str, entries: HashMap<String, CacheEntry>) {
    let Some(path) = cache_path_for_project(project_dir_name) else {
        return;
    };
    write_cache_file(
        &path,
        &ProjectCache {
            magic: CACHE_MAGIC,
            schema_version: SCHEMA_VERSION,
            entries,
        },
    );
}

fn write_cache_file(path: &std::path::Path, cache: &impl Serialize) {
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let Ok(data) = bincode::serialize(cache) else {
        return;
    };
    let Ok(mut tmp) = tempfile::NamedTempFile::new_in(parent) else {
        return;
    };
    if tmp.write_all(&data).is_err() {
        return;
    }
    let _ = tmp.persist(path);
}

fn source_cache_path(root: &std::path::Path, source: &str) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};

    let resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    resolved.hash(&mut hasher);
    Some(
        home::home_dir()?
            .join(".cache")
            .join("claude-history")
            .join(source)
            .join(format!("root-{:016x}", hasher.finish()))
            .join("sessions.bin"),
    )
}

pub fn pi_cache_path(root: &std::path::Path) -> Option<PathBuf> {
    source_cache_path(root, "pi")
}

pub fn omp_cache_path(root: &std::path::Path) -> Option<PathBuf> {
    source_cache_path(root, "omp")
}

pub fn read_pi_cache(root: &std::path::Path) -> Option<HashMap<String, PiCacheEntry>> {
    let data = std::fs::read(pi_cache_path(root)?).ok()?;
    let cache: PiCache = bincode::deserialize(&data).ok()?;
    if cache.magic != PI_CACHE_MAGIC || cache.schema_version != PI_SCHEMA_VERSION {
        return None;
    }
    Some(cache.entries)
}

pub fn write_pi_cache(root: &std::path::Path, entries: HashMap<String, PiCacheEntry>) {
    let Some(path) = pi_cache_path(root) else {
        return;
    };
    write_cache_file(
        &path,
        &PiCache {
            magic: PI_CACHE_MAGIC,
            schema_version: PI_SCHEMA_VERSION,
            entries,
        },
    );
}

pub fn read_omp_cache(root: &std::path::Path) -> Option<HashMap<String, PiCacheEntry>> {
    let data = std::fs::read(omp_cache_path(root)?).ok()?;
    let cache: PiCache = bincode::deserialize(&data).ok()?;
    if cache.magic != OMP_CACHE_MAGIC || cache.schema_version != OMP_SCHEMA_VERSION {
        return None;
    }
    Some(cache.entries)
}

pub fn write_omp_cache(root: &std::path::Path, entries: HashMap<String, PiCacheEntry>) {
    let Some(path) = omp_cache_path(root) else {
        return;
    };
    write_cache_file(
        &path,
        &PiCache {
            magic: OMP_CACHE_MAGIC,
            schema_version: OMP_SCHEMA_VERSION,
            entries,
        },
    );
}

/// Create a negative cache entry for files that parsed to no conversation
pub fn empty_entry(file_size: u64, mtime: SystemTime) -> CacheEntry {
    let duration_since_epoch = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
    CacheEntry {
        file_size,
        mtime_secs: duration_since_epoch.as_secs(),
        mtime_nsecs: duration_since_epoch.subsec_nanos(),
        is_empty: true,
        preview_first: String::new(),
        preview_last: String::new(),
        full_text: String::new(),
        agent_search_text: String::new(),
        semantic_route_text: String::new(),
        semantic_turns: Vec::new(),
        semantic_turn_ranges: Vec::new(),
        search_text_lower: String::new(),
        dialogue_text_lower: String::new(),
        cwd: None,
        message_count: 0,
        parse_errors: Vec::new(),
        summary: None,
        custom_title: None,
        model: None,
        total_tokens: 0,
        duration_minutes: None,
        timestamp_epoch_ms: 0,
    }
}

/// Create a CacheEntry from a parsed Conversation
pub fn entry_from_conversation(
    conv: &Conversation,
    file_size: u64,
    mtime: SystemTime,
) -> CacheEntry {
    let duration_since_epoch = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
    CacheEntry {
        file_size,
        mtime_secs: duration_since_epoch.as_secs(),
        mtime_nsecs: duration_since_epoch.subsec_nanos(),
        is_empty: false,
        preview_first: conv.preview_first.clone(),
        preview_last: conv.preview_last.clone(),
        full_text: conv.full_text.clone(),
        agent_search_text: conv.agent_search_text.clone(),
        semantic_route_text: conv.semantic_route_text.clone(),
        semantic_turns: conv.semantic_turns.clone(),
        semantic_turn_ranges: conv.semantic_turn_ranges.clone(),
        search_text_lower: conv.search_text_lower.clone(),
        dialogue_text_lower: conv.dialogue_text_lower.clone(),
        cwd: conv.cwd.clone(),
        message_count: conv.message_count,
        parse_errors: conv
            .parse_errors
            .iter()
            .map(|e| CachedParseError {
                line_number: e.line_number,
                line_content: e.line_content.clone(),
                error_message: e.error_message.clone(),
                context_before: e.context_before.clone(),
                context_after: e.context_after.clone(),
            })
            .collect(),
        summary: conv.summary.clone(),
        custom_title: conv.custom_title.clone(),
        model: conv.model.clone(),
        total_tokens: conv.total_tokens,
        duration_minutes: conv.duration_minutes,
        timestamp_epoch_ms: conv.timestamp.timestamp_millis(),
    }
}

/// Reconstruct a Conversation from a CacheEntry
pub fn conversation_from_entry(entry: &CacheEntry, path: PathBuf, show_last: bool) -> Conversation {
    let timestamp = Local
        .timestamp_millis_opt(entry.timestamp_epoch_ms)
        .single()
        .unwrap_or_else(Local::now);
    let preview = if show_last {
        entry.preview_last.clone()
    } else {
        entry.preview_first.clone()
    };
    Conversation {
        source: super::Source::Claude,
        session_id: path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned(),
        path,
        index: 0,
        timestamp,
        preview,
        preview_first: entry.preview_first.clone(),
        preview_last: entry.preview_last.clone(),
        full_text: entry.full_text.clone(),
        agent_search_text: entry.agent_search_text.clone(),
        semantic_route_text: entry.semantic_route_text.clone(),
        semantic_turns: entry.semantic_turns.clone(),
        semantic_turn_ranges: entry.semantic_turn_ranges.clone(),
        search_text_lower: entry.search_text_lower.clone(),
        dialogue_text_lower: entry.dialogue_text_lower.clone(),
        project_name: None,
        project_path: None,
        cwd: entry.cwd.clone(),
        message_count: entry.message_count,
        parse_errors: entry
            .parse_errors
            .iter()
            .map(|e| ParseError {
                line_number: e.line_number,
                line_content: e.line_content.clone(),
                error_message: e.error_message.clone(),
                context_before: e.context_before.clone(),
                context_after: e.context_after.clone(),
            })
            .collect(),
        summary: entry.summary.clone(),
        custom_title: entry.custom_title.clone(),
        model: entry.model.clone(),
        total_tokens: entry.total_tokens,
        duration_minutes: entry.duration_minutes,
    }
}

/// Check if a CacheEntry matches the given file metadata
pub fn entry_matches(entry: &CacheEntry, file_size: u64, mtime: SystemTime) -> bool {
    let duration_since_epoch = mtime.duration_since(UNIX_EPOCH).unwrap_or_default();
    entry.file_size == file_size
        && entry.mtime_secs == duration_since_epoch.as_secs()
        && entry.mtime_nsecs == duration_since_epoch.subsec_nanos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::normalize_for_search;
    use std::time::Duration;

    fn make_test_conversation() -> Conversation {
        let timestamp = Local::now();
        Conversation {
            source: crate::history::Source::Claude,
            session_id: "conv".to_owned(),
            path: PathBuf::from("/test/conv.jsonl"),
            index: 0,
            timestamp,
            preview: "Hello world ... Hi there".to_string(),
            preview_first: "Hello world ... Hi there".to_string(),
            preview_last: "Hi there ... Hello world".to_string(),
            full_text: "Hello world Hi there".to_string(),
            agent_search_text: "subagent cache text".to_string(),
            semantic_route_text: "semantic route text".to_string(),
            semantic_turns: vec!["Hello world".to_string(), "Hi there".to_string()],
            semantic_turn_ranges: vec![MessageRange::single(1), MessageRange::single(2)],
            search_text_lower: normalize_for_search("Hello world Hi there"),
            dialogue_text_lower: normalize_for_search("Hello world Hi there"),
            project_name: Some("test-project".to_string()),
            project_path: Some(PathBuf::from("/test/project")),
            cwd: Some(PathBuf::from("/test/cwd")),
            message_count: 2,
            parse_errors: vec![],
            summary: Some("Test summary".to_string()),
            custom_title: Some("My Session".to_string()),
            model: Some("claude-opus-4-5-20251101".to_string()),
            total_tokens: 1500,
            duration_minutes: Some(10),
        }
    }

    #[test]
    fn pi_cache_roots_are_isolated_from_claude_and_each_other() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let first_path = pi_cache_path(first.path()).unwrap();
        let second_path = pi_cache_path(second.path()).unwrap();
        let claude_path = cache_path_for_project("same-project").unwrap();
        let omp_path = omp_cache_path(first.path()).unwrap();

        assert_ne!(first_path, second_path);
        assert_ne!(first_path, claude_path);
        assert_ne!(first_path, omp_path);
        assert!(first_path.to_string_lossy().contains("/pi/root-"));
        assert!(omp_path.to_string_lossy().contains("/omp/root-"));
        assert!(claude_path.to_string_lossy().contains("/projects/"));
    }

    #[test]
    fn roundtrip_entry_preserves_data() {
        let conv = make_test_conversation();
        let mtime = UNIX_EPOCH + Duration::from_secs(1700000000) + Duration::from_nanos(123456789);
        let file_size = 42000;

        let entry = entry_from_conversation(&conv, file_size, mtime);

        // Verify entry_matches works
        assert!(entry_matches(&entry, file_size, mtime));
        assert!(!entry_matches(&entry, file_size + 1, mtime));
        assert!(!entry_matches(
            &entry,
            file_size,
            mtime + Duration::from_secs(1)
        ));

        // Roundtrip back to Conversation
        let restored = conversation_from_entry(&entry, PathBuf::from("/test/conv.jsonl"), false);

        assert_eq!(restored.preview, conv.preview_first);
        assert_eq!(restored.preview_first, conv.preview_first);
        assert_eq!(restored.preview_last, conv.preview_last);
        assert_eq!(restored.full_text, conv.full_text);
        assert_eq!(restored.agent_search_text, conv.agent_search_text);
        assert_eq!(restored.semantic_turns, conv.semantic_turns);
        assert_eq!(restored.semantic_turn_ranges, conv.semantic_turn_ranges);
        assert_eq!(restored.search_text_lower, conv.search_text_lower);
        assert_eq!(restored.dialogue_text_lower, conv.dialogue_text_lower);
        assert_eq!(restored.cwd, conv.cwd);
        assert_eq!(restored.message_count, conv.message_count);
        assert_eq!(restored.summary, conv.summary);
        assert_eq!(restored.custom_title, conv.custom_title);
        assert_eq!(restored.model, conv.model);
        assert_eq!(restored.total_tokens, conv.total_tokens);
        assert_eq!(restored.duration_minutes, conv.duration_minutes);
        // Timestamp roundtrips through milliseconds
        assert_eq!(
            restored.timestamp.timestamp_millis(),
            conv.timestamp.timestamp_millis()
        );
    }

    #[test]
    fn show_last_selects_correct_preview() {
        let conv = make_test_conversation();
        let mtime = UNIX_EPOCH + Duration::from_secs(1700000000);
        let entry = entry_from_conversation(&conv, 100, mtime);

        let first = conversation_from_entry(&entry, PathBuf::new(), false);
        assert_eq!(first.preview, "Hello world ... Hi there");

        let last = conversation_from_entry(&entry, PathBuf::new(), true);
        assert_eq!(last.preview, "Hi there ... Hello world");
    }

    #[test]
    fn empty_entry_roundtrips() {
        let mtime = UNIX_EPOCH + Duration::from_secs(1700000000);
        let entry = empty_entry(500, mtime);

        assert!(entry.is_empty);
        assert!(entry_matches(&entry, 500, mtime));
        assert!(!entry_matches(&entry, 501, mtime));
    }

    #[test]
    fn cache_file_roundtrip() {
        // Use a unique project name to avoid test interference
        let project_name = format!("test-cache-roundtrip-{}", std::process::id());

        let conv = make_test_conversation();
        let mtime = UNIX_EPOCH + Duration::from_secs(1700000000);
        let mut entries = HashMap::new();
        entries.insert(
            "conv1.jsonl".to_string(),
            entry_from_conversation(&conv, 42000, mtime),
        );
        entries.insert("empty.jsonl".to_string(), empty_entry(100, mtime));

        // Write cache
        write_project_cache(&project_name, entries);

        // Read it back
        let loaded = read_project_cache(&project_name);
        assert!(loaded.is_some(), "Cache file should be readable");

        let loaded = loaded.unwrap();
        assert_eq!(loaded.len(), 2);

        let conv_entry = loaded.get("conv1.jsonl").unwrap();
        assert!(!conv_entry.is_empty);
        assert_eq!(conv_entry.full_text, "Hello world Hi there");
        assert_eq!(conv_entry.agent_search_text, "subagent cache text");
        assert_eq!(conv_entry.total_tokens, 1500);

        let empty = loaded.get("empty.jsonl").unwrap();
        assert!(empty.is_empty);

        // Clean up
        if let Some(path) = cache_path_for_project(&project_name) {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn corrupt_cache_returns_none() {
        let project_name = format!("test-corrupt-{}", std::process::id());
        if let Some(path) = cache_path_for_project(&project_name) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Write garbage
            let _ = std::fs::write(&path, b"not a valid cache file");
            assert!(read_project_cache(&project_name).is_none());
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn wrong_version_returns_none() {
        let project_name = format!("test-version-{}", std::process::id());
        if let Some(path) = cache_path_for_project(&project_name) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Write valid magic but wrong version
            let cache = ProjectCache {
                magic: CACHE_MAGIC,
                schema_version: SCHEMA_VERSION + 1,
                entries: HashMap::new(),
            };
            let data = bincode::serialize(&cache).unwrap();
            let _ = std::fs::write(&path, &data);
            assert!(read_project_cache(&project_name).is_none());
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn wrong_magic_returns_none() {
        let project_name = format!("test-magic-{}", std::process::id());
        if let Some(path) = cache_path_for_project(&project_name) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let cache = ProjectCache {
                magic: *b"BADMAGIC",
                schema_version: SCHEMA_VERSION,
                entries: HashMap::new(),
            };
            let data = bincode::serialize(&cache).unwrap();
            let _ = std::fs::write(&path, &data);
            assert!(read_project_cache(&project_name).is_none());
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn missing_cache_returns_none() {
        assert!(read_project_cache("nonexistent-project-xyz-12345").is_none());
    }
}
