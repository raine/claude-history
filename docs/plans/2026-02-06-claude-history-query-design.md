# claude-history-query CLI Design

A programmatic CLI interface for searching Claude Code conversation history, optimized for both human users and LLM agents.

## Motivation

The existing `claude-history` binary provides a TUI for interactive browsing. This companion tool provides a UNIX-philosophy CLI that:

* Outputs structured data (JSONL) for machine consumption
* Composes with standard tools (`jq`, `fzf`, `xargs`)
* Self-documents via `usage` command for LLM agent discovery
* Uses explicit format flags rather than TTY auto-detection

## Command Structure

```
claude-history-query <command> [options]

Commands:
  list     List conversations with filters
  show     Output conversation content
  usage    Print documentation for LLM agents
```

### Global Options

```
--human      Human-readable output (default)
--jsonl      JSONL output (one JSON object per line)
```

## Commands

### list

List conversations with optional filters.

```
claude-history-query list [options]
```

**Options:**

```
-g, --global              Search all projects (not just current directory)
-s, --since <DURATION>    Conversations within duration (e.g., 2d, 1w, 3h)
--after <DATE|TIMESTAMP>  Conversations after date/time (YYYY-MM-DD or HH:mm)
--before <DATE|TIMESTAMP> Conversations before date/time
--include-path <PATTERN>  Include projects matching regex pattern
--exclude-path <PATTERN>  Exclude projects matching regex pattern
-q, --query <EXPR>        Boolean content query (e.g., "foo && !bar")
--field <FIELD>           Output only specified field(s)
--limit <N>               Limit results to N conversations
--sort <ORDER>            Sort order: newest (default), oldest
```

**Available fields:** `uuid`, `path`, `cwd`, `timestamp`, `preview`, `project`

**Output Examples:**

```bash
# Human output (default)
claude-history-query list --human
# [1] 2d ago  ~/projects/myapp  "Fix authentication bug..."
# [2] 5d ago  ~/work/api        "Add rate limiting..."

# JSONL output
claude-history-query list --jsonl
# {"uuid":"abc123","path":"/home/...","cwd":"...","timestamp":"...","preview":"..."}

# Single field output
claude-history-query list --field uuid
# abc123
# def456

# Multiple fields (tab-separated)
claude-history-query list --field uuid --field cwd
# abc123	/home/user/projects/myapp

# Field selection with JSONL
claude-history-query list --jsonl --field uuid --field timestamp
# {"uuid":"abc123","timestamp":"2026-02-04T14:30:00Z"}
```

### show

Output full conversation content.

```
claude-history-query show <UUID|PATH> [options]
```

**Options:**

```
--format <FORMAT>         Output format: markdown (default), plain, raw
--tools                   Include tool calls in output
--thinking                Include thinking blocks in output
--ts-after <TIMESTAMP>    Only messages after this time
--ts-before <TIMESTAMP>   Only messages before this time
--human                   Human-readable with syntax highlighting (default)
--jsonl                   Each message as JSONL record
```

**Output Examples:**

```bash
# Human output (default)
claude-history-query show abc123 --human
# # Conversation: abc123
# # Project: ~/projects/myapp
# # Started: 2026-02-04 14:30
#
# ## User
# Fix the authentication bug in login.rs
#
# ## Assistant
# I'll look at the login.rs file...

# JSONL output
claude-history-query show abc123 --jsonl
# {"role":"user","content":"Fix the authentication...","timestamp":"..."}
# {"role":"assistant","content":"I'll look at...","timestamp":"..."}

# Filter by message timestamp
claude-history-query show abc123 --ts-after 09:00 --ts-before 12:00
```

**Identifier Resolution:**

* If argument looks like UUID (alphanumeric, no path separators): search for matching session
* If argument is a path: use directly
* Exit with error if UUID matches multiple sessions

### usage

Output documentation optimized for LLM agents.

```
claude-history-query usage [--format human|markdown|jsonl]
```

