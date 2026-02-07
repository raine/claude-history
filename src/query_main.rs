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
use claude_history::path_filter::PathFilter;
use claude_history::query::{evaluate, parse_query};
use claude_history::time_filter::TimeFilter;
use error::{AppError, Result};
use history::Conversation;
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

/// Exit code when no results are found
const EXIT_NO_RESULTS: u8 = 3;

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
    /// Available: uuid, path, project, cwd, timestamp, preview
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

/// Output structure for a conversation in JSON format
#[derive(Serialize)]
struct ConversationOutput {
    uuid: String,
    path: String,
    cwd: Option<String>,
    timestamp: String,
    preview: String,
    project: Option<String>,
}

impl From<&Conversation> for ConversationOutput {
    fn from(conv: &Conversation) -> Self {
        Self {
            uuid: conv
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string(),
            path: conv.path.display().to_string(),
            cwd: conv.cwd.as_ref().map(|p| p.display().to_string()),
            timestamp: conv.timestamp.to_rfc3339(),
            preview: conv.preview.clone(),
            project: conv.project_name.clone(),
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {}", e);
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let format = cli.output_format();

    match cli.command {
        Commands::List(args) => run_list(args, format, cli.quiet),
        Commands::Show(args) => run_show(args, format),
        Commands::Usage => {
            if !cli.quiet {
                eprintln!("usage command");
            }
            // TODO: Implement usage command
            Ok(())
        }
    }
}

/// Build a time filter from command line arguments
fn build_time_filter(args: &ListArgs) -> Result<Option<TimeFilter>> {
    let mut filter = TimeFilter::new();
    let mut has_filter = false;

    if let Some(ref since) = args.since {
        filter = filter.with_since(since).map_err(|e| {
            AppError::InvalidArgs(format!("invalid --since value '{}': {}", since, e))
        })?;
        has_filter = true;
    }

    if let Some(ref after) = args.after {
        filter = filter.with_after(after).map_err(|e| {
            AppError::InvalidArgs(format!("invalid --after value '{}': {}", after, e))
        })?;
        has_filter = true;
    }

    if let Some(ref before) = args.before {
        filter = filter.with_before(before).map_err(|e| {
            AppError::InvalidArgs(format!("invalid --before value '{}': {}", before, e))
        })?;
        has_filter = true;
    }

    if has_filter {
        Ok(Some(filter))
    } else {
        Ok(None)
    }
}

/// Build a path filter from command line arguments
fn build_path_filter(args: &ListArgs) -> Result<Option<PathFilter>> {
    if args.include_path.is_empty() && args.exclude_path.is_empty() {
        return Ok(None);
    }

    let filter = PathFilter::from_patterns(&args.include_path, &args.exclude_path).map_err(|e| {
        AppError::InvalidArgs(format!("invalid path pattern: {}", e))
    })?;

    Ok(Some(filter))
}

/// Run the list command
fn run_list(args: ListArgs, format: OutputFormat, quiet: bool) -> Result<()> {
    // Build filters from args
    let time_filter = build_time_filter(&args)?;
    let path_filter = build_path_filter(&args)?;

    // Parse content query if provided
    let query_expr = if let Some(ref query_str) = args.query {
        Some(parse_query(query_str).map_err(|e| {
            AppError::InvalidArgs(format!("invalid query '{}': {}", query_str, e))
        })?)
    } else {
        None
    };

    // Load conversations based on global flag
    let mut conversations = if args.global {
        history::load_all_conversations(
            false, // show_last
            None,  // debug_level
            time_filter.as_ref(),
            path_filter.as_ref(),
        )?
    } else {
        // Load from current directory's project
        let current_dir = std::env::current_dir().map_err(|e| {
            AppError::Io(e)
        })?;
        let projects_dir = history::get_claude_projects_dir(&current_dir)?;

        if !projects_dir.exists() {
            if !quiet {
                eprintln!("No Claude history found for current directory");
            }
            std::process::exit(EXIT_NO_RESULTS as i32);
        }

        history::load_conversations(
            &projects_dir,
            false, // show_last
            None,  // debug_level
            time_filter.as_ref(),
            path_filter.as_ref(),
        )?
    };

    // Apply content query filter
    if let Some(ref expr) = query_expr {
        conversations.retain(|conv| evaluate(expr, &conv.full_text));
    }

    // Apply sort
    match args.sort.as_str() {
        "newest" => {
            conversations.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        }
        "oldest" => {
            conversations.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        }
        "most-messages" => {
            conversations.sort_by(|a, b| b.message_count.cmp(&a.message_count));
        }
        "least-messages" => {
            conversations.sort_by(|a, b| a.message_count.cmp(&b.message_count));
        }
        other => {
            return Err(AppError::InvalidArgs(format!(
                "invalid sort order '{}': expected newest, oldest, most-messages, or least-messages",
                other
            )));
        }
    }

    // Apply limit
    if let Some(limit) = args.limit {
        conversations.truncate(limit);
    }

    // Check if empty
    if conversations.is_empty() {
        if !quiet {
            eprintln!("No conversations found");
        }
        std::process::exit(EXIT_NO_RESULTS as i32);
    }

    // Output results
    output_list(&conversations, &args.field, format);

    Ok(())
}

/// Output the list of conversations
fn output_list(conversations: &[Conversation], fields: &[String], format: OutputFormat) {
    for conv in conversations {
        let out = ConversationOutput::from(conv);

        match format {
            OutputFormat::Jsonl => {
                let value = if fields.is_empty() {
                    serde_json::to_value(&out).unwrap()
                } else {
                    filter_fields(&out, fields)
                };
                println!("{}", serde_json::to_string(&value).unwrap());
            }
            OutputFormat::Human => {
                if fields.is_empty() {
                    // Default human output with relative time
                    let relative_time = chrono_humanize::HumanTime::from(conv.timestamp);
                    println!(
                        "{} ({}) - {}",
                        out.uuid,
                        relative_time,
                        out.preview
                    );
                } else {
                    // Custom field output
                    let values: Vec<String> = fields
                        .iter()
                        .map(|f| get_field(&out, f))
                        .collect();
                    println!("{}", values.join("\t"));
                }
            }
        }
    }
}

/// Filter output to specific fields
fn filter_fields(out: &ConversationOutput, fields: &[String]) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    for field in fields {
        match field.as_str() {
            "uuid" => {
                map.insert("uuid".to_string(), serde_json::Value::String(out.uuid.clone()));
            }
            "path" => {
                map.insert("path".to_string(), serde_json::Value::String(out.path.clone()));
            }
            "cwd" => {
                map.insert(
                    "cwd".to_string(),
                    out.cwd
                        .as_ref()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            "timestamp" => {
                map.insert(
                    "timestamp".to_string(),
                    serde_json::Value::String(out.timestamp.clone()),
                );
            }
            "preview" => {
                map.insert(
                    "preview".to_string(),
                    serde_json::Value::String(out.preview.clone()),
                );
            }
            "project" => {
                map.insert(
                    "project".to_string(),
                    out.project
                        .as_ref()
                        .map(|s| serde_json::Value::String(s.clone()))
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            _ => {
                // Unknown field, skip silently
            }
        }
    }

    serde_json::Value::Object(map)
}

/// Get a single field value as a string
fn get_field(out: &ConversationOutput, field: &str) -> String {
    match field {
        "uuid" => out.uuid.clone(),
        "path" => out.path.clone(),
        "cwd" => out.cwd.clone().unwrap_or_default(),
        "timestamp" => out.timestamp.clone(),
        "preview" => out.preview.clone(),
        "project" => out.project.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// Find a conversation file by its UUID across all projects.
///
/// Searches all project directories for a file matching the given UUID.
/// Matches if the file stem equals the uuid or starts with `{uuid}-`.
fn find_conversation_by_id(uuid: &str) -> Result<PathBuf> {
    let projects_root = history::get_claude_projects_root()?;

    if !projects_root.exists() {
        return Err(AppError::NoHistoryFound(format!(
            "Projects directory does not exist: {}",
            projects_root.display()
        )));
    }

    // Iterate through all project directories
    for entry in std::fs::read_dir(&projects_root)? {
        let entry = entry?;
        let project_path = entry.path();

        if !project_path.is_dir() {
            continue;
        }

        // Search for JSONL files in this project
        for file_entry in std::fs::read_dir(&project_path)? {
            let file_entry = file_entry?;
            let file_path = file_entry.path();

            if file_path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }

            if let Some(stem) = file_path.file_stem().and_then(|s| s.to_str()) {
                // Match exact UUID or UUID prefix (for files like uuid-timestamp.jsonl)
                if stem == uuid || stem.starts_with(&format!("{}-", uuid)) {
                    return Ok(file_path);
                }
            }
        }
    }

    Err(AppError::NoHistoryFound(format!(
        "No conversation found with UUID: {}",
        uuid
    )))
}

/// Run the show command to display conversation content
fn run_show(args: ShowArgs, format: OutputFormat) -> Result<()> {
    // Resolve the conversation path
    let path = if args.identifier.contains('/') || args.identifier.contains('.') {
        // Treat as a file path
        PathBuf::from(&args.identifier)
    } else {
        // Treat as a UUID and search for it
        find_conversation_by_id(&args.identifier)?
    };

    // Check if file exists
    if !path.exists() {
        return Err(AppError::NoHistoryFound(format!(
            "Conversation file not found: {}",
            path.display()
        )));
    }

    // Open and read the file
    let file = File::open(&path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // Parse the JSON line
        let entry: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // Skip malformed lines
        };

        // Get message type
        let msg_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // Skip non-message types
        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        // Get message content
        let message = match entry.get("message") {
            Some(m) => m,
            None => continue,
        };

        let role = message.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let content = match message.get("content") {
            Some(c) => c,
            None => continue,
        };

        // Filter tool_use and tool_result unless --tools is specified
        if !args.tools {
            if let Some(blocks) = content.as_array() {
                // Check if content is only tool_use/tool_result
                let has_text = blocks.iter().any(|block| {
                    block
                        .get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| t == "text" || (args.thinking && t == "thinking"))
                        .unwrap_or(false)
                });
                if !has_text && !blocks.is_empty() {
                    continue;
                }
            }
        }

        match format {
            OutputFormat::Jsonl => {
                // Print the raw line
                println!("{}", line);
            }
            OutputFormat::Human => {
                // Print role header and content
                let header = match role {
                    "user" => "## User",
                    "assistant" => "## Assistant",
                    _ => "## Unknown",
                };
                println!("\n{}\n", header);
                print_content(content, args.tools, args.thinking);
            }
        }
    }

    Ok(())
}

/// Print content from a message, handling both string and array formats
fn print_content(content: &serde_json::Value, include_tools: bool, include_thinking: bool) {
    match content {
        serde_json::Value::String(s) => {
            println!("{}", s);
        }
        serde_json::Value::Array(blocks) => {
            for block in blocks {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            println!("{}", text);
                        }
                    }
                    "thinking" if include_thinking => {
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            println!("<thinking>\n{}\n</thinking>", thinking);
                        }
                    }
                    "tool_use" if include_tools => {
                        if let Some(name) = block.get("name").and_then(|n| n.as_str()) {
                            println!("<tool_use name=\"{}\">", name);
                            if let Some(input) = block.get("input") {
                                println!(
                                    "{}",
                                    serde_json::to_string_pretty(input).unwrap_or_default()
                                );
                            }
                            println!("</tool_use>");
                        }
                    }
                    "tool_result" if include_tools => {
                        println!("<tool_result>");
                        if let Some(result_content) = block.get("content") {
                            match result_content {
                                serde_json::Value::String(s) => println!("{}", s),
                                serde_json::Value::Array(items) => {
                                    for item in items {
                                        if let Some(text) =
                                            item.get("text").and_then(|t| t.as_str())
                                        {
                                            println!("{}", text);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        println!("</tool_result>");
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
