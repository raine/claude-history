# claude-history-query Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create a programmatic CLI binary for searching Claude Code conversation history, optimized for scripts and LLM agents.

**Architecture:** New binary `src/query_main.rs` sharing internal modules with existing `src/main.rs`. Uses same `history`, `error`, `cli` modules. Clap-based CLI with three commands: `list`, `show`, `usage`. Output in human-readable or JSONL format.

**Tech Stack:** Rust, clap (derive), serde_json, existing internal modules.

**Key Insight:** Place binary at `src/query_main.rs` (not `src/bin/`) so it's part of the same crate and can access all internal modules directly via `crate::history`, `crate::error`, etc.

---

## Task 1: Add Binary Scaffold

**Files:**

* Modify: `Cargo.toml:17-19`
* Create: `src/query_main.rs`

**Step 1: Add [[bin]] entry to Cargo.toml**

Add after line 19 (after existing `[[bin]]` block):

```toml
[[bin]]
name = "claude-history-query"
path = "src/query_main.rs"
```

**Step 2: Create minimal binary scaffold**

Create `src/query_main.rs`:

```rust
//! claude-history-query: Programmatic CLI for conversation search
//!
//! Designed for scripts and LLM agents with structured output.

mod claude;
mod cli;
mod config;
mod debug;
mod debug_log;
mod error;
mod history;

use clap::{Parser, Subcommand, ValueEnum};
use error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
enum OutputFormat {
    #[default]
    Human,
    Jsonl,
}

#[derive(Parser)]
#[command(name = "claude-history-query")]
#[command(about = "Search Claude Code conversation history (programmatic CLI)")]
#[command(version)]
struct Cli {
    /// Output format: human (default) or jsonl
    #[arg(long, value_enum, global = true, default_value_t = OutputFormat::Human)]
    human: bool,

    /// Output in JSONL format (one JSON object per line)
    #[arg(long, global = true)]
    jsonl: bool,

    /// Suppress non-essential output
    #[arg(long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List conversations with filters
    List(ListArgs),
    /// Output conversation content
    Show(ShowArgs),
    /// Print documentation for LLM agents
    Usage,
}

#[derive(clap::Args)]
struct ListArgs {
    /// Search all projects globally
    #[arg(long, short = 'g')]
    global: bool,

    /// Filter by duration (e.g., 2d, 1w, 3h)
    #[arg(long, short = 's', value_name = "DURATION")]
    since: Option<String>,

    /// Filter conversations after date/time
    #[arg(long, value_name = "DATE|TIME")]
    after: Option<String>,

    /// Filter conversations before date/time
    #[arg(long, value_name = "DATE|TIME")]
    before: Option<String>,

    /// Include paths matching regex
    #[arg(long, action = clap::ArgAction::Append, value_name = "PATTERN")]
    include_path: Vec<String>,

    /// Exclude paths matching regex
    #[arg(long, action = clap::ArgAction::Append, value_name = "PATTERN")]
    exclude_path: Vec<String>,

    /// Boolean content query
    #[arg(long, short = 'q', value_name = "QUERY")]
    query: Option<String>,

    /// Output only specified field(s): uuid, path, cwd, timestamp, preview, project
    #[arg(long, action = clap::ArgAction::Append, value_name = "FIELD")]
    field: Vec<String>,

    /// Limit results to N conversations
    #[arg(long, value_name = "N")]
    limit: Option<usize>,

    /// Sort order
    #[arg(long, default_value = "newest", value_parser = ["newest", "oldest"])]
    sort: String,
}

#[derive(clap::Args)]
struct ShowArgs {
    /// UUID or path to conversation
    identifier: String,

    /// Output format: markdown, plain, raw
    #[arg(long, default_value = "markdown")]
    format: String,

    /// Include tool calls
    #[arg(long)]
    tools: bool,

    /// Include thinking blocks
    #[arg(long)]
    thinking: bool,

    /// Only messages after this time
    #[arg(long, value_name = "TIME")]
    ts_after: Option<String>,

    /// Only messages before this time
    #[arg(long, value_name = "TIME")]
    ts_before: Option<String>,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    let format = if cli.jsonl {
        OutputFormat::Jsonl
    } else {
        OutputFormat::Human
    };

    match cli.command {
        Commands::List(args) => {
            eprintln!("list: global={}, format={:?} (not implemented)", args.global, format);
        }
        Commands::Show(args) => {
            eprintln!("show: {} (not implemented)", args.identifier);
        }
        Commands::Usage => {
            eprintln!("usage (not implemented)");
        }
    }

    Ok(())
}
```

**Step 3: Verify it compiles**

Run: `cargo build --bin claude-history-query`
Expected: Compiles successfully

**Step 4: Verify help works**

Run: `cargo run --bin claude-history-query -- --help`
Expected: Shows help with list, show, usage subcommands

