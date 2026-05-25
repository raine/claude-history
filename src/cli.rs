use clap::{Parser, Subcommand};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// Log level for debug output filtering
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DebugLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl FromStr for DebugLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "debug" => Ok(DebugLevel::Debug),
            "info" => Ok(DebugLevel::Info),
            "warn" | "warning" => Ok(DebugLevel::Warn),
            "error" => Ok(DebugLevel::Error),
            _ => Err(format!(
                "invalid log level '{}', expected: debug, info, warn, error",
                s
            )),
        }
    }
}

impl fmt::Display for DebugLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DebugLevel::Debug => write!(f, "debug"),
            DebugLevel::Info => write!(f, "info"),
            DebugLevel::Warn => write!(f, "warn"),
            DebugLevel::Error => write!(f, "error"),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Update claude-history to the latest version
    Update,
}

#[derive(Parser, Debug)]
#[command(name = "claude-history")]
#[command(version)]
#[command(about = "View Claude conversation history")]
#[command(args_conflicts_with_subcommands = true)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Show tool calls in the conversation output
    #[arg(long, short = 't', group = "tools_display")]
    pub show_tools: bool,

    /// Hide tool calls from the conversation output
    #[arg(long, group = "tools_display")]
    pub no_tools: bool,

    /// Show the conversation directory and exit
    #[arg(
        long,
        short = 'd',
        help = "Print the conversation directory path and exit"
    )]
    pub show_dir: bool,

    /// Show the last messages in the TUI preview (default)
    #[arg(long, short = 'l', group = "preview_content")]
    pub last: bool,

    /// Show the first messages in the TUI preview
    #[arg(long, group = "preview_content")]
    pub first: bool,

    /// Show thinking blocks and subagent internals in the conversation output
    #[arg(long, group = "thinking_display")]
    pub show_thinking: bool,

    /// Hide thinking blocks and subagent internals from the conversation output
    #[arg(long, group = "thinking_display")]
    pub hide_thinking: bool,

    /// Resume the selected conversation in the Claude CLI
    #[arg(
        long,
        short = 'c',
        help = "Resume the selected conversation in Claude Code"
    )]
    pub resume: bool,

    /// Fork the session when resuming (creates a new session branching from the original)
    #[arg(long, help = "Fork the session when resuming", requires = "resume")]
    pub fork_session: bool,

    /// Print the selected conversation's file path and exit
    #[arg(long, short = 'p', help = "Print the selected conversation file path")]
    pub show_path: bool,

    /// Print the selected conversation's session ID and exit
    #[arg(long, short = 'i', help = "Print the selected conversation session ID")]
    pub show_id: bool,

    /// Output in plain text format without ledger formatting (for piping to other tools)
    #[arg(long, help = "Output plain text without ledger formatting")]
    pub plain: bool,

    /// Show debug output for conversation loading
    #[arg(
        long,
        value_name = "LEVEL",
        default_missing_value = "debug",
        num_args = 0..=1,
        help = "Print debug information (optionally filter by level: debug, info, warn, error)"
    )]
    pub debug: Option<DebugLevel>,

    /// Deprecated: global is now the default behavior
    #[arg(long, short = 'g', hide = true)]
    pub global: bool,

    /// Show only conversations from the current workspace
    #[arg(
        long,
        short = 'L',
        help = "Show only conversations from the current workspace directory"
    )]
    pub local: bool,

    /// Display output through a pager (less)
    #[arg(long, group = "pager_display")]
    pub pager: bool,

    /// Disable pager output
    #[arg(long, group = "pager_display")]
    pub no_pager: bool,

    /// Render a JSONL file in ledger format and exit (for debugging)
    #[arg(
        long,
        value_name = "FILE",
        help = "Render a JSONL file in ledger format and exit"
    )]
    pub render: Option<PathBuf>,

    /// Disable colored output (for --render)
    #[arg(long, help = "Disable colored output")]
    pub no_color: bool,

    /// Delete a session by its ID
    #[arg(
        long,
        value_name = "SESSION_ID",
        help = "Delete a session by its UUID and exit",
        conflicts_with_all = ["global", "show_dir", "resume", "show_path", "show_id", "plain", "render", "input_file"]
    )]
    pub delete: Option<String>,

    /// Archive a session by its ID (moves to ~/.claude/archive/<project>/)
    #[arg(
        long,
        value_name = "SESSION_ID",
        help = "Archive a session by its UUID and exit",
        conflicts_with_all = ["global", "show_dir", "resume", "show_path", "show_id", "plain", "render", "input_file", "delete"]
    )]
    pub archive: Option<String>,

    /// Debug search scoring for a query
    #[arg(
        long = "debug-search",
        value_name = "QUERY",
        help = "Debug search result scoring for a query",
        conflicts_with_all = ["show_dir", "resume", "show_path", "show_id", "plain", "render", "delete", "archive", "input_file", "semantic_search"]
    )]
    pub debug_search: Option<String>,

    /// Debug semantic search matching for a query
    #[arg(
        long = "debug-semantic-search",
        value_name = "QUERY",
        value_parser = non_empty_string,
        hide = true,
        conflicts_with_all = ["show_dir", "resume", "show_path", "show_id", "plain", "render", "delete", "archive", "input_file", "debug_search", "semantic_search", "generate_semantic_cache", "clear_semantic_cache"]
    )]
    pub debug_semantic_search: Option<String>,

    /// Run a local semantic search over conversations
    #[arg(
        long = "semantic-search",
        value_name = "QUERY",
        help = "Run a local semantic search over conversations",
        value_parser = non_empty_string,
        hide = true,
        conflicts_with_all = ["show_dir", "resume", "show_path", "show_id", "plain", "render", "delete", "archive", "input_file", "debug_search", "clear_semantic_cache"]
    )]
    pub semantic_search: Option<String>,

    /// Number of semantic search results to show
    #[arg(
        long = "semantic-top",
        default_value_t = 20,
        value_parser = non_zero_usize,
        hide = true,
        requires = "semantic_search"
    )]
    pub semantic_top: usize,

    /// Generate the semantic embedding cache and exit
    #[arg(
        long = "generate-semantic-cache",
        hide = true,
        conflicts_with_all = ["show_dir", "resume", "show_path", "show_id", "plain", "render", "delete", "archive", "input_file", "debug_search", "semantic_search", "clear_semantic_cache"]
    )]
    pub generate_semantic_cache: bool,

    /// Clear semantic embedding and model cache files and exit
    #[arg(
        long = "clear-semantic-cache",
        hide = true,
        conflicts_with_all = ["show_dir", "resume", "show_path", "show_id", "plain", "render", "delete", "archive", "input_file", "debug_search", "semantic_search", "generate_semantic_cache", "debug_semantic_search"]
    )]
    pub clear_semantic_cache: bool,

    /// Input JSONL file to view directly (skips conversation selection)
    #[arg(
        value_name = "FILE",
        help = "JSONL conversation file to view directly",
        conflicts_with_all = ["global", "local", "show_dir", "resume", "show_path", "show_id", "plain", "render", "delete", "archive"]
    )]
    pub input_file: Option<PathBuf>,
}