**Purpose:** Self-describing interface for LLM agents to discover tool capabilities.

**Output Example (markdown):**

```markdown
# claude-history-query

Search and query Claude Code conversation history.

## Commands

### list
List conversations with optional filters.
Options: -g, --since, --after, --before, --include-path, --exclude-path, -q, --field, --limit, --sort

### show <UUID|PATH>
Output conversation content.
Options: --format, --tools, --thinking, --ts-after, --ts-before

## Common Patterns

- Resume: `claude --resume "$(claude-history-query list --limit 1 --field uuid)"`
- Search: `claude-history-query list -q 'keyword' --jsonl`
```

## Compound Command Patterns

Operations like `delete` and `resume` are not implemented as subcommands. Instead, compose with shell tools:

### Resume a Conversation

```bash
# Interactive selection, then resume
claude --resume "$(claude-history-query list -g --field uuid | fzf)"

# Resume most recent from current project
claude --resume "$(claude-history-query list --limit 1 --field uuid)"

# Resume most recent matching query
claude --resume "$(claude-history-query list -q 'authentication' --limit 1 --field uuid)"
```

### Delete Conversations

```bash
# Delete single conversation by UUID
rm "$(claude-history-query list --field path | grep abc123)"

# Delete all conversations older than 30 days
claude-history-query list --before 30d --field path | xargs rm

# Interactive multi-select delete
claude-history-query list -g --field path | fzf --multi | xargs rm
```

### Filtering Pipelines

```bash
# Find conversations about "docker" in work projects
claude-history-query list -g --include-path '/work/' -q 'docker' --jsonl | jq '.uuid'

# Export last week's conversations to archive
claude-history-query list --since 1w --jsonl > weekly-backup.jsonl

# Count conversations per project
claude-history-query list -g --field cwd | sort | uniq -c | sort -rn

# Convert JSONL to JSON array if needed
claude-history-query list --jsonl | jq -s '.'
```

## Exit Codes

```
0   Success
1   General error (IO, parse failure)
2   Invalid arguments / usage error
3   No results found (empty query result)
4   Ambiguous identifier (UUID matches multiple)
```

### Error Handling

* Errors go to stderr, never stdout (keeps pipelines clean)
* `--quiet` suppresses non-essential output (no short form to avoid collision with `-q`/`--query`)

```bash
# Won't pollute the pipeline even on error
claude-history-query list -q 'nonexistent' --field uuid | wc -l
# stderr: "No conversations match query"
# stdout: (empty)
# exit: 3
```

## Architecture

### Binary Structure

```
claude-history/
├── src/
│   ├── lib.rs              # Shared library (existing filters, parsers)
│   ├── main.rs             # TUI binary (existing claude-history)
│   └── bin/
│       └── claude-history-query.rs   # New CLI binary
├── Cargo.toml              # Add [[bin]] entry
```

### Dependencies

Reuses existing library crate:

* `claude_history::time_filter::TimeFilter`
* `claude_history::path_filter::PathFilter`
* `claude_history::query::{parse_query, evaluate}`
* `history::Conversation`, loaders

No TUI dependencies (ratatui, crossterm) - keeps binary small.

### Implementation Order

1. Add `src/bin/claude-history-query.rs` scaffold with clap
2. Implement `list` command using existing loaders
3. Implement `show` command using existing display logic
4. Implement `usage` command (static documentation)
5. Add `--field` output formatting
6. Add `--ts-after/--ts-before` to show
7. Documentation with compound examples

## Design Principles

* **Explicit over implicit:** Always use `--human` or `--jsonl` flags rather than TTY auto-detection
* **UNIX composability:** Single-purpose commands that combine with `jq`, `fzf`, `xargs`
* **Self-documenting:** `usage` command provides LLM-friendly documentation
* **Minimal subcommands:** Compound operations via shell composition, not built-in commands
* **Descriptive naming:** `claude-history-query` over abbreviated `chq`