**Step 5: Commit**

```bash
git add Cargo.toml src/query_main.rs
git commit -m "feat: add claude-history-query binary scaffold

New programmatic CLI for scripts and LLM agents.
Subcommands: list, show, usage (stubs).
Shares internal modules with main binary.

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>"
```

---

## Task 2: Implement List Command Core Logic

**Files:**

* Modify: `src/query_main.rs`

**Step 1: Add imports and output struct**

Add after the `mod` declarations:

```rust
use claude_history::path_filter::PathFilter;
use claude_history::query::{evaluate, parse_query, QueryExpr};
use claude_history::time_filter::TimeFilter;
use error::AppError;
use history::Conversation;
use serde::Serialize;

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
            uuid: conv.path.file_stem()
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
```

**Step 2: Implement run_list function**

Add before `main()`:

```rust
fn build_time_filter(args: &ListArgs) -> Result<Option<TimeFilter>> {
    if args.since.is_none() && args.after.is_none() && args.before.is_none() {
        return Ok(None);
    }

    let mut filter = TimeFilter::new();

    if let Some(ref since) = args.since {
        filter = filter
            .with_since(since)
            .map_err(|e| AppError::InvalidArgs(format!("Invalid --since: {}", e)))?;
    }
    if let Some(ref after) = args.after {
        filter = filter
            .with_after(after)
            .map_err(|e| AppError::InvalidArgs(format!("Invalid --after: {}", e)))?;
    }
    if let Some(ref before) = args.before {
        filter = filter
            .with_before(before)
            .map_err(|e| AppError::InvalidArgs(format!("Invalid --before: {}", e)))?;
    }

    Ok(Some(filter))
}

fn build_path_filter(args: &ListArgs) -> Result<Option<PathFilter>> {
    if args.include_path.is_empty() && args.exclude_path.is_empty() {
        return Ok(None);
    }

    PathFilter::from_patterns(&args.include_path, &args.exclude_path)
        .map(Some)
        .map_err(|e| AppError::InvalidArgs(format!("Invalid path pattern: {}", e)))
}

fn run_list(args: ListArgs, format: OutputFormat, quiet: bool) -> Result<()> {
    let time_filter = build_time_filter(&args)?;
    let path_filter = build_path_filter(&args)?;
    let query_expr = args.query.as_ref()
        .map(|q| parse_query(q))
        .transpose()
        .map_err(|e| AppError::InvalidArgs(format!("Invalid query: {}", e)))?;

    // Load conversations
    let mut conversations = if args.global {
        history::load_all_conversations(false, None, time_filter.as_ref(), path_filter.as_ref())?
    } else {
        let current_dir = std::env::current_dir()?;
        let projects_dir = history::get_claude_projects_dir(&current_dir)?;
        if !projects_dir.exists() {
            return Err(AppError::ProjectsDirNotFound(projects_dir.display().to_string()));
        }
        history::load_conversations(&projects_dir, false, None, time_filter.as_ref(), path_filter.as_ref())?
    };

    // Apply content query filter
    if let Some(ref query) = query_expr {
        conversations.retain(|conv| evaluate(query, &conv.full_text));
    }

    // Apply sort
    if args.sort == "oldest" {
        conversations.reverse();
    }

    // Apply limit
    if let Some(limit) = args.limit {
        conversations.truncate(limit);
    }

    if conversations.is_empty() {
        if !quiet {
            eprintln!("No conversations found");
        }
        std::process::exit(3);
    }

    // Output
    output_list(&conversations, &args.field, format);

    Ok(())
}

fn output_list(conversations: &[Conversation], fields: &[String], format: OutputFormat) {
    for conv in conversations {
        let out = ConversationOutput::from(conv);

        match format {
            OutputFormat::Jsonl => {
                if fields.is_empty() {
                    println!("{}", serde_json::to_string(&out).unwrap());
                } else {
                    let filtered = filter_fields(&out, fields);
                    println!("{}", serde_json::to_string(&filtered).unwrap());
                }
            }
            OutputFormat::Human => {
                if fields.is_empty() {
                    // Full human output
                    let age = chrono_humanize::HumanTime::from(conv.timestamp);
                    let project = out.project.as_deref().unwrap_or("unknown");
                    println!("{}\t{}\t{}\t{}", out.uuid, age, project, out.preview);
                } else if fields.len() == 1 {
                    println!("{}", get_field(&out, &fields[0]));
                } else {
                    let vals: Vec<_> = fields.iter().map(|f| get_field(&out, f)).collect();
                    println!("{}", vals.join("\t"));
                }
            }
        }
    }
}

fn filter_fields(out: &ConversationOutput, fields: &[String]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for field in fields {
        match field.as_str() {
            "uuid" => { map.insert("uuid".into(), serde_json::json!(out.uuid)); }
            "path" => { map.insert("path".into(), serde_json::json!(out.path)); }
            "cwd" => { map.insert("cwd".into(), serde_json::json!(out.cwd)); }
            "timestamp" => { map.insert("timestamp".into(), serde_json::json!(out.timestamp)); }
            "preview" => { map.insert("preview".into(), serde_json::json!(out.preview)); }
            "project" => { map.insert("project".into(), serde_json::json!(out.project)); }
            _ => {}
        }
    }
    serde_json::Value::Object(map)
}

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
```

