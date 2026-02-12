# claude-history

<img src="/meta/screenshot.webp" />

`claude-history` is a companion CLI for Claude Code. It lets you search recent
conversations recorded in Claude's local project history with a built-in
terminal UI, then view the selected transcript directly in the terminal with
scrolling, search, and export capabilities.

Run it from the project directory you work on with Claude Code and it will
discover the matching transcript folder automatically.

[Install](#install) · [Usage](#usage) · [Configuration](#configuration) ·
[Changelog](CHANGELOG.md)

## Install

### Quick install

```sh
curl -fsSL https://raw.githubusercontent.com/raine/claude-history/main/scripts/install.sh | bash
```

### Homebrew (macOS/Linux)

```sh
brew install raine/claude-history/claude-history
```

### Cargo

```sh
cargo install claude-history
```

### From source

```sh
git clone https://github.com/raine/claude-history.git
cd claude-history
cargo install --locked --path .
```

This installs both binaries: `claude-history` (TUI) and `claude-history-query` (CLI).

## Usage

Run the tool from inside the project directory you're interested in:

```sh
$ claude-history
```

This opens a terminal UI listing all conversations, sorted by recency. Type to
search across all transcripts. Each item shows a preview of the conversation and
match context is highlighted when your search matches content not visible in the
preview.

### Keyboard navigation (List mode)

| Key                     | Action                          |
| ----------------------- | ------------------------------- |
| `↑` / `↓`               | Move selection                  |
| `←` / `→`               | Move cursor in search           |
| `Ctrl+P` / `Ctrl+N`     | Move selection (vi-style)       |
| `Ctrl+D` / `Ctrl+U`     | Half page down/up (vim-style)   |
| `Page Up` / `Page Down` | Jump by page                    |
| `Home` / `End`          | Jump to first/last              |
| `Enter`                 | Open conversation viewer        |
| `Ctrl+O`                | Select and exit (for scripting) |
| `Ctrl+W`                | Delete word before cursor       |
| `Ctrl+R`                | Resume conversation             |
| `Ctrl+X`                | Delete conversation             |
| `?`                     | Show keyboard shortcuts         |
| `Esc` / `Ctrl+C`        | Quit                            |

### Keyboard navigation (Viewer mode)

| Key            | Action                                             |
| -------------- | -------------------------------------------------- |
| `j` / `↓`      | Scroll down                                        |
| `k` / `↑`      | Scroll up                                          |
| `d` / `Ctrl+D` | Half page down                                     |
| `u` / `Ctrl+U` | Half page up                                       |
| `Page Down`    | Full page down                                     |
| `Page Up`      | Full page up                                       |
| `g` / `Home`   | Jump to top                                        |
| `G` / `End`    | Jump to bottom                                     |
| `/`            | Start search                                       |
| `n`            | Next search match                                  |
| `N`            | Previous search match                              |
| `t`            | Cycle tools: off/truncated/full                    |
| `T`            | Toggle thinking                                    |
| `p`            | Show file path                                     |
| `Y`            | Copy file path to clipboard                        |
| `I`            | Copy session ID to clipboard                       |
| `Ctrl+R`       | Resume conversation                                |
| `Ctrl+X`       | Delete conversation                                |
| `?`            | Show keyboard shortcuts                            |
| `q` / `Esc`    | Return to list (or quit in direct file input mode) |
| `Ctrl+C`       | Quit                                               |

### Search

Search uses fuzzy word matching with the following features:

- **Case-insensitive**: "config" matches "CONFIG"
- **Underscore as separator**: "api key" matches "API_KEY"
- **Prefix matching**: "auth" matches "authentication", "authorize"
- **Multi-word AND logic**: all query words must match

Results are ranked by recency, so recent conversations appear first.

### Direct file input

You can open a JSONL conversation file directly, bypassing the conversation
selection UI:

```sh
$ claude-history /path/to/conversation.jsonl
```

All display options work in this mode:

```sh
$ claude-history --show-tools --show-thinking /path/to/conversation.jsonl
```

Press `q` or `Esc` to quit when viewing a file directly.

### Conversation viewer

Press `Enter` on a conversation to open the built-in viewer. The viewer displays
conversations in a ledger-style format with scrolling support.

**Features:**

- **Scrolling**: Navigate with vim-style keys (`j`/`k`) or arrow keys
- **Search**: Press `/` to search within the conversation, then `n`/`N` to
  navigate matches
- **Cycle tools**: Press `t` to cycle tool display (off → truncated → full)
- **Toggle thinking**: Press `T` to show/hide thinking blocks
- **Show path**: Press `p` to display the conversation file path

Press `q` or `Esc` to return to the conversation list.

```
View Claude conversation history

Usage: claude-history [OPTIONS] [FILE]

Arguments:
  [FILE]  JSONL conversation file to view directly

Options:
  -t, --show-tools       Show tool calls in the conversation output
      --no-tools         Hide tool calls from the conversation output
  -d, --show-dir         Print the conversation directory path and exit
  -l, --last             Show the last messages in the TUI preview
      --first            Show the first messages in the TUI preview
  -r, --relative-time    Display relative time (e.g. "10 minutes ago")
      --absolute-time    Display absolute timestamp
      --show-thinking    Show thinking blocks in the conversation output
      --hide-thinking    Hide thinking blocks from the conversation output
  -c, --resume           Resume the selected conversation in Claude Code
  -p, --show-path        Print the selected conversation file path
  -i, --show-id          Print the selected conversation session ID
      --plain            Output plain text without ledger formatting
      --debug [<LEVEL>]  Print debug information (filter by level: debug, info, warn, error)
  -g, --global           Search all conversations from all projects at once
      --render <FILE>    Render a JSONL file in ledger format and exit
      --no-color         Disable colored output
      --pager            Display output through a pager (less)
      --no-pager         Disable pager output
  -h, --help             Print help
```

### Preview modes

- `claude-history` shows the first messages in the preview
- `claude-history --last` flips the preview to the last messages

### Showing tool calls

In the TUI viewer, tool calls default to **truncated** mode — showing the tool
header plus the first few body lines with a "(N more lines...)" indicator. Press
`t` to cycle through modes: off → truncated → full. Use `--show-tools` (or `-t`)
to start in full mode, or `--no-tools` to start with tools hidden.

### Showing thinking blocks

Extended thinking models (like Claude Sonnet 4.5) include reasoning steps in
their output. By default, these thinking blocks are hidden to keep conversations
focused. Use `--show-thinking` to display them when you want to see Claude's
reasoning process.

### Resuming conversations

If you want to continue a conversation, launch `claude-history` with `--resume`
and it will hand off to `claude --resume <conversation-id>`.

You can configure default arguments to pass to the `claude` command every time
you resume a conversation. This is useful if you typically run Claude with
specific flags (like `--dangerously-skip-permissions`) and want them applied
automatically when resuming:

```toml
# ~/.config/claude-history/config.toml
[resume]
default_args = ["--dangerously-skip-permissions"]
```

With this configuration, when you resume a conversation, it will run:

```sh
claude --resume <conversation-id> --dangerously-skip-permissions
```

This provides a cleaner alternative to shell aliases, as the arguments are
applied specifically when resuming through `claude-history`, without affecting
how you normally invoke Claude.

If you use a shell alias for `claude` with extra flags, you can use `--show-id`
to select a session and resume it manually:

```sh
claude --resume $(claude-history --show-id)
```

In the viewer, press `I` to copy the session ID to clipboard.

### Markdown rendering

Claude's responses are rendered with markdown formatting for better terminal
readability. Use `--plain` to disable rendering and get raw text output.

### Plain output mode

Use `--plain` to output conversations without ledger formatting:

```sh
$ claude-history --plain
```

This produces simple `Role: content` output without colors, text wrapping, or
markdown rendering, suitable for piping to other tools or LLMs:

```
You: How do I fix this bug?

Claude: Looking at the code, the issue is...
```

### Pager output

By default, conversation output is piped through a pager (`less -R`) when stdout
is a terminal. This enables scrolling through long conversations. Use
`--no-pager` to disable this behavior and print directly to stdout.

The pager respects the `$PAGER` environment variable. If not set, it defaults to
`less -R` (which preserves ANSI colors).

### Global search

Use `--global` (or `-g`) to search all conversations from all projects at once:

```sh
$ claude-history --global
```

This displays all conversations from every project in a single view, sorted by
modification time (newest first). Each conversation shows its project path so
you can identify which project it belongs to. Conversations load in the
background so you can start typing immediately.

For [workmux](https://github.com/raine/workmux) users, worktree paths are
displayed in a compact format: `[project/worktree]` instead of just the worktree
folder name.

The `--resume` flag works with global search. It will automatically run Claude
in the correct project directory for the selected conversation.

### Integration with other scripts

You can integrate `claude-history` into other tools to pass conversation context
to new Claude Code sessions. This is useful when you want Claude to understand
what you were working on previously.

For example, a commit message generator script could use the conversation
history to write more contextual commit messages:

```bash
# Get conversation history if --context flag is set
conversation_context=""
if [ "$include_history" = true ]; then
    echo "Loading conversation history..."
    conversation_history=$(claude-history --plain 2>/dev/null)
    if [ -n "$conversation_history" ]; then
        conversation_context="

=== START CONVERSATION CONTEXT ===
$conversation_history
=== END CONVERSATION CONTEXT ===

"
    fi
fi

# Pass to Claude CLI with the conversation context
prompt="Write a commit message for these changes.
${conversation_context}
Staged changes:
$staged_diff"

claude -p "$prompt"
```

## Configuration

You can set default preferences for display options in
`~/.config/claude-history/config.toml`. Command-line flags will override these
settings.

Create the config file:

```sh
mkdir -p ~/.config/claude-history
cat > ~/.config/claude-history/config.toml << 'EOF'
[display]
# Tool display: true = hidden, false = full (default: unset = truncated)
# no_tools = false

# Show last messages in TUI preview (default: false)
last = false

# Use relative time formatting (default: false)
relative_time = true

# Show thinking blocks (default: false)
show_thinking = false

# Use plain output without ledger formatting (default: false)
plain = false

# Use pager for output (default: true when stdout is a terminal)
pager = true

[resume]
# Default arguments to pass to claude command when resuming
# Example: default_args = ["--dangerously-skip-permissions"]
EOF
```

### Available options

#### Display options

- `no_tools` (boolean): When `true`, hides tool calls; when `false`, shows them
  in full. When unset (default), tools display in truncated mode
- `last` (boolean): Show last messages instead of first in TUI preview (default:
  false)
- `relative_time` (boolean): Display relative time instead of absolute timestamp
  (default: false)
- `show_thinking` (boolean): Show thinking blocks in conversation output
  (default: false)
- `plain` (boolean): Output plain text without ledger formatting (default:
  false)
- `pager` (boolean): Pipe output through a pager for scrolling (default: true
  when stdout is a terminal)

#### Resume options

- `default_args` (array of strings): Arguments to pass to the `claude` command
  when resuming conversations. Useful for flags like
  `--dangerously-skip-permissions` that you want applied every time you resume.
  Example: `default_args = ["--dangerously-skip-permissions", "--verbose"]`

### Overriding config

Each display option has opposing flags for explicit override:

- `--no-tools` / `--show-tools`
- `--last` / `--first`
- `--relative-time` / `--absolute-time`
- `--hide-thinking` / `--show-thinking`
- `--plain` (no opposite flag)
- `--no-pager` / `--pager`

For example, if your config has `no_tools = false` (showing tools), you can
temporarily hide them with `--no-tools`.

## Filtering details

The tool filters out some noisy artifacts before showing conversations, so you
only see transcripts that are likely to matter for your recent work.

- Skips the "Warmup / I'm Claude Code…" exchanges that are sometimes injected
  without user interaction
- Skips conversations that only contain the `/clear` terminal command

## Development

The repository includes `just` recipes:

```sh
$ just check
```

This runs `cargo fmt`, `cargo clippy --fix`, and `cargo build` in parallel.

## claude-history-query

A non-interactive CLI companion optimized for shell scripts and LLM agents.
Outputs JSONL or human-readable formats suitable for pipelines.

### Commands

```sh
claude-history-query list [OPTIONS]    # List conversations
claude-history-query show <ID>         # Show conversation content
claude-history-query usage             # Self-documenting help for LLMs
```

### Examples

```sh
# Resume most recent conversation
claude --resume "$(claude-history-query list -n 1 -f uuid)"

# Search and resume with fzf
claude --resume "$(claude-history-query list -g -f uuid | fzf)"

# Export recent conversations to JSONL
claude-history-query list --since 1w --jsonl > backup.jsonl

# Find conversations about a topic
claude-history-query list -g --query "docker && kubernetes"

# Delete old conversations
claude-history-query list --before 2024-01-01 -f path | xargs rm

# Count conversations per project
claude-history-query list -g -f cwd | sort | uniq -c | sort -rn
```

### Output formats

* `--human` (default) — Human-readable with relative timestamps
* `--jsonl` — JSON Lines, one object per line for parsing with `jq`

### Field selection

Use `-f/--field` to output specific fields (tab-separated in human mode, filtered JSON in JSONL mode):

```sh
claude-history-query list -f uuid -f cwd
# abc123    /home/user/project
# def456    /home/user/other
```

Available fields: `uuid`, `path`, `cwd`, `timestamp`, `preview`, `project`

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error (I/O, parsing) |
| 2 | Invalid arguments |
| 3 | No results found |

### Full options

```
claude-history-query list [OPTIONS]

Options:
  -g, --global                Search all projects globally
  -s, --since <DURATION>      Since duration ago (2d, 1w, 3h)
      --after <DATE>          After date (YYYY-MM-DD)
      --before <DATE>         Before date (YYYY-MM-DD)
      --include-path <REGEX>  Include paths matching pattern
      --exclude-path <REGEX>  Exclude paths matching pattern
      --query <EXPR>          Boolean search (e.g., "rust && !deprecated")
  -f, --field <FIELD>         Output specific field(s)
  -n, --limit <N>             Limit results
      --sort <ORDER>          newest, oldest, most-messages, least-messages
      --human                 Human-readable output (default)
      --jsonl                 JSON Lines output
  -q, --quiet                 Suppress non-error output

claude-history-query show <ID> [OPTIONS]

Options:
  -t, --tools                 Include tool calls
      --thinking              Include thinking blocks
      --ts-after <TS>         Messages after timestamp
      --ts-before <TS>        Messages before timestamp
      --human                 Human-readable output (default)
      --jsonl                 JSON Lines output
```

## Related projects

- [workmux](https://github.com/raine/workmux) — Git worktrees + tmux windows for
  parallel AI agent workflows
- [git-surgeon](https://github.com/raine/git-surgeon) — Non-interactive
  hunk-level git staging for AI agents
- [consult-llm-mcp](https://github.com/raine/consult-llm-mcp) — MCP server that
  lets Claude Code consult stronger AI models (o3, Gemini, GPT-5.1 Codex)
- [tmux-file-picker](https://github.com/raine/tmux-file-picker) — Pop up fzf in
  tmux to quickly insert file paths, perfect for AI coding assistants