fn non_empty_string(value: &str) -> std::result::Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err("value must not be empty".to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn non_zero_usize(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid positive integer: {value}"))?;
    if parsed == 0 {
        Err("value must be greater than zero".to_string())
    } else {
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn semantic_query_must_not_be_empty() {
        let err = Args::try_parse_from(["claude-history", "--semantic-search", "   "])
            .expect_err("empty query should fail");

        assert!(err.to_string().contains("value must not be empty"));
    }

    #[test]
    fn semantic_top_must_be_positive() {
        let err = Args::try_parse_from([
            "claude-history",
            "--semantic-search",
            "cache",
            "--semantic-top",
            "0",
        ])
        .expect_err("zero top should fail");

        assert!(err.to_string().contains("value must be greater than zero"));
    }

    #[test]
    fn semantic_help_text_is_polished_when_available() {
        let help = Args::command().render_long_help().to_string();

        assert!(!help.contains("proof of concept"));
        assert!(!help.contains("POC"));
    }

    #[test]
    fn default_help_hides_semantic_flags() {
        let help = Args::command().render_long_help().to_string();

        assert!(!help.contains("--semantic-search"));
        assert!(!help.contains("--semantic-top"));
        assert!(!help.contains("--generate-semantic-cache"));
        assert!(!help.contains("--clear-semantic-cache"));
        assert!(!help.contains("--debug-semantic-search"));
    }

    #[test]
    fn debug_semantic_search_flag_is_parseable() {
        let args =
            Args::try_parse_from(["claude-history", "--debug-semantic-search", "cache"]).unwrap();

        assert_eq!(args.debug_semantic_search.as_deref(), Some("cache"));
    }

    #[test]
    fn generate_semantic_cache_flag_is_parseable() {
        let args = Args::try_parse_from(["claude-history", "--generate-semantic-cache"]).unwrap();

        assert!(args.generate_semantic_cache);
    }

    #[test]
    fn clear_semantic_cache_flag_is_parseable() {
        let args = Args::try_parse_from(["claude-history", "--clear-semantic-cache"]).unwrap();

        assert!(args.clear_semantic_cache);
    }

    #[test]
    fn semantic_flag_is_removed() {
        let err = Args::try_parse_from(["claude-history", "--semantic"])
            .expect_err("removed flag should fail");

        assert!(err.to_string().contains("unexpected argument '--semantic'"));
    }
}
