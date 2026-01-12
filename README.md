# claude-history

```
% claude-history
╭──────────────────────────────────────────────────────────────────────────────────╮
│ > ▌                                                                              │
│   19/19 ──────────────────────────────────────────────────────────────────────── │
│ ▌ [0] 11 hours | Adding 1 card(s) to Anki... Successfully added 1 new note ··    │
│   [1] 12 hours | ~/D/anki-llm-decks % anki-llm generate-init ... I can see the·· │
│   [2] a day | Why? Manually editing hundreds or thousands of Anki cards is ted·· │
│   [3] a day | Add disclaimer about pricing in ### Supported models that check ·· │
│   [4] a day | @README.md Manually editing hundreds or thousands of Anki cards ·· │
│   [5] a day | ~/c/anki-llm % identify logo.png logo.png PNG 756x238 756x238+0+·· │
│   [6] 2 days | @src/commands/generate-init.ts Which model parameter does gener·· │
│   [7] 2 days | @README.md Ask gemini what would be a good way to improve the r·· │
╰──────────────────────────────────────────────────────────────────────────────────╯
```

`claude-history` is a companion CLI for Claude Code. It lets you search recent
conversations recorded in Claude's local project history with an
`fzf`-powered fuzzy finder, then prints the selected transcript in a tidy,
readable format.

Run it from the project directory you work on with Claude Code and it will
discover the matching transcript folder automatically.

