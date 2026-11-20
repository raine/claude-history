//! Per-user pin/star state for conversations.
//!
//! Pinning is a claude-history-only concept: it is never written into a
//! session's transcript (unlike `history::rename`, which appends records the
//! real agent CLI also understands) and never stored in the keyword store
//! under `store::cache_base_dir()`, whose generations are rebuilt and pruned
//! by `store::reconcile`. Instead pins live in their own small JSON file
//! beside `config.toml`, so they survive store rebuilds, work the same from
//! every workspace, and are never silently dropped by a reconcile pass.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Path of the file pinned conversations are recorded in:
/// `~/.config/claude-history/pins.json`. `None` when there is no home
/// directory, or in tests, so they never touch a real pins file.
pub fn pins_path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    home::home_dir().map(|mut path| {
        path.push(".config");
        path.push("claude-history");
        path.push("pins.json");
        path
    })
}

#[derive(Default, Deserialize, Serialize)]
struct PinsFile {
    /// Pinned conversation's path (as `Path::display` would show it) to when
    /// it was pinned, epoch milliseconds. The timestamp is kept for a
    /// possible future "pinned at" sort; today only the key set matters.
    #[serde(default)]
    pinned: std::collections::HashMap<String, i64>,
}

/// Every pinned conversation's path, read from disk. Empty when there is no
/// pins file, no home directory, or the file cannot be parsed: pinning is a
/// nice-to-have, so a corrupt or missing file never blocks the app.
pub fn load_pins() -> HashSet<PathBuf> {
    let Some(path) = pins_path() else {
        return HashSet::new();
    };
    read_pins_file(&path)
        .map(|file| file.pinned.into_keys().map(PathBuf::from).collect())
        .unwrap_or_default()
}

fn read_pins_file(path: &Path) -> Result<PinsFile> {
    if !path.exists() {
        return Ok(PinsFile::default());
    }
    let content = std::fs::read(path)?;
    Ok(serde_json::from_slice(&content)?)
}

fn write_pins_file(path: &Path, file: &PinsFile) -> Result<()> {
    let dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;
    let mut temp = tempfile::NamedTempFile::new_in(&dir)?;
    serde_json::to_writer_pretty(&mut temp, file)?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Records whether `conversation` is pinned in the pins file at `path`.
fn set_pinned_at(path: &Path, conversation: &Path, pinned: bool) -> Result<()> {
    let mut file = read_pins_file(path)?;
    let key = conversation.to_string_lossy().into_owned();
    if pinned {
        file.pinned
            .insert(key, chrono::Utc::now().timestamp_millis());
    } else {
        file.pinned.remove(&key);
    }
    write_pins_file(path, &file)
}

/// Records whether `conversation` is pinned, in the user's pins file.
/// Does nothing when there is no file to write to (no home directory, or in
/// tests) — callers that need the toggle reflected should track it
/// themselves, as the TUI's `App` does in memory.
pub fn set_pinned(conversation: &Path, pinned: bool) -> Result<()> {
    match pins_path() {
        Some(path) => set_pinned_at(&path, conversation, pinned),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_pinned_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-history").join("pins.json");
        let session = PathBuf::from("/home/user/.claude/projects/-p/abc.jsonl");

        assert!(read_pins_file(&path).unwrap().pinned.is_empty());

        set_pinned_at(&path, &session, true).unwrap();
        let file = read_pins_file(&path).unwrap();
        assert_eq!(file.pinned.len(), 1);
        assert!(
            file.pinned
                .contains_key(&session.to_string_lossy().into_owned())
        );

        set_pinned_at(&path, &session, false).unwrap();
        assert!(read_pins_file(&path).unwrap().pinned.is_empty());
    }

    #[test]
    fn set_pinned_keeps_other_entries_and_overwrites_the_file_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        let a = PathBuf::from("/a.jsonl");
        let b = PathBuf::from("/b.jsonl");

        set_pinned_at(&path, &a, true).unwrap();
        set_pinned_at(&path, &b, true).unwrap();
        let file = read_pins_file(&path).unwrap();
        assert_eq!(file.pinned.len(), 2);

        set_pinned_at(&path, &a, false).unwrap();
        let file = read_pins_file(&path).unwrap();
        assert_eq!(file.pinned.len(), 1);
        assert!(file.pinned.contains_key(&b.to_string_lossy().into_owned()));

        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn read_pins_file_is_empty_for_a_missing_or_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        assert!(read_pins_file(&missing).unwrap().pinned.is_empty());

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, "{not json").unwrap();
        assert!(read_pins_file(&corrupt).is_err());
    }

    #[test]
    fn pins_path_is_none_in_tests() {
        assert!(pins_path().is_none());
    }

    #[test]
    fn set_pinned_is_a_noop_without_a_path() {
        // In tests `pins_path()` is always `None`; this should not panic or
        // try to touch disk.
        set_pinned(Path::new("/does/not/matter.jsonl"), true).unwrap();
    }
}