**Step 3: Wire up in run()**

Update the `Commands::List` match arm:

```rust
Commands::List(args) => run_list(args, format, cli.quiet)?,
```

**Step 4: Add chrono-humanize import**

Add to top of file:

```rust
use chrono_humanize;
```

**Step 5: Verify list works**

Run: `cargo run --bin claude-history-query -- list -g --limit 3`
Expected: Shows 3 conversations

Run: `cargo run --bin claude-history-query -- list --jsonl --limit 2`
Expected: JSONL output

Run: `cargo run --bin claude-history-query -- list --field uuid --limit 2`
Expected: Just UUIDs

**Step 6: Commit**

```bash
git add src/query_main.rs
git commit -m "feat(query): implement list command with all filters

- Time filtering: --since, --after, --before
- Path filtering: --include-path, --exclude-path
- Content query: -q/--query
- Output control: --field, --limit, --sort
- Formats: --human (default), --jsonl

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>"
```

---

## Task 3: Implement Show Command

**Files:**

* Modify: `src/query_main.rs`

**Step 1: Add show command imports and implementation**

Add these functions:

```rust
fn find_conversation_by_id(uuid: &str) -> Result<std::path::PathBuf> {
    let projects_root = history::get_claude_projects_root()?;

    // Search all project directories for matching UUID
    for entry in std::fs::read_dir(&projects_root)? {
        let entry = entry?;
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }

        for file in std::fs::read_dir(&project_dir)? {
            let file = file?;
            let path = file.path();
            if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem == uuid || stem.starts_with(&format!("{}-", uuid)) {
                        return Ok(path);
                    }
                }
            }
        }
    }

    Err(AppError::NoHistoryFound(format!("UUID: {}", uuid)))
}

fn run_show(args: ShowArgs, format: OutputFormat) -> Result<()> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    // Resolve identifier to path
    let path = if args.identifier.contains('/') || args.identifier.contains('.') {
        std::path::PathBuf::from(&args.identifier)
    } else {
        find_conversation_by_id(&args.identifier)?
    };

    if !path.exists() {
        return Err(AppError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("File not found: {}", path.display()),
        )));
    }

    let file = File::open(&path)?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let entry: serde_json::Value = serde_json::from_str(&line)?;

        // Filter by message type
        let msg_type = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // Skip tool messages unless --tools
        if !args.tools && (msg_type == "tool_use" || msg_type == "tool_result") {
            continue;
        }

        // Handle based on format
        match format {
            OutputFormat::Jsonl => {
                println!("{}", line);
            }
            OutputFormat::Human => {
                // Simple human-readable output
                if let Some(role) = entry.get("role").and_then(|r| r.as_str()) {
                    match role {
                        "user" => {
                            if let Some(content) = entry.get("content") {
                                println!("## User\n");
                                print_content(content);
                                println!();
                            }
                        }
                        "assistant" => {
                            if let Some(content) = entry.get("content") {
                                println!("## Assistant\n");
                                print_content(content);
                                println!();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
}

fn print_content(content: &serde_json::Value) {
    match content {
        serde_json::Value::String(s) => println!("{}", s),
        serde_json::Value::Array(arr) => {
            for item in arr {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    println!("{}", text);
                }
            }
        }
        _ => {}
    }
}
```

**Step 2: Wire up in run()**

Update the `Commands::Show` match arm:

```rust
Commands::Show(args) => run_show(args, format)?,
```

**Step 3: Verify show works**

Run: `cargo run --bin claude-history-query -- list --field uuid --limit 1`
(Note the UUID)

Run: `cargo run --bin claude-history-query -- show <UUID>`
Expected: Shows conversation content

Run: `cargo run --bin claude-history-query -- show --jsonl <UUID>`
Expected: JSONL message stream

**Step 4: Commit**

```bash
git add src/query_main.rs
git commit -m "feat(query): implement show command

- Accepts UUID or path
- Filters: --tools, --thinking
- Formats: human (markdown-ish) or jsonl

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>"
```

---

## Task 4: Implement Usage Command

**Files:**

* Modify: `src/query_main.rs`

**Step 1: Add usage command implementation**

