use crate::agent::protocol::MessageLineRange;
use crate::search::mode::SearchMode;
use crate::time_filter::{TimeFilter, TimeFilterError, TimePoint};
use clap::{ArgGroup, Args as ClapArgs, Parser, Subcommand};
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
    /// Run agent-oriented search and transcript commands
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    /// Delete transcript files with no Claude messages
    DeleteEmpty {
        /// Delete matching transcripts instead of printing a dry run
        #[arg(long)]
        yes: bool,
        /// Only inspect the current workspace
        #[arg(long, group = "delete_empty_scope")]
        local: bool,
        /// Inspect all workspaces
        #[arg(long, group = "delete_empty_scope")]
        all: bool,
    },
    /// Update claude-history to the latest version
    Update,
}

#[derive(Debug, ClapArgs)]
pub struct DeleteEmptyArgs {
    pub yes: bool,
    pub local: bool,
    pub all: bool,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Search across all conversations
    Search(AgentSearchArgs),
    /// Search within one conversation
    Within(AgentWithinArgs),
    /// Read transcript message ranges
    Read(AgentReadArgs),
    /// Outline a conversation transcript
    Outline(AgentOutlineArgs),
}

#[derive(Debug, ClapArgs, Default)]
#[group(id = "agent_search_mode", multiple = false)]
pub struct AgentSearchModeArgs {
    /// Search mode for this invocation
    #[arg(long, value_enum)]
    pub mode: Option<SearchMode>,
    /// Alias for --mode lexical
    #[arg(long)]
    lexical: bool,
    /// Alias for --mode semantic
    #[arg(long)]
    semantic: bool,
    /// Alias for --mode exact
    #[arg(long)]
    exact: bool,
    /// Alias for --mode hybrid. Combine lexical and semantic message ranking
    #[arg(long)]
    hybrid: bool,
}

impl AgentSearchModeArgs {
    #[cfg(test)]
    pub fn explicit(mode: SearchMode) -> Self {
        Self {
            mode: Some(mode),
            ..Self::default()
        }
    }

    pub fn override_value(&self) -> Option<SearchMode> {
        self.mode.or_else(|| {
            [
                (self.lexical, SearchMode::Lexical),
                (self.semantic, SearchMode::Semantic),
                (self.exact, SearchMode::Exact),
                (self.hybrid, SearchMode::Hybrid),
            ]
            .into_iter()
            .find_map(|(selected, mode)| selected.then_some(mode))
        })
    }
}

#[derive(Debug, ClapArgs)]
pub struct AgentSearchArgs {
    /// Search query
    #[arg(value_parser = non_empty_string)]
    pub query: String,
    /// Maximum conversations, or message hits with --flat
    #[arg(long, value_parser = non_zero_usize)]
    pub top: Option<usize>,
    /// Output budget in Unicode characters
    #[arg(long, value_parser = non_zero_usize, conflicts_with = "no_budget")]
    pub budget: Option<usize>,
    /// Disable output budgeting
    #[arg(long)]
    pub no_budget: bool,
    /// Rank message hits across conversations; --top counts hits
    #[arg(long)]
    pub flat: bool,
    /// Maximum evidence hits to show per conversation in grouped output
    #[arg(long, value_parser = non_zero_usize)]
    pub hits_per_conv: Option<usize>,
    /// Disable duplicate suppression inside grouped conversation results
    #[arg(long)]
    pub all_hits: bool,
    /// Search only the current workspace
    #[arg(long, group = "agent_search_scope")]
    pub local: bool,
    /// Search all workspaces
    #[arg(long, group = "agent_search_scope")]
    pub all: bool,
    #[command(flatten)]
    pub search_mode: AgentSearchModeArgs,
    #[command(flatten)]
    pub time: TimeRangeArgs,
}