[Install](#install) ·
[Usage](#usage) ·
[Configuration](#configuration) ·
[Changelog](CHANGELOG.md)

## requirements

- [`fzf`](https://github.com/junegunn/fzf) available on your `PATH`
- Claude Code conversation logs under `~/.claude/projects`

## install

### Homebrew (macOS/Linux)

```sh
brew install raine/claude-history/claude-history
```

### Cargo

```sh
cargo install claude-history
```

## usage

Run the tool from inside the project directory you're interested in:

```sh
$ claude-history
```

This opens an `fzf` session listing all conversations, newest first. The search
matches against the entire transcript; the preview shows the first three
messages by default.

### search syntax

The fuzzy finder supports fzf's extended search syntax:

| Token    | Match type  | Example                                   |
|----------|-------------|-------------------------------------------|
| `'word`  | Exact match | `'error` matches "error" exactly          |
| `!word`  | Exclude     | `!test` excludes results containing "test"|
| `a \| b` | OR          | `'error \| 'warning` matches either term  |
| `a b`    | AND         | `'error 'api` matches both terms          |

Combine these for precise searches: `'deploy 'production !staging` finds
conversations mentioning both "deploy" and "production" but not "staging".

```
View Claude conversation history with fuzzy search

Usage: claude-history [OPTIONS]

Options:
  -t, --show-tools     Show tool calls in the conversation output
      --no-tools       Hide tool calls from the conversation output
  -d, --show-dir       Print the conversation directory path and exit
  -l, --last           Show the last messages in the fuzzy finder preview
      --first          Show the first messages in the fuzzy finder preview
  -r, --relative-time  Display relative time (e.g. "10 minutes ago")
      --absolute-time  Display absolute timestamp
      --show-thinking  Show thinking blocks in the conversation output
      --hide-thinking  Hide thinking blocks from the conversation output
  -c, --resume         Resume the selected conversation in Claude Code
  -a, --all-projects   Browse conversations from all projects (select project first)
  -g, --global         Search all conversations from all projects at once
  -h, --help           Print help
```

### preview modes

- `claude-history` shows the first three messages in the preview
- `claude-history --last` flips the preview to the last three messages.

### showing tool calls

By default, tool invocations (`<Calling Tool: …>`) are hidden to keep the
conversation focused on the human dialogue. Use `--show-tools` (or `-t`) to
display them when you want to see what tools Claude used.

### showing thinking blocks

Extended thinking models (like Claude Sonnet 4.5) include reasoning steps in
their output. By default, these thinking blocks are hidden to keep
conversations focused. Use `--show-thinking` to display them when you want to
see Claude's reasoning process.

### resuming conversations

If you want to continue a conversation, launch `claude-history` with `--resume`
and it will hand off to `claude --resume <conversation-id>`.

### browsing all projects

By default, `claude-history` only shows conversations from the current directory's
project. Use `--all-projects` (or `-a`) to browse conversations from any project:

```sh
$ claude-history --all-projects
```

This first shows an fzf selector with all projects that have conversation history,
sorted by most recently modified. After selecting a project, you'll see the usual
conversation selector.

Note: Project paths are decoded from Claude's internal format using a heuristic.
Claude encodes paths by replacing `/`, `_`, and `.` with `-`, which is lossy.
The displayed paths may not be exact (e.g., single underscores may appear as `/`),
but should be recognizable enough to identify your projects.

### global search

Use `--global` (or `-g`) to search all conversations from all projects at once:

```sh
$ claude-history --global
```

This displays all conversations from every project in a single fzf view, sorted by
modification time (newest first). Each conversation is prefixed with its project path
so you can identify which project it belongs to.

For [workmux](https://github.com/raine/workmux) users, worktree paths are displayed in
a compact format: `[project/worktree]` instead of just the worktree folder name.

The `--resume` flag works with global search. It will automatically run Claude in the
correct project directory for the selected conversation.

### integration with other scripts

You can integrate `claude-history` into other tools to pass conversation context
to new Claude Code sessions. This is useful when you want Claude to understand
what you were working on previously.

For example, a commit message generator script could use the conversation history
to write more contextual commit messages:

```bash
# Get conversation history if --context flag is set
conversation_context=""
if [ "$include_history" = true ]; then
    echo "Loading conversation history..."
    conversation_history=$(claude-history 2>/dev/null)
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

## configuration

You can set default preferences for display options in `~/.config/claude-history/config.toml`. Command-line flags will override these settings.

Create the config file:

```sh
mkdir -p ~/.config/claude-history
cat > ~/.config/claude-history/config.toml << 'EOF'
[display]
# Show tool calls in output (default: false)
no_tools = false

# Show last messages in preview (default: false)
last = false

# Use relative time formatting (default: false)
relative_time = true

# Show thinking blocks (default: false)
show_thinking = false
EOF
```

### available options

- `no_tools` (boolean): When false, shows tool calls; when true, hides them (default: false means tools are hidden)
- `last` (boolean): Show last messages instead of first in fuzzy finder preview (default: false)
- `relative_time` (boolean): Display relative time instead of absolute timestamp (default: false)
- `show_thinking` (boolean): Show thinking blocks in conversation output (default: false)

### overriding config

Each display option has opposing flags for explicit override:

- `--no-tools` / `--show-tools`
- `--last` / `--first`
- `--relative-time` / `--absolute-time`
- `--hide-thinking` / `--show-thinking`

For example, if your config has `no_tools = false` (showing tools), you can temporarily hide them with `--no-tools`.

## filtering details

The tool filters out some noisy artifacts before showing conversations, so you
only see transcripts that are likely to matter for your recent work.

- Skips the "Warmup / I'm Claude Code…" exchanges that are sometimes injected
  without user interaction
- Skips conversations that only contain the `/clear` terminal command

## development

The repository includes `just` recipes:

```sh
$ just check
```

This runs `cargo fmt`, `cargo clippy --fix`, and `cargo build` in parallel.

## Related projects

- [workmux](https://github.com/raine/workmux) — Git worktrees + tmux windows for parallel AI agent workflows
- [consult-llm-mcp](https://github.com/raine/consult-llm-mcp) — MCP server that lets Claude Code consult stronger AI models (o3, Gemini, GPT-5.1 Codex)
- [tmux-file-picker](https://github.com/raine/tmux-file-picker) — Pop up fzf in tmux to quickly insert file paths, perfect for AI coding assistants
