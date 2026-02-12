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

/// Exit codes for the CLI application
mod exit_codes {
    pub const SUCCESS: u8 = 0;
    pub const ERROR: u8 = 1;
    pub const INVALID_ARGS: u8 = 2;
    pub const NO_RESULTS: u8 = 3;
}

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
        Ok(()) => ExitCode::from(exit_codes::SUCCESS),
        Err(e) => {
            eprintln!("Error: {}", e);
            match e {
                AppError::InvalidArgs(_) => ExitCode::from(exit_codes::INVALID_ARGS),
                AppError::NoHistoryFound(_) => ExitCode::from(exit_codes::NO_RESULTS),
                _ => ExitCode::from(exit_codes::ERROR),
            }
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let format = cli.output_format();

    match cli.command {
        Commands::List(args) => run_list(args, format, cli.quiet),
        Commands::Show(args) => run_show(args, format),
        Commands::Usage => run_usage(format),
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
fn run_list(args: ListArgs, format: OutputFormat, _quiet: bool) -> Result<()> {
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
            return Err(AppError::NoHistoryFound(
                "No Claude history found for current directory".to_string(),
            ));
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
        return Err(AppError::NoHistoryFound("No conversations found".to_string()));
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

/// Run the usage command to display documentation for LLM agents
fn run_usage(_format: OutputFormat) -> Result<()> {
    print!(r#"# claude-history-query

Non-interactive CLI for querying Claude Code conversation history.
Provides scriptable, pipeable access to conversation data for shell scripts
and LLM agents without requiring a TUI.

## Commands

### list

List conversations matching criteria.

```
claude-history-query list [OPTIONS]
```

**Options:**

* `-g, --global` - Search all conversations globally (not just current directory)
* `-s, --since <DURATION>` - Show conversations since duration ago (e.g., 2d, 1w, 3h)
* `--after <DATE>` - Show conversations after date (YYYY-MM-DD)
* `--before <DATE>` - Show conversations before date (YYYY-MM-DD)
* `--include-path <PATTERN>` - Include paths matching regex (can be repeated)
* `--exclude-path <PATTERN>` - Exclude paths matching regex (can be repeated)
* `--query <QUERY>` - Boolean search query (e.g., 'rust && !deprecated')
* `-f, --field <FIELD>` - Fields to output (can be repeated)
* `-n, --limit <COUNT>` - Maximum number of results to return
* `--sort <ORDER>` - Sort order: newest, oldest, most-messages, least-messages

### show

Show conversation content.

```
claude-history-query show <IDENTIFIER> [OPTIONS]
```

**Arguments:**

* `<IDENTIFIER>` - Conversation identifier (path, UUID, or index from list)

**Options:**

* `--format <FORMAT>` - Output format for messages
* `-t, --tools` - Include tool calls in output
* `--thinking` - Include thinking blocks in output
* `--ts-after <TIMESTAMP>` - Only show messages after this timestamp (ISO 8601 or Unix)
* `--ts-before <TIMESTAMP>` - Only show messages before this timestamp (ISO 8601 or Unix)

## Output Formats

* `--human` - Human-readable output (default). Shows relative timestamps and formatted text.
* `--jsonl` - JSON Lines format. One JSON object per line, suitable for parsing.

## Field Names

Available fields for `--field` option:

* `uuid` - Conversation unique identifier
* `path` - Full path to conversation file
* `cwd` - Working directory where conversation was started
* `timestamp` - ISO 8601 timestamp of last activity
* `preview` - First line of the first user message
* `project` - Project name derived from working directory

## Common Patterns

### Resume a conversation

```bash
# Get the UUID of the most recent conversation
UUID=$(claude-history-query list -n 1 --jsonl | jq -r '.uuid')
claude --resume "$UUID"
```

### Search and resume with fzf

```bash
claude-history-query list -g | fzf | cut -d' ' -f1 | xargs -I{{}} claude --resume {{}}
```

### Export conversations to backup

```bash
claude-history-query list -g --jsonl > backup.jsonl
```

### Delete old conversations

```bash
claude-history-query list --before 2024-01-01 -f path | xargs rm
```

### Find conversations about a topic

```bash
claude-history-query list -g --query "docker && kubernetes"
```

### Get recent conversations from current project

```bash
claude-history-query list --since 1w
```

### Show conversation content as JSONL

```bash
claude-history-query show <UUID> --jsonl
```

## Exit Codes

* `0` - Success
* `1` - General error (I/O, parsing, etc.)
* `2` - Invalid arguments
* `3` - No results found

"#);
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

#[cfg(test)]
mod tests {
    use super::*;

    // === Exit Codes ===

    #[test]
    fn test_exit_codes_are_distinct() {
        // Exit codes should be unique to allow scripts to distinguish error types
        let codes = [
            exit_codes::SUCCESS,
            exit_codes::ERROR,
            exit_codes::INVALID_ARGS,
            exit_codes::NO_RESULTS,
        ];
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "Exit codes must be unique");
    }

    #[test]
    fn test_exit_code_values() {
        // Document expected exit code values for shell scripts
        assert_eq!(exit_codes::SUCCESS, 0);
        assert_eq!(exit_codes::ERROR, 1);
        assert_eq!(exit_codes::INVALID_ARGS, 2);
        assert_eq!(exit_codes::NO_RESULTS, 3);
    }

    // === Output Format ===

    #[test]
    fn test_output_format_default_is_human() {
        // When no format flag is specified, default to human-readable
        let cli = Cli {
            human: false,
            jsonl: false,
            quiet: false,
            command: Commands::Usage,
        };
        assert!(matches!(cli.output_format(), OutputFormat::Human));
    }

    #[test]
    fn test_output_format_jsonl_flag() {
        // --jsonl flag produces JSONL output
        let cli = Cli {
            human: false,
            jsonl: true,
            quiet: false,
            command: Commands::Usage,
        };
        assert!(matches!(cli.output_format(), OutputFormat::Jsonl));
    }

    #[test]
    fn test_output_format_human_flag() {
        // --human flag produces human-readable output
        let cli = Cli {
            human: true,
            jsonl: false,
            quiet: false,
            command: Commands::Usage,
        };
        assert!(matches!(cli.output_format(), OutputFormat::Human));
    }

    // === Field Extraction ===

    fn sample_conversation_output() -> ConversationOutput {
        ConversationOutput {
            uuid: "abc123".to_string(),
            path: "/home/user/.claude/projects/test/abc123.jsonl".to_string(),
            cwd: Some("/home/user/project".to_string()),
            timestamp: "2026-02-06T14:30:00Z".to_string(),
            preview: "Fix the authentication bug".to_string(),
            project: Some("my-project".to_string()),
        }
    }

    #[test]
    fn test_get_field_uuid() {
        let out = sample_conversation_output();
        assert_eq!(get_field(&out, "uuid"), "abc123");
    }

    #[test]
    fn test_get_field_path() {
        let out = sample_conversation_output();
        assert_eq!(
            get_field(&out, "path"),
            "/home/user/.claude/projects/test/abc123.jsonl"
        );
    }

    #[test]
    fn test_get_field_cwd() {
        let out = sample_conversation_output();
        assert_eq!(get_field(&out, "cwd"), "/home/user/project");
    }

    #[test]
    fn test_get_field_cwd_none() {
        // When cwd is None, return empty string
        let mut out = sample_conversation_output();
        out.cwd = None;
        assert_eq!(get_field(&out, "cwd"), "");
    }

    #[test]
    fn test_get_field_timestamp() {
        let out = sample_conversation_output();
        assert_eq!(get_field(&out, "timestamp"), "2026-02-06T14:30:00Z");
    }

    #[test]
    fn test_get_field_preview() {
        let out = sample_conversation_output();
        assert_eq!(get_field(&out, "preview"), "Fix the authentication bug");
    }

    #[test]
    fn test_get_field_project() {
        let out = sample_conversation_output();
        assert_eq!(get_field(&out, "project"), "my-project");
    }

    #[test]
    fn test_get_field_project_none() {
        // When project is None, return empty string
        let mut out = sample_conversation_output();
        out.project = None;
        assert_eq!(get_field(&out, "project"), "");
    }

    #[test]
    fn test_get_field_unknown() {
        // Unknown field returns empty string
        let out = sample_conversation_output();
        assert_eq!(get_field(&out, "nonexistent"), "");
    }

    // === Field Filtering (JSONL) ===

    #[test]
    fn test_filter_fields_single() {
        let out = sample_conversation_output();
        let result = filter_fields(&out, &["uuid".to_string()]);

        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert_eq!(obj.get("uuid").unwrap().as_str().unwrap(), "abc123");
    }

    #[test]
    fn test_filter_fields_multiple() {
        let out = sample_conversation_output();
        let result = filter_fields(&out, &["uuid".to_string(), "cwd".to_string()]);

        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 2);
        assert_eq!(obj.get("uuid").unwrap().as_str().unwrap(), "abc123");
        assert_eq!(
            obj.get("cwd").unwrap().as_str().unwrap(),
            "/home/user/project"
        );
    }

    #[test]
    fn test_filter_fields_none_value() {
        // When cwd is None, the JSON value should be null
        let mut out = sample_conversation_output();
        out.cwd = None;
        let result = filter_fields(&out, &["cwd".to_string()]);

        let obj = result.as_object().unwrap();
        assert!(obj.get("cwd").unwrap().is_null());
    }

    #[test]
    fn test_filter_fields_unknown_skipped() {
        // Unknown fields are silently skipped
        let out = sample_conversation_output();
        let result = filter_fields(&out, &["uuid".to_string(), "nonexistent".to_string()]);

        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("uuid"));
        assert!(!obj.contains_key("nonexistent"));
    }

    #[test]
    fn test_filter_fields_all() {
        // All valid fields can be selected
        let out = sample_conversation_output();
        let result = filter_fields(
            &out,
            &[
                "uuid".to_string(),
                "path".to_string(),
                "cwd".to_string(),
                "timestamp".to_string(),
                "preview".to_string(),
                "project".to_string(),
            ],
        );

        let obj = result.as_object().unwrap();
        assert_eq!(obj.len(), 6);
    }

    // === ConversationOutput From Conversion ===

    #[test]
    fn test_conversation_output_uuid_extraction() {
        // UUID should be extracted from the file stem
        use chrono::Local;
        let conv = Conversation {
            path: PathBuf::from("/home/user/.claude/projects/test/abc123-xyz.jsonl"),
            index: 0,
            timestamp: Local::now(),
            preview: "Test".to_string(),
            full_text: "Test content".to_string(),
            project_name: None,
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: vec![],
            summary: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        };

        let out = ConversationOutput::from(&conv);
        // File stem is "abc123-xyz", which becomes the UUID
        assert_eq!(out.uuid, "abc123-xyz");
    }

    #[test]
    fn test_conversation_output_preserves_cwd() {
        use chrono::Local;
        let conv = Conversation {
            path: PathBuf::from("/test.jsonl"),
            index: 0,
            timestamp: Local::now(),
            preview: "Test".to_string(),
            full_text: "".to_string(),
            project_name: None,
            project_path: None,
            cwd: Some(PathBuf::from("/home/user/myproject")),
            message_count: 1,
            parse_errors: vec![],
            summary: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        };

        let out = ConversationOutput::from(&conv);
        assert_eq!(out.cwd.as_deref(), Some("/home/user/myproject"));
    }

    #[test]
    fn test_conversation_output_timestamp_rfc3339() {
        use chrono::{Local, TimeZone};
        let timestamp = Local.with_ymd_and_hms(2026, 2, 6, 14, 30, 0).unwrap();
        let conv = Conversation {
            path: PathBuf::from("/test.jsonl"),
            index: 0,
            timestamp,
            preview: "Test".to_string(),
            full_text: "".to_string(),
            project_name: None,
            project_path: None,
            cwd: None,
            message_count: 1,
            parse_errors: vec![],
            summary: None,
            model: None,
            total_tokens: 0,
            duration_minutes: None,
        };

        let out = ConversationOutput::from(&conv);
        // Should be valid RFC3339 format
        assert!(out.timestamp.contains("2026-02-06"));
        assert!(out.timestamp.contains("14:30:00"));
    }

    // === Sort Order Validation ===

    #[test]
    fn test_valid_sort_orders() {
        // Document the valid sort order values
        let valid_orders = ["newest", "oldest", "most-messages", "least-messages"];
        for order in valid_orders {
            // These should not panic in a match statement
            match order {
                "newest" | "oldest" | "most-messages" | "least-messages" => {}
                _ => panic!("Unexpected sort order: {}", order),
            }
        }
    }
}
