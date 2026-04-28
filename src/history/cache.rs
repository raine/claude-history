//! Per-project binary cache for parsed conversation metadata.
//!
//! Stores parsed conversation data in bincode format, keyed by session filename
//! and validated by mtime + file size. Eliminates redundant JSONL parsing and
//! search text normalization on startup for unchanged files.

use super::{Conversation, ParseError};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_MAGIC: [u8; 8] = *b"CLHIST01";
const SCHEMA_VERSION: u32 = 2;

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
    pub search_text_lower: String,
    pub cwd: Option<PathBuf>,
    pub message_count: usize,
    pub user_turn_count: usize,
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
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let cache = ProjectCache {
        magic: CACHE_MAGIC,
        schema_version: SCHEMA_VERSION,
        entries,
    };
    let Ok(data) = bincode::serialize(&cache) else {
        return;
    };
    // Use tempfile in the same directory for safe atomic rename
    let Ok(mut tmp) = tempfile::NamedTempFile::new_in(parent) else {
        return;
    };
    if tmp.write_all(&data).is_err() {
        return;
    }
    let _ = tmp.persist(&path);
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
        search_text_lower: String::new(),
        cwd: None,
        message_count: 0,
        user_turn_count: 0,
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
        search_text_lower: conv.search_text_lower.clone(),
        cwd: conv.cwd.clone(),
        message_count: conv.message_count,
        user_turn_count: conv.user_turn_count,
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
        path,
        index: 0,
        timestamp,
        preview,
        preview_first: entry.preview_first.clone(),
        preview_last: entry.preview_last.clone(),
        full_text: entry.full_text.clone(),
        search_text_lower: entry.search_text_lower.clone(),
        project_name: None,
        project_path: None,
        cwd: entry.cwd.clone(),
        message_count: entry.message_count,
        user_turn_count: entry.user_turn_count,
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
    use crate::tui::search::normalize_for_search;
    use std::time::Duration;

    fn make_test_conversation() -> Conversation {
        let timestamp = Local::now();
        Conversation {
            path: PathBuf::from("/test/conv.jsonl"),
            index: 0,
            timestamp,
            preview: "Hello world ... Hi there".to_string(),
            preview_first: "Hello world ... Hi there".to_string(),
            preview_last: "Hi there ... Hello world".to_string(),
            full_text: "Hello world Hi there".to_string(),
            search_text_lower: normalize_for_search("Hello world Hi there"),
            project_name: Some("test-project".to_string()),
            project_path: Some(PathBuf::from("/test/project")),
            cwd: Some(PathBuf::from("/test/cwd")),
            message_count: 2,
            user_turn_count: 1,
            parse_errors: vec![],
            summary: Some("Test summary".to_string()),
            custom_title: Some("My Session".to_string()),
            model: Some("claude-opus-4-5-20251101".to_string()),
            total_tokens: 1500,
            duration_minutes: Some(10),
        }
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
        assert_eq!(restored.search_text_lower, conv.search_text_lower);
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
