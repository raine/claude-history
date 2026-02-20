use crate::claude::LogEntry;
use crate::error::{AppError, Result};
use crate::history::{self, Conversation, LoaderMessage, ProviderKind};
use crate::tui::viewer;
use std::process::Command;
use std::sync::mpsc::Receiver;

pub struct ClaudeProvider {
    current_dir: Option<std::path::PathBuf>,
}

impl ClaudeProvider {
    pub fn new() -> Self {
        Self {
            current_dir: std::env::current_dir().ok(),
        }
    }
}

impl super::Provider for ClaudeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }

    fn name(&self) -> &str {
        "Claude Code"
    }

    fn detect(&self) -> bool {
        // Claude is always available if ~/.claude/projects exists
        history::get_claude_projects_root()
            .map(|p| p.exists())
            .unwrap_or(false)
    }

    fn load_conversations(
        &self,
        show_last: bool,
        debug: Option<crate::cli::DebugLevel>,
    ) -> Result<Vec<Conversation>> {
        let current_dir = self.current_dir.as_ref().ok_or_else(|| {
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Failed to get current directory",
            ))
        })?;
        let projects_dir = history::get_claude_projects_dir(current_dir)?;
        if !projects_dir.exists() {
            return Ok(Vec::new());
        }
        history::load_conversations(&projects_dir, show_last, debug)
    }

    fn load_conversations_streaming(
        &self,
        show_last: bool,
        debug: Option<crate::cli::DebugLevel>,
    ) -> Receiver<LoaderMessage> {
        history::load_all_conversations_streaming(show_last, debug)
    }

    fn read_entries(&self, conversation: &Conversation) -> Result<Vec<LogEntry>> {
        viewer::read_log_entries(&conversation.path).map_err(AppError::Io)
    }

    fn resume(&self, conversation: &Conversation, default_args: &[String]) -> Result<()> {
        let project_dir = match &conversation.project_path {
            Some(path) if path.exists() && path.is_dir() => path,
            Some(path) => {
                return Err(AppError::ClaudeExecutionError(format!(
                    "Project directory no longer exists: {}",
                    path.display()
                )));
            }
            None => {
                return Err(AppError::ClaudeExecutionError(
                    "Cannot determine project directory for this conversation".to_string(),
                ));
            }
        };

        let mut command = Command::new("claude");
        command.args(["--resume", &conversation.id]);
        command.args(default_args);
        command.current_dir(project_dir);

        run_claude_command(command)
    }

    fn delete(&self, conversation: &Conversation) -> Result<()> {
        std::fs::remove_file(&conversation.path).map_err(AppError::Io)
    }
}

#[cfg(unix)]
fn run_claude_command(mut command: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let err = command.exec();
    Err(AppError::ClaudeExecutionError(err.to_string()))
}

#[cfg(not(unix))]
fn run_claude_command(mut command: Command) -> Result<()> {
    let status = command
        .status()
        .map_err(|e| AppError::ClaudeExecutionError(e.to_string()))?;

    if !status.success() {
        return Err(AppError::ClaudeExecutionError(format!(
            "claude CLI exited with status {}",
            status
        )));
    }

    Ok(())
}
