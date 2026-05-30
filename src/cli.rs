use crate::search::mode::SearchMode;
use clap::{Args as ClapArgs, Parser, Subcommand};
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
    /// Update claude-history to the latest version
    Update,
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

#[derive(Debug, ClapArgs)]
#[command(group = clap::ArgGroup::new("agent_search_mode").args(["lexical", "semantic", "exact", "hybrid"]).multiple(false))]
pub struct AgentSearchArgs {
    /// Search query
    #[arg(value_parser = non_empty_string)]
    pub query: String,
    /// Maximum number of results
    #[arg(long, default_value_t = 10, value_parser = non_zero_usize)]
    pub top: usize,
    /// Return flat message-hit results instead of grouped conversation results
    #[arg(long)]
    pub flat: bool,
    /// Maximum evidence hits to show per conversation in grouped output
    #[arg(long, default_value_t = 2, value_parser = non_zero_usize)]
    pub hits_per_conv: usize,
    /// Disable duplicate suppression inside grouped conversation results
    #[arg(long)]
    pub all_hits: bool,
    /// Search only the current workspace
    #[arg(long, group = "agent_search_scope")]
    pub local: bool,
    /// Search all workspaces
    #[arg(long, group = "agent_search_scope")]
    pub all: bool,
    /// Use lexical search for this invocation
    #[arg(long, group = "agent_search_mode")]
    pub lexical: bool,
    /// Use semantic search for this invocation
    #[arg(long, group = "agent_search_mode")]
    pub semantic: bool,
    /// Use exact search for this invocation
    #[arg(long, group = "agent_search_mode")]
    pub exact: bool,
    /// Use hybrid search for this invocation
    #[arg(long, group = "agent_search_mode")]
    pub hybrid: bool,
}

#[derive(Debug, ClapArgs)]
#[command(group = clap::ArgGroup::new("agent_within_mode").args(["lexical", "semantic", "exact", "hybrid"]).multiple(false))]
pub struct AgentWithinArgs {
    /// Conversation reference
    #[arg(value_parser = non_empty_string)]
    pub conversation: String,
    /// Search query
    #[arg(value_parser = non_empty_string)]
    pub query: String,
    /// Maximum number of results
    #[arg(long, default_value_t = 20, value_parser = non_zero_usize)]
    pub top: usize,
    /// Use lexical search for this invocation
    #[arg(long, group = "agent_within_mode")]
    pub lexical: bool,
    /// Use semantic search for this invocation
    #[arg(long, group = "agent_within_mode")]
    pub semantic: bool,
    /// Use exact search for this invocation
    #[arg(long, group = "agent_within_mode")]
    pub exact: bool,
    /// Use hybrid search for this invocation
    #[arg(long, group = "agent_within_mode")]
    pub hybrid: bool,
}

#[derive(Debug, ClapArgs)]
pub struct AgentReadArgs {
    /// Conversation or message range refs to read
    #[arg(required = true, value_parser = non_empty_string)]
    pub refs: Vec<String>,
    /// Message or range to prioritize when budgeted output is truncated
    #[arg(long, value_parser = non_empty_string)]
    pub focus: Option<String>,
    /// Output budget in approximate tokens
    #[arg(long, default_value_t = 6000, value_parser = non_zero_usize, conflicts_with = "no_budget")]
    pub budget: usize,
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
pub struct AgentOutlineArgs {
    /// Conversation reference to outline
    #[arg(value_parser = non_empty_string)]
    pub conversation: String,
    /// Output budget in approximate tokens
    #[arg(long, default_value_t = 6000, value_parser = non_zero_usize, conflicts_with = "no_budget")]
    pub budget: usize,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AgentScope {
    #[default]
    Global,
    Local,
}

impl AgentSearchArgs {
    pub fn scope(&self) -> AgentScope {
        if self.local {
            AgentScope::Local
        } else {
            AgentScope::Global
        }
    }

    pub fn mode_override(&self) -> Option<SearchMode> {
        agent_mode_override(self.lexical, self.semantic, self.exact, self.hybrid)
    }
}

impl AgentWithinArgs {
    pub fn mode_override(&self) -> Option<SearchMode> {
        agent_mode_override(self.lexical, self.semantic, self.exact, self.hybrid)
    }
}

fn agent_mode_override(
    lexical: bool,
    semantic: bool,
    exact: bool,
    hybrid: bool,
) -> Option<SearchMode> {
    if lexical {
        Some(SearchMode::Lexical)
    } else if semantic {
        Some(SearchMode::Semantic)
    } else if exact {
        Some(SearchMode::Exact)
    } else if hybrid {
        Some(SearchMode::Hybrid)
    } else {
        None
    }
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
                assert_eq!(search.top, 7);
                assert!(!search.flat);
                assert_eq!(search.hits_per_conv, 2);
                assert!(!search.all_hits);
                assert_eq!(search.scope(), AgentScope::Local);
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
                assert_eq!(search.top, 10);
                assert!(!search.flat);
                assert_eq!(search.hits_per_conv, 2);
                assert!(!search.all_hits);
                assert_eq!(search.scope(), AgentScope::Global);
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
                assert_eq!(search.hits_per_conv, 5);
                assert!(search.all_hits);
            }
            other => panic!("unexpected command: {other:?}"),
        }
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
                assert_eq!(within.top, 4);
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
                assert_eq!(read.focus.as_deref(), Some("m2"));
                assert_eq!(read.budget, 1234);
                assert!(!read.no_budget);
                assert!(read.tools);
                assert!(read.tool_results);
                assert!(read.thinking);
                assert!(read.subagents);
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
                assert_eq!(read.budget, 6000);
                assert!(!read.no_budget);
                assert!(!read.tools);
                assert!(!read.tool_results);
                assert!(!read.thinking);
                assert!(!read.subagents);
            }
            other => panic!("unexpected command: {other:?}"),
        }
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
                assert_eq!(outline.budget, 6000);
                assert!(outline.no_budget);
                assert!(outline.tools);
                assert!(outline.tool_results);
                assert!(outline.thinking);
                assert!(outline.subagents);
            }
            other => panic!("unexpected command: {other:?}"),
        }
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
