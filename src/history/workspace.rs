//! The current workspace: the directory claude-history was started from and
//! the Claude project directory name it encodes to.
//!
//! The workspace filter (`--local`, `Tab` in the TUI, local agent scope) uses a
//! single rule defined here so every surface agrees on which conversations
//! belong to the current folder.

use std::path::{Path, PathBuf};

use super::path::{convert_path_to_project_dir_name, format_short_name_from_path, is_same_project};
use super::{Conversation, Source};

/// The directory the workspace filter is anchored to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    /// The workspace directory as given (usually the process cwd).
    pub dir: PathBuf,
    /// The canonicalized directory used for path comparisons.
    canonical_dir: PathBuf,
    /// Claude's encoded project directory name for `dir`.
    pub project_dir_name: String,
}

impl Workspace {
    /// Build a workspace anchored at `dir`.
    pub fn from_dir(dir: &Path) -> Self {
        let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        Self {
            dir: dir.to_path_buf(),
            canonical_dir,
            project_dir_name: convert_path_to_project_dir_name(dir),
        }
    }

    /// Build a workspace anchored at the process working directory.
    pub fn current() -> Option<Self> {
        std::env::current_dir().ok().map(|dir| Self::from_dir(&dir))
    }

    /// Short display name for the workspace directory (folder name, or
    /// `project/worktree` for workmux worktrees).
    pub fn short_name(&self) -> String {
        format_short_name_from_path(&self.dir)
    }

    /// Whether a conversation belongs to this workspace.
    ///
    /// Claude conversations match when either:
    /// - they are stored under this workspace's project directory (or one of
    ///   its workmux worktrees), or
    /// - any working directory recorded in the transcript is this directory or
    ///   a directory below it. Claude Code records the shell's cwd on every
    ///   message, so a session started in a parent folder that `cd`s into this
    ///   folder is included; a session that only visited a sibling is not.
    ///
    /// Pi and OMP conversations match when their header cwd is this directory.
    pub fn contains(&self, conversation: &Conversation) -> bool {
        if conversation.source != Source::Claude {
            return conversation
                .project_path
                .as_ref()
                .or(conversation.cwd.as_ref())
                .is_some_and(|path| self.canonical(path) == self.canonical_dir);
        }

        let stored_here = conversation
            .path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| is_same_project(&name.to_string_lossy(), &self.project_dir_name));
        if stored_here {
            return true;
        }

        conversation
            .cwds
            .iter()
            .chain(conversation.cwd.iter())
            .any(|path| self.canonical(path).starts_with(&self.canonical_dir))
    }

    fn canonical(&self, path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    fn claude_conversation(project_dir_name: &str, cwds: &[&str]) -> Conversation {
        let cwds: Vec<PathBuf> = cwds.iter().map(PathBuf::from).collect();
        Conversation {
            source: Source::Claude,
            session_id: "11111111-1111-4111-8111-111111111111".to_string(),
            path: PathBuf::from(format!(
                "/home/u/.claude/projects/{project_dir_name}/11111111-1111-4111-8111-111111111111.jsonl"
            )),
            index: 0,
            timestamp: Local::now(),
            preview: String::new(),
            preview_first: String::new(),
            preview_last: String::new(),
            full_text: String::new(),
            agent_search_text: String::new(),
            semantic_route_text: String::new(),
            semantic_turns: Vec::new(),
            semantic_turn_ranges: Vec::new(),
            search_text_lower: String::new(),
            project_name: None,
            project_path: cwds.first().cloned(),
            cwd: cwds.first().cloned(),
            cwds,
            message_count: 0,
            parse_errors: Vec::new(),
            summary: None,
            custom_title: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        }
    }

    fn workspace(dir: &str) -> Workspace {
        Workspace::from_dir(Path::new(dir))
    }

    #[test]
    fn matches_conversation_stored_under_workspace_project_dir() {
        let ws = workspace("/Users/me/src/group/repo-a");
        let conv = claude_conversation("-Users-me-src-group-repo-a", &[]);
        assert!(ws.contains(&conv));
    }

    #[test]
    fn matches_worktree_of_workspace_project() {
        let ws = workspace("/Users/me/src/group/repo-a");
        let conv = claude_conversation("-Users-me-src-group-repo-a--worktrees-feature", &[]);
        assert!(ws.contains(&conv));
    }

    #[test]
    fn matches_parent_session_that_worked_inside_workspace() {
        // Session started in the group folder, then cd'd into repo-a and repo-b.
        let ws = workspace("/Users/me/src/group/repo-a");
        let conv = claude_conversation(
            "-Users-me-src-group",
            &[
                "/Users/me/src/group",
                "/Users/me/src/group/repo-b",
                "/Users/me/src/group/repo-a/docs",
            ],
        );
        assert!(ws.contains(&conv));
    }

    #[test]
    fn rejects_parent_session_that_only_visited_sibling() {
        let ws = workspace("/Users/me/src/group/repo-a");
        let conv = claude_conversation(
            "-Users-me-src-group",
            &["/Users/me/src/group", "/Users/me/src/group/repo-b"],
        );
        assert!(!ws.contains(&conv));
    }

    #[test]
    fn rejects_session_stored_under_sibling_project() {
        let ws = workspace("/Users/me/src/group/repo-a");
        let conv = claude_conversation(
            "-Users-me-src-group-repo-b",
            &["/Users/me/src/group/repo-b"],
        );
        assert!(!ws.contains(&conv));
    }

    #[test]
    fn rejects_prefix_that_is_not_a_path_component() {
        let ws = workspace("/Users/me/src/group/repo-a");
        let conv = claude_conversation("-Users-me-src-group", &["/Users/me/src/group/repo-a-old"]);
        assert!(!ws.contains(&conv));
    }

    #[test]
    fn parent_workspace_includes_child_sessions_by_recorded_cwd() {
        let ws = workspace("/Users/me/src/group");
        let conv = claude_conversation(
            "-Users-me-src-group-repo-a",
            &["/Users/me/src/group/repo-a"],
        );
        assert!(ws.contains(&conv));
    }

    #[test]
    fn pi_conversation_matches_only_exact_cwd() {
        let ws = workspace("/Users/me/src/group/repo-a");
        let mut conv = claude_conversation("ignored", &["/Users/me/src/group/repo-a"]);
        conv.source = Source::Pi;
        assert!(ws.contains(&conv));
        conv.project_path = Some(PathBuf::from("/Users/me/src/group/repo-a/sub"));
        assert!(!ws.contains(&conv));
    }

    #[test]
    fn short_name_is_folder_name() {
        assert_eq!(
            workspace("/Users/me/src/group/repo-a").short_name(),
            "repo-a"
        );
    }
}
