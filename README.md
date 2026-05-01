# claude-history

<img src="/meta/screenshot.webp" />

> _"This is the best thing ever thanks for this project."_ —
> [@andrewle8](https://github.com/andrewle8)

`claude-history` is a companion CLI for Claude Code. It lets you search recent
conversations recorded in Claude's local project history with a built-in
terminal UI, then view the selected transcript directly in the terminal with
scrolling, search, and export capabilities.

Run it from the project directory you work on with Claude Code and it will
discover the matching transcript folder automatically.

[Install](#install) · [Features](#features) · [Usage](#usage) ·
[Configuration](#configuration) · [Changelog](CHANGELOG.md)

## Features

- **Fuzzy search** across all conversations with field-aware relevance scoring,
  prefix matching, word boundary awareness, and tool output indexing
- **Conversation viewer** with vim-style scrolling, in-viewer search, message
  navigation, and markdown rendering
- **Resume and fork** conversations directly from the TUI with configurable
  keybindings
- **Cross-project fork** — fork a conversation from any project into your
  current working directory, useful when working across git worktrees
- **Worktree-aware** project filtering for
  [workmux](https://github.com/raine/workmux) users
- **Export and copy** conversations or individual messages to clipboard
- **Configurable** display options, keybindings, and default resume arguments

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

## Updating

```sh
claude-history update
```

Homebrew users should use `brew upgrade claude-history` instead.

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
| Mouse wheel             | Scroll the result list          |
| Mouse click             | Open conversation under cursor  |
| `Ctrl+O`                | Select and exit (for scripting) |
| `Ctrl+W`                | Delete word before cursor       |
| `Ctrl+R`                | Resume conversation             |
| `Ctrl+F`                | Fork and resume conversation    |
| `Ctrl+X`                | Delete conversation             |
| `Tab`                   | Toggle all / workspace scope    |
| `?`                     | Show keyboard shortcuts         |
| `Esc`                   | Clear search input, or quit     |
| `Ctrl+C`                | Quit                            |

### Keyboard navigation (Viewer mode)

| Key            | Action                                             |
| -------------- | -------------------------------------------------- |
| `j` / `↓`      | Scroll down                                        |
| `k` / `↑`      | Scroll up                                          |
| Mouse wheel    | Scroll the conversation                            |
| `J` / `]`      | Jump to next message                               |
| `K` / `[`      | Jump to previous message                           |
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
| `e`            | Export conversation to file                        |
| `y`            | Copy to clipboard (message if selected, else menu) |
| `p`            | Show file path                                     |
| `Y`            | Copy file path to clipboard                        |
| `I`            | Copy session ID to clipboard                       |
| `Ctrl+R`       | Resume conversation                                |
| `Ctrl+F`       | Fork and resume conversation                       |
| `Ctrl+X`       | Delete conversation                                |
| `?`            | Show keyboard shortcuts                            |
| `q` / `Esc`    | Return to list (or quit in direct file input mode) |
| `Ctrl+C`       | Quit                                               |

### Message navigation

Press `J`/`K` or `[`/`]` to enter message navigation mode. A teal `▌` marker
appears in the gutter showing which message is focused. While in this mode:

- `J` / `]` — jump to next message
- `K` / `[` — jump to previous message
- `y` — copy the focused message to clipboard (raw markdown)
- `Esc` — exit message navigation mode

Searching with `/` also activates message navigation, focusing the message
containing each match as you move through results with `n`/`N`.

### Search

Search uses fuzzy word matching with the following features:

- **Case-insensitive**: "config" matches "CONFIG"
- **Underscore as separator**: "api key" matches "API_KEY"
- **Prefix matching**: "auth" matches "authentication", "authorize"
- **Word boundary**: matches start at word boundaries, so "red" won't match
  inside "fired"
- **Multi-word AND logic**: all query words must match, each word is highlighted
  individually
- **Tool output indexing**: search matches content inside tool results (bash
  output, file contents, grep results, etc.), not just user/assistant text
- **UUID lookup**: paste a full session UUID (e.g.
  `e7d318b1-4274-4ee2-a341-e94893b5df49`) to jump directly to that session, even
  if it belongs to a different project

Results are ranked by relevance using field-aware scoring: matches in the
title, project name, and summary are weighted higher than body text. Within
equally relevant results, recent conversations rank first.

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

- **Scrolling**: Navigate with vim-style keys (`j`/`k`), arrow keys, or the
  mouse wheel
- **Search**: Press `/` to search within the conversation, then `n`/`N` to
  navigate matches
- **Cycle tools**: Press `t` to cycle tool display (off → truncated → full)
- **Toggle thinking**: Press `T` to show/hide thinking blocks
- **Show path**: Press `p` to display the conversation file path
- **Light/dark theme**: Automatically detects terminal background color and
  applies an appropriate color theme

Press `q` or `Esc` to return to the conversation list.

```
View Claude conversation history

Usage: claude-history [OPTIONS] [FILE]
       claude-history [COMMAND]

Commands:
  update  Update claude-history to the latest version

Arguments:
  [FILE]  JSONL conversation file to view directly

Options:
  -t, --show-tools       Show tool calls in the conversation output
      --no-tools         Hide tool calls from the conversation output
  -d, --show-dir         Print the conversation directory path and exit
  -l, --last             Show the last messages in the TUI preview (default)
      --first            Show the first messages in the TUI preview
      --show-thinking    Show thinking blocks and subagent internals in the conversation output
      --hide-thinking    Hide thinking blocks and subagent internals from the conversation output
  -c, --resume           Resume the selected conversation in Claude Code
      --fork-session     Fork the session when resuming
  -p, --show-path        Print the selected conversation file path
  -i, --show-id          Print the selected conversation session ID
      --plain            Output plain text without ledger formatting
      --delete <SESSION_ID>  Delete a session by its UUID and exit
      --debug-search <QUERY>  Debug search result scoring for a query
      --debug [<LEVEL>]  Print debug information (optionally filter by level: debug, info, warn, error)
  -L, --local            Show only conversations from the current workspace directory
      --pager            Display output through a pager (less)
      --no-pager         Disable pager output
      --render <FILE>    Render a JSONL file in ledger format and exit
      --no-color         Disable colored output
  -h, --help             Print help
  -V, --version          Print version
```

### Preview modes

- `claude-history` shows the last messages in the preview (default)
- `claude-history --first` flips the preview to the first messages

### Showing tool calls

In the TUI viewer, tool calls default to **truncated** mode — showing the tool
header plus the first few body lines with a "(N more lines...)" indicator. Press
`t` to cycle through modes: off → truncated → full. Use `--show-tools` (or `-t`)
to start in full mode, or `--no-tools` to start with tools hidden.

### Showing thinking blocks and subagent messages

Extended thinking models (like Claude Sonnet 4.5) include reasoning steps in
their output. When Claude uses the Task tool to spawn subagents, the internal
tool calls and messages within those subagents are also hidden by default. Use
`--show-thinking` (or press `T` in the TUI) to display both thinking blocks and
subagent internals. Subagent messages appear dimmed with a `↳` prefix to
distinguish them from top-level conversation entries.

### Resuming conversations

If you want to continue a conversation, launch `claude-history` with `--resume`
and it will hand off to `claude --resume <conversation-id>`.

To fork a conversation (creating a new session branching from the original), use
`--resume --fork-session` or press `Ctrl+F` in the TUI.

Within the same project, this passes `--fork-session` to `claude`, which creates
a new session ID branching from the original. When forking a conversation from a
different project, the session files are copied to your CWD's project directory
and resumed there — the copy continues independently without affecting the
original.

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

### Scope: all conversations vs current workspace

By default, `claude-history` shows all conversations from every project, sorted
by modification time (newest first). Each conversation shows its project path so
you can identify which project it belongs to.

Press `Tab` to toggle between all conversations and the current workspace only.
Use `-L`/`--local` to start with the workspace filter active.

For [workmux](https://github.com/raine/workmux) users, worktree paths are
displayed in a compact format: `[project/worktree]` instead of just the worktree
folder name. The project filter (toggled with `Tab`) is worktree-aware: it
includes conversations from the main repo and all its worktrees, regardless of
which one you're currently in.

The `--resume` flag works across projects. It will automatically run Claude in
the correct project directory for the selected conversation.

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

# Show last messages in TUI preview (default: true)
# last = true

# Show thinking blocks (default: false)
show_thinking = false

# Use plain output without ledger formatting (default: false)
plain = false

# Use pager for output (default: true when stdout is a terminal)
pager = true

[resume]
# Default arguments to pass to claude command when resuming
# Example: default_args = ["--dangerously-skip-permissions"]

[keys]
# Customize keybindings (default: ctrl+r, ctrl+f, ctrl+x)
# Supports ctrl+<key> and alt+<key> combinations
# fork = "alt+f"

EOF
```

### Available options

#### Display options

- `no_tools` (boolean): When `true`, hides tool calls; when `false`, shows them
  in full. When unset (default), tools display in truncated mode
- `last` (boolean): Show last messages instead of first in TUI preview (default:
  true)
- `show_thinking` (boolean): Show thinking blocks and subagent internals in
  conversation output (default: false)
- `plain` (boolean): Output plain text without ledger formatting (default:
  false)
- `pager` (boolean): Pipe output through a pager for scrolling (default: true
  when stdout is a terminal)

#### Resume options

- `default_args` (array of strings): Arguments to pass to the `claude` command
  when resuming conversations. Useful for flags like
  `--dangerously-skip-permissions` that you want applied every time you resume.
  Example: `default_args = ["--dangerously-skip-permissions", "--verbose"]`

#### Key bindings

Customize the keybindings for resume, fork, and delete actions. Values are key
combinations like `"ctrl+r"` or `"alt+f"`. Only single-character keys with
`ctrl+` or `alt+` modifiers are supported.

- `resume` (string): Resume conversation (default: `"ctrl+r"`)
- `fork` (string): Fork and resume conversation (default: `"ctrl+f"`)
- `delete` (string): Delete conversation (default: `"ctrl+x"`)

### Overriding config

Each display option has opposing flags for explicit override:

- `--no-tools` / `--show-tools`
- `--last` / `--first`
- `--hide-thinking` / `--show-thinking`
- `--plain` (no opposite flag)
- `--no-pager` / `--pager`

For example, if your config has `no_tools = false` (showing tools), you can
temporarily hide them with `--no-tools`.

## Custom Claude config directory

If you use the `CLAUDE_CONFIG_DIR` environment variable to store Claude's
configuration in a non-default location, `claude-history` will respect it
automatically — no extra flags needed.

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

## Related projects

- [workmux](https://github.com/raine/workmux) — Git worktrees + tmux windows for
  parallel AI agent workflows
- [git-surgeon](https://github.com/raine/git-surgeon) — Non-interactive
  hunk-level git staging for AI agents
- [consult-llm](https://github.com/raine/consult-llm) — Consult other AI models
  from your agent workflow
- [tmux-file-picker](https://github.com/raine/tmux-file-picker) — Pop up fzf in
  tmux to quickly insert file paths, perfect for AI coding assistants
- [tmux-agent-usage](https://github.com/raine/tmux-agent-usage) — Display AI agent
  rate limit usage in your tmux status bar
