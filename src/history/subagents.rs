//! Discovery of subagent transcripts stored as sidecar files.
//!
//! Some Claude Code setups (notably openclaw) do not embed subagent messages
//! inline in the parent transcript. Instead each subagent gets its own JSONL
//! transcript stored under a per-session directory:
//!
//! ```text
//! <projects>/<project>/<session-id>.jsonl        # parent session
//! <projects>/<project>/<session-id>/subagents/
//!     agent-<id>.jsonl                            # subagent transcript
//!     agent-<id>.meta.json                        # { agentType, description, toolUseId }
//! ```
//!
//! The main discovery pass deliberately skips `agent-*.jsonl` files and never
//! recurses into these directories, so subagents are invisible in the normal
//! conversation list. This module enumerates them on demand for a given
//! session so the TUI can offer a picker.

use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A subagent transcript discovered next to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentEntry {
    /// Path to the subagent's `agent-<id>.jsonl` transcript.
    pub path: PathBuf,
    /// Agent type from the meta sidecar (e.g. `Explore`), or the file stem
    /// when no meta is present.
    pub agent_type: String,
    /// Human description of the subagent's task from the meta sidecar.
    pub description: String,
    /// The parent `tool_use` id that spawned this subagent, if recorded.
    pub tool_use_id: String,
}

impl SubagentEntry {
    /// One-line label for display in a picker, e.g. `Explore — do the thing`.
    pub fn label(&self) -> String {
        if self.description.is_empty() {
            self.agent_type.clone()
        } else {
            format!("{} — {}", self.agent_type, self.description)
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct SubagentMeta {
    #[serde(default, rename = "agentType")]
    agent_type: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "toolUseId")]
    tool_use_id: String,
}

/// The `subagents/` directory that would hold sidecar transcripts for a
/// session whose transcript file is `session_path`.
fn subagents_dir(session_path: &Path) -> PathBuf {
    // `<...>/<session-id>.jsonl` -> `<...>/<session-id>` -> `<...>/<session-id>/subagents`
    session_path.with_extension("").join("subagents")
}

/// Discover subagent transcripts stored alongside `session_path`.
///
/// Returns entries ordered by file modification time (spawn order). Returns an
/// empty vec when the session has no `subagents/` directory — the common case
/// for standard inline-sidechain transcripts.
pub fn discover_subagents(session_path: &Path) -> Vec<SubagentEntry> {
    let dir = subagents_dir(session_path);
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut entries: Vec<(SystemTime, SubagentEntry)> = Vec::new();
    for dir_entry in read_dir.flatten() {
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with("agent-") {
            continue;
        }

        let meta = std::fs::read_to_string(path.with_extension("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<SubagentMeta>(&s).ok())
            .unwrap_or_default();

        let agent_type = if meta.agent_type.is_empty() {
            stem.to_string()
        } else {
            meta.agent_type
        };

        let mtime = dir_entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        entries.push((
            mtime,
            SubagentEntry {
                path,
                agent_type,
                description: meta.description,
                tool_use_id: meta.tool_use_id,
            },
        ));
    }

    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.into_iter().map(|(_, e)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn returns_empty_when_no_subagents_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let session = tmp.path().join("proj").join("sess-1.jsonl");
        write(&session, "{}\n");
        assert!(discover_subagents(&session).is_empty());
    }

    #[test]
    fn discovers_entries_with_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let session = proj.join("sess-1.jsonl");
        write(&session, "{}\n");

        let subdir = proj.join("sess-1").join("subagents");
        write(&subdir.join("agent-aaa.jsonl"), "{}\n");
        write(
            &subdir.join("agent-aaa.meta.json"),
            r#"{"agentType":"Explore","description":"look around","toolUseId":"call_1"}"#,
        );

        let found = discover_subagents(&session);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent_type, "Explore");
        assert_eq!(found[0].description, "look around");
        assert_eq!(found[0].tool_use_id, "call_1");
        assert_eq!(found[0].label(), "Explore — look around");
        assert!(found[0].path.ends_with("agent-aaa.jsonl"));
    }

    #[test]
    fn falls_back_to_stem_when_meta_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let session = proj.join("sess-1.jsonl");
        write(&session, "{}\n");

        let subdir = proj.join("sess-1").join("subagents");
        write(&subdir.join("agent-bbb.jsonl"), "{}\n");

        let found = discover_subagents(&session);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent_type, "agent-bbb");
        assert_eq!(found[0].description, "");
        assert_eq!(found[0].label(), "agent-bbb");
    }

    #[test]
    fn ignores_non_agent_and_non_jsonl_files() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        let session = proj.join("sess-1.jsonl");
        write(&session, "{}\n");

        let subdir = proj.join("sess-1").join("subagents");
        write(&subdir.join("agent-aaa.jsonl"), "{}\n");
        write(&subdir.join("agent-aaa.meta.json"), "{}");
        write(&subdir.join("notes.txt"), "ignore me");
        write(&subdir.join("other.jsonl"), "{}\n");

        let found = discover_subagents(&session);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("agent-aaa.jsonl"));
    }
}
