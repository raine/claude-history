//! claude-history-query: Non-interactive CLI for querying Claude Code conversation history.
//!
//! This binary provides scriptable, pipeable access to conversation data without requiring
//! a TUI. It outputs JSONL or human-readable formats suitable for shell pipelines.

mod claude;
mod cli;
mod config;
mod debug;
mod debug_log;
mod display;
mod error;
mod history;
mod markdown;
mod pager;
mod syntax;
mod tool_format;
mod tui;

use clap::{Parser, Subcommand, ValueEnum};
use error::Result;

/// Output format for query results
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output (default)
    #[default]
    Human,
    /// JSON Lines format (one JSON object per line)
    Jsonl,
}

/// Non-interactive CLI for querying Claude Code conversation history.
///
/// Provides scriptable access to conversation data without requiring a TUI.
/// Outputs JSONL or human-readable formats suitable for shell pipelines.
#[derive(Parser, Debug)]
#[command(name = "claude-history-query")]
#[command(version)]
#[command(about = "Non-interactive CLI for querying Claude Code conversation history")]
pub struct Cli {
    /// Output in human-readable format (default)
    #[arg(long, global = true, group = "output_format")]
    pub human: bool,

    /// Output in JSON Lines format
    #[arg(long, global = true, group = "output_format")]
    pub jsonl: bool,

    /// Suppress all non-error output (exit code only)
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// Determine the output format from flags
    pub fn output_format(&self) -> OutputFormat {
        if self.jsonl {
            OutputFormat::Jsonl
        } else {
            OutputFormat::Human
        }
    }
}

/// Available subcommands
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List conversations matching criteria
    List(ListArgs),
    /// Show conversation content
    Show(ShowArgs),
    /// Show usage statistics
    Usage,
}

/// Arguments for the list subcommand
#[derive(Parser, Debug)]
pub struct ListArgs {
    /// Search all conversations globally (not just current directory)
    #[arg(long, short = 'g')]
    pub global: bool,

    /// Show conversations since duration ago (e.g., 2d, 1w, 3h)
    #[arg(long, short = 's', value_name = "DURATION")]
    pub since: Option<String>,

    /// Show conversations after date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub after: Option<String>,

    /// Show conversations before date (YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub before: Option<String>,

    /// Include paths matching regex (can be repeated)
    #[arg(long, action = clap::ArgAction::Append, value_name = "PATTERN")]
    pub include_path: Vec<String>,

    /// Exclude paths matching regex (can be repeated)
    #[arg(long, action = clap::ArgAction::Append, value_name = "PATTERN")]
    pub exclude_path: Vec<String>,

    /// Boolean search query (e.g., 'rust && !deprecated')
    #[arg(long, value_name = "QUERY")]
    pub query: Option<String>,

    /// Fields to output (can be repeated)
    /// Available: id, path, project, summary, message_count, first_ts, last_ts, duration
    #[arg(long, short = 'f', action = clap::ArgAction::Append, value_name = "FIELD")]
    pub field: Vec<String>,

    /// Maximum number of results to return
    #[arg(long, short = 'n', value_name = "COUNT")]
    pub limit: Option<usize>,

    /// Sort order: newest, oldest, most-messages, least-messages
    #[arg(long, value_name = "ORDER", default_value = "newest")]
    pub sort: String,
}

/// Arguments for the show subcommand
#[derive(Parser, Debug)]
pub struct ShowArgs {
    /// Conversation identifier (path, ID, or index from list)
    pub identifier: String,

    /// Output format for messages
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,

    /// Include tool calls in output
    #[arg(long, short = 't')]
    pub tools: bool,

    /// Include thinking blocks in output
    #[arg(long)]
    pub thinking: bool,

    /// Only show messages after this timestamp (ISO 8601 or Unix)
    #[arg(long, value_name = "TIMESTAMP")]
    pub ts_after: Option<String>,

    /// Only show messages before this timestamp (ISO 8601 or Unix)
    #[arg(long, value_name = "TIMESTAMP")]
    pub ts_before: Option<String>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match &cli.command {
        Commands::List(args) => {
            if !cli.quiet {
                eprintln!("list command: global={}, since={:?}, query={:?}, limit={:?}, sort={}",
                    args.global, args.since, args.query, args.limit, args.sort);
            }
            // TODO: Implement list command
            Ok(())
        }
        Commands::Show(args) => {
            if !cli.quiet {
                eprintln!("show command: identifier={}, tools={}, thinking={}",
                    args.identifier, args.tools, args.thinking);
            }
            // TODO: Implement show command
            Ok(())
        }
        Commands::Usage => {
            if !cli.quiet {
                eprintln!("usage command");
            }
            // TODO: Implement usage command
            Ok(())
        }
    }
}
