# mnemonai

Universal AI coding conversation history browser. Search, browse, and resume conversations across multiple AI coding tools from a single TUI.

## Supported Tools

| Tool | History Format | Resume Support |
|------|---------------|----------------|
| **Claude Code** | JSONL files in `~/.claude/projects/` | `claude --resume <session-id>` |
| **Cursor** | SQLite in workspace storage | Bridge extension + `cursor://` URI |

## Install

### From source

```bash
cargo install --path .
```

### From crates.io (coming soon)

```bash
cargo install mnemonai
```

## Usage

```bash
# Launch the TUI
mnemonai

# Filter by provider
mnemonai --provider claude
mnemonai --provider cursor

# Start with a search query
mnemonai --query "authentication"

# Filter by project
mnemonai --project /path/to/project
```

## Keyboard Shortcuts

### List View

| Key | Action |
|-----|--------|
| Type | Fuzzy search conversations |
| `Up/Down` or `j/k` | Navigate list |
| `Enter` | View conversation |
| `r` | Resume conversation in original tool |
| `Tab` | Cycle provider filter (All → Claude → Cursor) |
| `Esc` | Clear search / Quit |
| `q` | Quit |

### Detail View

| Key | Action |
|-----|--------|
| `Up/Down` or `j/k` | Scroll |
| `Page Up/Down` | Scroll fast |
| `g/G` | Jump to top/bottom |
| `r` | Resume conversation |
| `Esc` or `q` | Back to list |

## Configuration

Create `~/.config/mnemonai/config.toml`:

```toml
[display]
show_tools = false      # Show tool-use messages
relative_time = true    # "2 hours ago" vs "2026-02-18 14:30"

[providers.claude]
enabled = true

[providers.cursor]
enabled = true
```

## Architecture

```
src/
├── main.rs              # Entry point, event loop
├── cli.rs               # clap argument parsing
├── config.rs            # TOML configuration
├── error.rs             # Error types
├── model.rs             # Unified conversation/message types
├── providers/
│   ├── mod.rs           # Provider trait
│   ├── claude.rs        # Claude Code JSONL loader
│   └── cursor.rs        # Cursor SQLite loader
├── resume/
│   ├── claude.rs        # claude --resume
│   └── cursor.rs        # cursor:// URI + bridge extension
├── tui/
│   ├── app.rs           # State machine
│   ├── ui.rs            # List + detail rendering
│   ├── search.rs        # Fuzzy search
│   └── viewer.rs        # Conversation renderer
└── render/
    ├── markdown.rs      # Markdown → styled text
    └── syntax.rs        # Syntax highlighting
```

### Provider Trait

Adding a new AI tool is straightforward — implement the `Provider` trait:

```rust
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    fn detect(&self) -> bool;
    fn load_conversations(&self) -> Result<Vec<Conversation>>;
    fn resume(&self, conversation: &Conversation) -> Result<()>;
}
```

## Cursor Bridge Extension

The `extension/` directory contains a minimal VS Code extension that enables resuming Cursor conversations. It registers a URI handler that calls `composer.openComposer(composerId)` when mnemonai opens a `cursor://` URI.

The extension is auto-installed on first resume attempt.

## License

MIT