#[derive(Debug, ClapArgs)]
pub struct AgentWithinArgs {
    /// Conversation reference
    #[arg(value_parser = non_empty_string)]
    pub conversation: String,
    /// Search query
    #[arg(value_parser = non_empty_string)]
    pub query: String,
    /// Maximum number of results
    #[arg(long, value_parser = non_zero_usize)]
    pub top: Option<usize>,
    /// Output budget in Unicode characters
    #[arg(long, value_parser = non_zero_usize, conflicts_with = "no_budget")]
    pub budget: Option<usize>,
    /// Disable output budgeting
    #[arg(long)]
    pub no_budget: bool,
    #[command(flatten)]
    pub search_mode: AgentSearchModeArgs,
}

#[derive(Debug, ClapArgs)]
pub struct AgentOutputFlags {
    /// Output budget in Unicode characters
    #[arg(long, value_parser = non_zero_usize, conflicts_with = "no_budget")]
    pub budget: Option<usize>,
    /// Disable output budgeting
    #[arg(long)]
    pub no_budget: bool,
    /// Include tool calls
    #[arg(long)]
    pub tools: bool,
    /// Include tool results
    #[arg(long)]
    pub tool_results: bool,
    /// Include thinking blocks
    #[arg(long)]
    pub thinking: bool,
    /// Include subagent internals
    #[arg(long)]
    pub subagents: bool,
}

#[derive(Debug, ClapArgs)]
pub struct AgentReadArgs {
    /// Conversation or message range refs to read
    #[arg(required = true, value_parser = non_empty_string)]
    pub refs: Vec<String>,
    /// Durable message anchor to read within one conversation
    #[arg(long, value_parser = non_empty_string)]
    pub anchor: Option<String>,
    /// Message or range to prioritize when budgeted output is truncated
    #[arg(long, value_parser = non_empty_string)]
    pub focus: Option<String>,
    /// Inclusive content line range within one message
    #[arg(long, value_name = "START..END", conflicts_with = "match_query")]
    pub lines: Option<MessageLineRange>,
    /// Find text within one message and return matching lines with context
    #[arg(long = "match", value_parser = non_empty_string, conflicts_with = "lines")]
    pub match_query: Option<String>,
    /// Content lines to include before and after each match
    #[arg(long, default_value_t = 3, requires = "match_query")]
    pub context: usize,
    #[command(flatten)]
    pub output: AgentOutputFlags,
}

#[derive(Debug, ClapArgs)]
pub struct AgentOutlineArgs {
    /// Conversation reference to outline
    #[arg(value_parser = non_empty_string)]
    pub conversation: String,
    #[command(flatten)]
    pub output: AgentOutputFlags,
}

// Bounds that narrow the conversation corpus by timestamp before ranking.
//
// Flattened into both the top-level args and `agent search`, because
// `args_conflicts_with_subcommands` means a top-level flag cannot be combined
// with a subcommand. Kept as `//` rather than `///`: clap promotes a flattened
// struct's doc comment to the containing command's long help.
#[derive(Debug, Default, ClapArgs)]
pub struct TimeRangeArgs {
    /// Only conversations this recent, as a duration or date (2d, 1d6h, 6mo, 2026-07-20)
    #[arg(long, value_name = "WHEN", group = "time_lower_bound")]
    pub since: Option<TimePoint>,
    /// Alias for --since
    #[arg(long, value_name = "WHEN", group = "time_lower_bound")]
    pub after: Option<TimePoint>,
    /// Only conversations older than this duration or date
    #[arg(long, value_name = "WHEN")]
    pub before: Option<TimePoint>,
}

impl TimeRangeArgs {
    /// Resolve the flags into a filter, relative to the current instant.
    pub fn resolve(&self) -> std::result::Result<TimeFilter, TimeFilterError> {
        TimeFilter::resolve(
            self.since.as_ref(),
            self.after.as_ref(),
            self.before.as_ref(),
        )
    }
}

impl AgentSearchArgs {
    pub fn mode_override(&self) -> Option<SearchMode> {
        self.search_mode.override_value()
    }
}

impl AgentWithinArgs {
    pub fn mode_override(&self) -> Option<SearchMode> {
        self.search_mode.override_value()
    }
}

