use clap::Parser;
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

#[derive(Parser, Debug)]
#[command(name = "claude-history")]
#[command(about = "View Claude conversation history")]
pub struct Args {
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

    /// Show the last messages in the TUI preview
    #[arg(long, short = 'l', group = "preview_content")]
    pub last: bool,

    /// Show the first messages in the TUI preview
    #[arg(long, group = "preview_content")]
    pub first: bool,

    /// Display relative time (e.g. "10 minutes ago")
    #[arg(long, short = 'r', group = "time_display")]
    pub relative_time: bool,

    /// Display absolute timestamp
    #[arg(long, group = "time_display")]
    pub absolute_time: bool,

    /// Show thinking blocks in the conversation output
    #[arg(long, group = "thinking_display")]
    pub show_thinking: bool,

    /// Hide thinking blocks from the conversation output
    #[arg(long, group = "thinking_display")]
    pub hide_thinking: bool,

    /// Resume the selected conversation in the Claude CLI
    #[arg(
        long,
        short = 'c',
        help = "Resume the selected conversation in Claude Code"
    )]
    pub resume: bool,

    /// Print the selected conversation's file path and exit
    #[arg(long, short = 'p', help = "Print the selected conversation file path")]
    pub show_path: bool,

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

    /// Search conversations from all projects globally
    #[arg(
        long,
        short = 'g',
        help = "Search all conversations from all projects at once"
    )]
    pub global: bool,

    // ==================== Time Filtering ====================
    /// Show conversations since a duration ago (e.g., 2d, 1w, 3h)
    ///
    /// Duration format: <number><unit> where unit is:
    /// - h: hours (e.g., 3h = 3 hours ago)
    /// - d: days (e.g., 2d = 2 days ago)
    /// - w: weeks (e.g., 1w = 1 week ago)
    /// - m: months, ~30 days (e.g., 1m = ~30 days ago)
    #[arg(
        long,
        short = 's',
        value_name = "DURATION",
        help = "Show conversations since duration (e.g., 2d, 1w, 3h)"
    )]
    pub since: Option<String>,

    /// Show conversations after a specific date (YYYY-MM-DD)
    #[arg(
        long,
        value_name = "DATE",
        help = "Show conversations after date (YYYY-MM-DD)"
    )]
    pub after: Option<String>,

    /// Show conversations before a specific date (YYYY-MM-DD)
    #[arg(
        long,
        value_name = "DATE",
        help = "Show conversations before date (YYYY-MM-DD)"
    )]
    pub before: Option<String>,

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

    /// Input JSONL file to view directly (skips conversation selection)
    #[arg(
        value_name = "FILE",
        help = "JSONL conversation file to view directly",
        conflicts_with_all = ["global", "show_dir", "resume", "show_path", "plain", "render"]
    )]
    pub input_file: Option<PathBuf>,
}