```rust
fn run_usage(format: OutputFormat) {
    match format {
        OutputFormat::Human | OutputFormat::Jsonl => {
            // Same markdown output for both (usage is documentation)
            print!(r#"# claude-history-query

Search and query Claude Code conversation history.

## Commands

### list
List conversations with optional filters.

Options:
  -g, --global         Search all projects
  -s, --since DURATION Filter by duration (2d, 1w, 3h)
  --after DATE|TIME    Filter after date/time
  --before DATE|TIME   Filter before date/time
  --include-path PAT   Include paths matching regex
  --exclude-path PAT   Exclude paths matching regex
  -q, --query EXPR     Boolean content query
  --field FIELD        Output specific field(s)
  --limit N            Limit results
  --sort ORDER         newest (default) or oldest

### show <UUID|PATH>
Output conversation content.

Options:
  --format FORMAT      markdown, plain, raw
  --tools              Include tool calls
  --thinking           Include thinking blocks
  --ts-after TIME      Messages after time
  --ts-before TIME     Messages before time

## Output Formats

--human    Human-readable (default)
--jsonl    One JSON object per line

## Field Names

uuid, path, cwd, timestamp, preview, project

## Common Patterns

Resume conversation:
  claude --resume "$(claude-history-query list --limit 1 --field uuid)"

Search and resume:
  claude --resume "$(claude-history-query list -q 'auth' --field uuid | fzf)"

Export to backup:
  claude-history-query list --since 1w --jsonl > backup.jsonl

Delete old conversations:
  claude-history-query list --before 30d --field path | xargs rm

## Exit Codes

0  Success
1  General error
2  Invalid arguments
3  No results found
"#);
        }
    }
}
```

**Step 2: Wire up in run()**

Update the `Commands::Usage` match arm:

```rust
Commands::Usage => run_usage(format),
```

**Step 3: Verify usage works**

Run: `cargo run --bin claude-history-query -- usage`
Expected: Shows documentation

**Step 4: Commit**

```bash
git add src/query_main.rs
git commit -m "feat(query): implement usage command

Self-documenting CLI for LLM agents.
Shows commands, options, common patterns.

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>"
```

---

## Task 5: Add Exit Code Handling

**Files:**

* Modify: `src/query_main.rs`

**Step 1: Define exit codes**

Add after imports:

```rust
mod exit_codes {
    pub const SUCCESS: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const INVALID_ARGS: i32 = 2;
    pub const NO_RESULTS: i32 = 3;
}
```

**Step 2: Update main() for proper exit codes**

```rust
fn main() {
    match run() {
        Ok(()) => std::process::exit(exit_codes::SUCCESS),
        Err(e) => {
            eprintln!("Error: {}", e);
            let code = match &e {
                AppError::InvalidArgs(_) => exit_codes::INVALID_ARGS,
                AppError::NoHistoryFound(_) => exit_codes::NO_RESULTS,
                _ => exit_codes::ERROR,
            };
            std::process::exit(code);
        }
    }
}
```

**Step 3: Verify exit codes**

Run: `cargo run --bin claude-history-query -- list -q 'xyznonexistent123'; echo $?`
Expected: Exit code 3

Run: `cargo run --bin claude-history-query -- list --since invalid; echo $?`
Expected: Exit code 2

**Step 4: Commit**

```bash
git add src/query_main.rs
git commit -m "feat(query): add proper exit codes

0=success, 1=error, 2=invalid args, 3=no results

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>"
```

---

## Task 6: Final Testing and Documentation

**Files:**

* Modify: `README.md` (optional section about query binary)

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass

**Step 2: Test key workflows**

```bash
# List with JSONL
cargo run --bin claude-history-query -- list -g --jsonl --limit 5

# Field selection
cargo run --bin claude-history-query -- list --field uuid --field cwd --limit 3

# Resume pattern
UUID=$(cargo run --bin claude-history-query -- list --limit 1 --field uuid)
echo "Would resume: $UUID"

# Show conversation
cargo run --bin claude-history-query -- show "$UUID"

# Usage
cargo run --bin claude-history-query -- usage
```

**Step 3: Commit final version**

```bash
git add -A
git commit -m "feat(query): complete claude-history-query implementation

Programmatic CLI for scripts and LLM agents:
- list: search with filters, field selection, JSONL output
- show: view conversation content
- usage: self-documenting for LLM discovery

Co-Authored-By: Claude Opus 4.5 <noreply@anthropic.com>"
```

---

## Summary

| Task | Description | Key Files |
|------|-------------|-----------|
| 1 | Binary scaffold | Cargo.toml, src/query_main.rs |
| 2 | List command | src/query_main.rs |
| 3 | Show command | src/query_main.rs |
| 4 | Usage command | src/query_main.rs |
| 5 | Exit codes | src/query_main.rs |
| 6 | Testing | - |

Total: 6 tasks, single file implementation leveraging existing modules.