#[derive(Parser, Debug)]
#[command(name = "claude-history")]
#[command(version)]
#[command(about = "View Claude conversation history")]
#[command(args_conflicts_with_subcommands = true)]
#[command(group(
    ArgGroup::new("one_shot_action")
        .args([
            "show_dir",
            "delete",
            "debug_search",
            "debug_semantic_search",
            "semantic_search",
            "generate_semantic_cache",
            "clear_semantic_cache",
            "render",
            "input_file",
        ])
        .multiple(false)
))]
#[command(group(
    ArgGroup::new("selection_conflicting_action")
        .args([
            "delete",
            "debug_search",
            "debug_semantic_search",
            "semantic_search",
            "generate_semantic_cache",
            "clear_semantic_cache",
            "input_file",
        ])
        .multiple(false)
        .conflicts_with_all(["resume", "show_path", "show_id", "plain"])
))]
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

    #[command(flatten)]
    pub time: TimeRangeArgs,

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
        conflicts_with = "global"
    )]
    pub delete: Option<String>,

    /// Debug search scoring for a query
    #[arg(
        long = "debug-search",
        value_name = "QUERY",
        help = "Debug search result scoring for a query"
    )]
    pub debug_search: Option<String>,

    /// Debug semantic search matching for a query
    #[arg(
        long = "debug-semantic-search",
        value_name = "QUERY",
        value_parser = non_empty_string,
        hide = true
    )]
    pub debug_semantic_search: Option<String>,

    /// Run a local semantic search over conversations
    #[arg(
        long = "semantic-search",
        value_name = "QUERY",
        help = "Run a local semantic search over conversations",
        value_parser = non_empty_string,
        hide = true
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
    #[arg(long = "generate-semantic-cache", hide = true)]
    pub generate_semantic_cache: bool,

    /// Clear semantic embedding and model cache files and exit
    #[arg(long = "clear-semantic-cache", hide = true)]
    pub clear_semantic_cache: bool,

    /// Input JSONL file to view directly (skips conversation selection)
    #[arg(
        value_name = "FILE",
        help = "JSONL conversation file to view directly",
        conflicts_with_all = ["global", "local"]
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
    fn delete_empty_defaults_to_dry_run_all_workspaces() {
        let args = Args::try_parse_from(["claude-history", "delete-empty"]).unwrap();

        match args.command.unwrap() {
            Commands::DeleteEmpty { yes, local, all } => {
                assert!(!yes);
                assert!(!local);
                assert!(!all);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn delete_empty_captures_delete_and_local_flags() {
        let args =
            Args::try_parse_from(["claude-history", "delete-empty", "--yes", "--local"]).unwrap();

        match args.command.unwrap() {
            Commands::DeleteEmpty { yes, local, all } => {
                assert!(yes);
                assert!(local);
                assert!(!all);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_search_captures_time_range_flags() {
        let args = Args::try_parse_from([
            "claude-history",
            "agent",
            "search",
            "cache warming",
            "--since",
            "2d",
            "--before",
            "2026-07-20",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Search(search),
            } => {
                assert!(search.time.since.is_some());
                assert!(search.time.after.is_none());
                assert!(search.time.before.is_some());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn top_level_time_range_flags_are_accepted() {
        let args = Args::try_parse_from(["claude-history", "--since", "1w"]).unwrap();
        assert!(args.time.since.is_some());
        assert!(args.time.resolve().unwrap().is_active());
    }

    #[test]
    fn absent_time_range_flags_resolve_to_an_inactive_filter() {
        let args = Args::try_parse_from(["claude-history"]).unwrap();
        assert_eq!(args.time.resolve().unwrap(), TimeFilter::default());
    }

    #[test]
    fn since_and_after_are_mutually_exclusive() {
        assert!(
            Args::try_parse_from(["claude-history", "--since", "2d", "--after", "3d"]).is_err()
        );
        assert!(
            Args::try_parse_from([
                "claude-history",
                "agent",
                "search",
                "needle",
                "--since",
                "2d",
                "--after",
                "3d",
            ])
            .is_err()
        );
    }

    #[test]
    fn since_and_before_can_be_combined() {
        // Absolute on both ends so the assertion does not depend on today's
        // date the way a relative lower bound would.
        let args = Args::try_parse_from([
            "claude-history",
            "--since",
            "2026-01-01",
            "--before",
            "2026-07-20",
        ])
        .unwrap();

        let filter = args.time.resolve().unwrap();
        assert!(filter.after.is_some());
        assert!(filter.before.is_some());
    }

    #[test]
    fn invalid_time_value_is_rejected_at_parse_time() {
        let error = Args::try_parse_from(["claude-history", "--since", "30x"])
            .expect_err("30x is not a valid span");

        // Proves the bare FromStr impl's Display reaches the user rather than a
        // generic clap "invalid value" message.
        assert!(
            error.to_string().contains("unknown unit 'x'"),
            "unhelpful error: {error}"
        );
    }

    #[test]
    fn inverted_time_range_is_rejected_at_resolve_time() {
        // clap accepts each value on its own; only resolving the pair catches
        // the contradiction.
        let args = Args::try_parse_from([
            "claude-history",
            "--after",
            "2026-07-20",
            "--before",
            "2026-01-01",
        ])
        .unwrap();

        assert!(args.time.resolve().is_err());
    }

    #[test]
    fn agent_search_captures_query_top_scope_and_mode_override() {
        let args = Args::try_parse_from([
            "claude-history",
            "agent",
            "search",
            "cache warming",
            "--hybrid",
            "--top",
            "7",
            "--local",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Search(search),
            } => {
                assert_eq!(search.query, "cache warming");
                assert_eq!(search.top, Some(7));
                assert!(!search.flat);
                assert_eq!(search.hits_per_conv, None);
                assert!(!search.all_hits);
                assert!(search.local);
                assert!(!search.all);
                assert_eq!(search.mode_override(), Some(SearchMode::Hybrid));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_search_defaults_to_global_scope_top_ten_and_config_mode() {
        let args = Args::try_parse_from(["claude-history", "agent", "search", "cache"]).unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Search(search),
            } => {
                assert_eq!(search.top, None);
                assert!(!search.flat);
                assert_eq!(search.hits_per_conv, None);
                assert!(!search.all_hits);
                assert!(!search.local);
                assert!(!search.all);
                assert_eq!(search.mode_override(), None);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_search_captures_browsing_flags() {
        let args = Args::try_parse_from([
            "claude-history",
            "agent",
            "search",
            "cache",
            "--flat",
            "--hits-per-conv",
            "5",
            "--all-hits",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Search(search),
            } => {
                assert!(search.flat);
                assert_eq!(search.hits_per_conv, Some(5));
                assert!(search.all_hits);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_search_help_explains_grouped_and_flat_top_semantics() {
        let mut command = Args::command();
        let search = command
            .find_subcommand_mut("agent")
            .unwrap()
            .find_subcommand_mut("search")
            .unwrap();
        let help = search.render_long_help().to_string();

        assert!(help.contains("Maximum conversations, or message hits with --flat"));
        assert!(help.contains("Rank message hits across conversations; --top counts hits"));
        assert!(help.contains("Combine lexical and semantic message ranking"));
    }

    #[test]
    fn agent_help_documents_anchor_reads() {
        let mut command = Args::command();
        let agent = command.find_subcommand_mut("agent").unwrap();
        let read_help = agent
            .find_subcommand_mut("read")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(read_help.contains("--anchor"));
        assert!(read_help.contains("Durable message anchor"));
    }

    #[test]
    fn agent_within_rejects_search_browsing_flags() {
        let err = Args::try_parse_from([
            "claude-history",
            "agent",
            "within",
            "ch_abc123",
            "cache",
            "--flat",
        ])
        .expect_err("search-only flag should fail for within");

        assert!(err.to_string().contains("unexpected argument '--flat'"));
    }

    #[test]
    fn agent_within_captures_ref_query_top_and_mode_override() {
        let args = Args::try_parse_from([
            "claude-history",
            "agent",
            "within",
            "ch_abc123",
            "cache",
            "--semantic",
            "--top",
            "4",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Within(within),
            } => {
                assert_eq!(within.conversation, "ch_abc123");
                assert_eq!(within.query, "cache");
                assert_eq!(within.top, Some(4));
                assert_eq!(within.mode_override(), Some(SearchMode::Semantic));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_read_captures_refs_and_display_options() {
        let args = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123:m1..m3",
            "ch_def456:m4",
            "--focus",
            "m2",
            "--budget",
            "1234",
            "--tools",
            "--tool-results",
            "--thinking",
            "--subagents",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Read(read),
            } => {
                assert_eq!(read.refs, vec!["ch_abc123:m1..m3", "ch_def456:m4"]);
                assert_eq!(read.anchor, None);
                assert_eq!(read.focus.as_deref(), Some("m2"));
                assert_eq!(read.lines, None);
                assert_eq!(read.match_query, None);
                assert_eq!(read.context, 3);
                assert_eq!(read.output.budget, Some(1234));
                assert!(!read.output.no_budget);
                assert!(read.output.tools);
                assert!(read.output.tool_results);
                assert!(read.output.thinking);
                assert!(read.output.subagents);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_read_defaults_to_budgeted_minimal_output() {
        let args = Args::try_parse_from(["claude-history", "agent", "read", "ch_abc123"]).unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Read(read),
            } => {
                assert_eq!(read.output.budget, None);
                assert!(!read.output.no_budget);
                assert!(!read.output.tools);
                assert!(!read.output.tool_results);
                assert!(!read.output.thinking);
                assert!(!read.output.subagents);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_read_captures_message_slice_options() {
        let lines = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123:m7",
            "--lines",
            "40..120",
        ])
        .unwrap();
        match lines.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Read(read),
            } => assert_eq!(
                read.lines,
                Some(MessageLineRange {
                    start: 40,
                    end: 120
                })
            ),
            other => panic!("unexpected command: {other:?}"),
        }

        let matching = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123:m7",
            "--match",
            "historical correction",
            "--context",
            "12",
        ])
        .unwrap();
        match matching.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Read(read),
            } => {
                assert_eq!(read.match_query.as_deref(), Some("historical correction"));
                assert_eq!(read.context, 12);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_read_slice_options_validate_arguments() {
        let conflict = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123:m7",
            "--lines",
            "1..2",
            "--match",
            "needle",
        ])
        .expect_err("line and match slices should conflict");
        assert!(conflict.to_string().contains("cannot be used with"));

        let missing_match = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123:m7",
            "--context",
            "2",
        ])
        .expect_err("context should require a match query");
        assert!(missing_match.to_string().contains("required"));

        let invalid_lines = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123:m7",
            "--lines",
            "4..2",
        ])
        .expect_err("reversed line range should fail");
        assert!(invalid_lines.to_string().contains("line range"));
    }

    #[test]
    fn agent_outline_captures_ref_and_display_options() {
        let args = Args::try_parse_from([
            "claude-history",
            "agent",
            "outline",
            "ch_abc123",
            "--no-budget",
            "--tools",
            "--tool-results",
            "--thinking",
            "--subagents",
        ])
        .unwrap();

        match args.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Outline(outline),
            } => {
                assert_eq!(outline.conversation, "ch_abc123");
                assert_eq!(outline.output.budget, None);
                assert!(outline.output.no_budget);
                assert!(outline.output.tools);
                assert!(outline.output.tool_results);
                assert!(outline.output.thinking);
                assert!(outline.output.subagents);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_mode_value_and_legacy_aliases_share_one_override() {
        let canonical = Args::try_parse_from([
            "claude-history",
            "agent",
            "search",
            "cache",
            "--mode",
            "hybrid",
        ])
        .unwrap();
        let legacy = Args::try_parse_from([
            "claude-history",
            "agent",
            "within",
            "ch_abc123",
            "cache",
            "--hybrid",
        ])
        .unwrap();

        match canonical.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Search(search),
            } => assert_eq!(search.mode_override(), Some(SearchMode::Hybrid)),
            other => panic!("unexpected command: {other:?}"),
        }
        match legacy.command.unwrap() {
            Commands::Agent {
                command: AgentCommand::Within(within),
            } => assert_eq!(within.mode_override(), Some(SearchMode::Hybrid)),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn agent_mode_value_conflicts_with_legacy_alias() {
        let err = Args::try_parse_from([
            "claude-history",
            "agent",
            "search",
            "cache",
            "--mode",
            "semantic",
            "--hybrid",
        ])
        .expect_err("canonical and legacy modes should conflict");

        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn top_level_one_shot_actions_share_one_conflict_group() {
        let cases = [
            vec!["claude-history", "--show-dir", "--delete", "session"],
            vec![
                "claude-history",
                "--debug-search",
                "cache",
                "--render",
                "session.jsonl",
            ],
            vec![
                "claude-history",
                "--generate-semantic-cache",
                "--clear-semantic-cache",
            ],
        ];

        for args in cases {
            let err = Args::try_parse_from(args).expect_err("one-shot actions should conflict");
            assert!(err.to_string().contains("cannot be used with"));
        }
    }

    #[test]
    fn display_one_shots_keep_compatible_selection_modifiers() {
        assert!(Args::try_parse_from(["claude-history", "--show-dir", "--plain"]).is_ok());
        assert!(
            Args::try_parse_from(["claude-history", "--render", "session.jsonl", "--resume",])
                .is_ok()
        );
    }

    #[test]
    fn agent_mode_flags_conflict() {
        let err = Args::try_parse_from([
            "claude-history",
            "agent",
            "search",
            "cache",
            "--semantic",
            "--hybrid",
        ])
        .expect_err("conflicting mode flags should fail");

        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn agent_budget_and_no_budget_conflict() {
        let err = Args::try_parse_from([
            "claude-history",
            "agent",
            "read",
            "ch_abc123",
            "--budget",
            "10",
            "--no-budget",
        ])
        .expect_err("budget and no-budget should conflict");

        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn agent_top_must_be_positive() {
        let err = Args::try_parse_from([
            "claude-history",
            "agent",
            "within",
            "ch_abc123",
            "cache",
            "--top",
            "0",
        ])
        .expect_err("zero top should fail");

        assert!(err.to_string().contains("value must be greater than zero"));
    }

    #[test]
    fn agent_budget_must_be_positive() {
        let err = Args::try_parse_from([
            "claude-history",
            "agent",
            "outline",
            "ch_abc123",
            "--budget",
            "0",
        ])
        .expect_err("zero budget should fail");

        assert!(err.to_string().contains("value must be greater than zero"));
    }

    #[test]
    fn agent_query_must_not_be_empty() {
        let err = Args::try_parse_from(["claude-history", "agent", "search", "   "])
            .expect_err("empty query should fail");

        assert!(err.to_string().contains("value must not be empty"));
    }

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
    fn agent_budget_help_uses_character_units() {
        let mut command = Args::command();
        let help = command
            .find_subcommand_mut("agent")
            .unwrap()
            .find_subcommand_mut("read")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("Output budget in Unicode characters"));
        assert!(!help.contains("approximate tokens"));
    }

    #[test]
    fn readme_cli_reference_lists_visible_subcommands() {
        let readme = include_str!("../README.md");
        let cli_reference = readme
            .split("### CLI reference")
            .nth(1)
            .expect("README CLI reference")
            .split("### Preview modes")
            .next()
            .expect("CLI reference boundary");

        for subcommand in Args::command()
            .get_subcommands()
            .filter(|subcommand| !subcommand.is_hide_set())
        {
            assert!(
                cli_reference.contains(&format!("  {}", subcommand.get_name())),
                "README CLI reference lacks {}",
                subcommand.get_name()
            );
        }
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
