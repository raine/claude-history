//! Markdown to ANSI text rendering
//!
//! Converts markdown text to ANSI-styled strings suitable for terminal output.
//! Handles text wrapping internally to preserve ledger alignment.

use colored::{ColoredString, Colorize};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

/// Render markdown text to ANSI-styled string with line wrapping
pub fn render_markdown(input: &str, max_width: usize) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(input, options);
    let mut renderer = MarkdownRenderer::new(max_width);

    for event in parser {
        renderer.handle_event(event);
    }

    renderer.finish()
}

struct MarkdownRenderer {
    output: String,
    max_width: usize,
    style_stack: Vec<TextStyle>,
    list_stack: Vec<ListContext>,
    in_code_block: bool,
    code_block_lang: String,
    code_block_content: String,
    pending_text: String,
    at_line_start: bool,
    in_list_item_start: bool, // Suppress paragraph newline right after list bullet
    table_state: Option<TableState>,
}

struct TableState {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
}

impl TableState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            current_row: Vec::new(),
            current_cell: String::new(),
        }
    }
}

#[derive(Clone)]
struct ListContext {
    index: Option<u64>,
    depth: usize,
}

#[derive(Clone)]
enum TextStyle {
    Bold,
    Italic,
    Strikethrough,
    Quote,
    Link(String),
}

impl MarkdownRenderer {
    fn new(max_width: usize) -> Self {
        Self {
            output: String::new(),
            max_width,
            style_stack: vec![],
            list_stack: vec![],
            in_code_block: false,
            code_block_lang: String::new(),
            code_block_content: String::new(),
            pending_text: String::new(),
            at_line_start: true,
            in_list_item_start: false,
            table_state: None,
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.inline_code(&code),
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.rule(),
            _ => {}
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                self.flush_pending();
                // Don't add newline if we just started a list item (bullet is on same line)
                if !self.in_list_item_start && !self.output.is_empty() {
                    // Add blank line before paragraph for visual separation (like original)
                    if !self.output.ends_with("\n\n") {
                        if !self.output.ends_with('\n') {
                            self.output.push('\n');
                        }
                        self.output.push('\n');
                    }
                }
                self.in_list_item_start = false;
            }
            Tag::Heading { level, .. } => {
                self.flush_pending();
                // Add blank line before heading for visual separation
                if !self.output.is_empty() && !self.output.ends_with("\n\n") {
                    if !self.output.ends_with('\n') {
                        self.output.push('\n');
                    }
                    self.output.push('\n');
                }
                let hashes = heading_level_to_usize(level);
                let prefix = "#".repeat(hashes);
                self.output
                    .push_str(&format!("{} ", prefix).cyan().bold().to_string());
            }
            Tag::CodeBlock(kind) => {
                self.flush_pending();
                self.in_code_block = true;
                self.code_block_content.clear();
                // Add blank line before code block for visual separation
                if !self.output.is_empty() && !self.output.ends_with("\n\n") {
                    if !self.output.ends_with('\n') {
                        self.output.push('\n');
                    }
                    self.output.push('\n');
                }
                let lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code_block_lang = lang.clone();
                if !lang.is_empty() {
                    self.output
                        .push_str(&format!("```{}", lang).dimmed().to_string());
                } else {
                    self.output.push_str(&"```".dimmed().to_string());
                }
                self.output.push('\n');
            }
            Tag::List(start) => {
                self.flush_pending();
                // Add blank line before list for visual separation
                if !self.output.is_empty() && !self.output.ends_with("\n\n") {
                    if !self.output.ends_with('\n') {
                        self.output.push('\n');
                    }
                    self.output.push('\n');
                }
                let depth = self.list_stack.len();
                self.list_stack.push(ListContext {
                    index: start,
                    depth,
                });
            }
            Tag::Item => {
                self.flush_pending();
                if !self.output.is_empty() && !self.output.ends_with('\n') {
                    self.output.push('\n');
                }
                let indent = if let Some(ctx) = self.list_stack.last() {
                    "  ".repeat(ctx.depth)
                } else {
                    String::new()
                };
                if let Some(ctx) = self.list_stack.last_mut() {
                    match &mut ctx.index {
                        None => self.output.push_str(&format!("{}- ", indent)),
                        Some(n) => {
                            self.output
                                .push_str(&format!("{}{}. ", indent, n).dimmed().to_string());
                            *n += 1;
                        }
                    }
                }
                self.at_line_start = false;
                self.in_list_item_start = true; // Next paragraph shouldn't add newline
            }
            Tag::Emphasis => self.style_stack.push(TextStyle::Italic),
            Tag::Strong => self.style_stack.push(TextStyle::Bold),
            Tag::Strikethrough => self.style_stack.push(TextStyle::Strikethrough),
            Tag::BlockQuote(_) => {
                self.flush_pending();
                if !self.output.is_empty() && !self.output.ends_with('\n') {
                    self.output.push('\n');
                }
                self.output.push_str(&"> ".green().to_string());
                self.style_stack.push(TextStyle::Quote);
            }
            Tag::Link { dest_url, .. } => {
                self.style_stack.push(TextStyle::Link(dest_url.to_string()));
            }
            Tag::Table(_alignments) => {
                self.flush_pending();
                if !self.output.is_empty() && !self.output.ends_with("\n\n") {
                    if !self.output.ends_with('\n') {
                        self.output.push('\n');
                    }
                    self.output.push('\n');
                }
                self.table_state = Some(TableState::new());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(ref mut state) = self.table_state {
                    state.current_row = Vec::new();
                }
            }
            Tag::TableCell => {
                if let Some(ref mut state) = self.table_state {
                    state.current_cell = String::new();
                }
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_pending();
                self.output.push('\n');
                self.at_line_start = true;
            }
            TagEnd::Heading(_) => {
                self.flush_pending();
                self.output.push('\n');
                self.at_line_start = true;
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;

                // Output highlighted code
                let code = std::mem::take(&mut self.code_block_content);
                if let Some(highlighted) =
                    crate::syntax::highlight_code_ansi(&code, &self.code_block_lang)
                {
                    self.output.push_str(&highlighted);
                } else {
                    // Fallback: apply uniform style per line for unknown languages
                    for line in code.lines() {
                        self.output.push_str(&line.on_bright_black().to_string());
                        self.output.push('\n');
                    }
                }

                // Ensure proper line ending before closing fence
                if !self.output.ends_with('\n') {
                    self.output.push('\n');
                }
                self.output.push_str(&"```".dimmed().to_string());
                self.output.push('\n');
                self.at_line_start = true;
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.in_list_item_start = false; // Clear flag when list ends
            }
            TagEnd::Item => {
                self.flush_pending();
                self.in_list_item_start = false; // Clear flag when item ends
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.style_stack.pop();
            }
            TagEnd::BlockQuote(_) => {
                self.flush_pending();
                self.style_stack.pop();
            }
            TagEnd::Link => {
                if let Some(TextStyle::Link(url)) = self.style_stack.pop() {
                    self.pending_text
                        .push_str(&format!(" ({})", url).blue().underline().to_string());
                }
            }
            TagEnd::Table => {
                if let Some(state) = self.table_state.take() {
                    let rendered = render_table(&state.rows);
                    self.output.push_str(&rendered);
                }
                self.at_line_start = true;
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(ref mut state) = self.table_state {
                    let row = std::mem::take(&mut state.current_row);
                    state.rows.push(row);
                }
            }
            TagEnd::TableCell => {
                if let Some(ref mut state) = self.table_state {
                    let cell = std::mem::take(&mut state.current_cell);
                    state.current_row.push(cell);
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        // Handle table cell text
        if let Some(ref mut state) = self.table_state {
            // Replace newlines with spaces to prevent breaking table layout
            state.current_cell.push_str(&text.replace('\n', " "));
            return;
        }

        if self.in_code_block {
            // Buffer code block content for syntax highlighting at block end
            self.code_block_content.push_str(text);
        } else {
            // Apply styles immediately (before they get popped from stack)
            let styled = apply_styles(text, &self.style_stack);
            self.pending_text.push_str(&styled);
        }
    }

    fn inline_code(&mut self, code: &str) {
        // Handle table cell inline code
        if let Some(ref mut state) = self.table_state {
            state.current_cell.push_str(code);
            return;
        }

        // Inline code with subtle blueish color (no backticks - color distinguishes it)
        let styled = code.truecolor(147, 161, 199).to_string();
        self.pending_text.push_str(&styled);
    }

    fn soft_break(&mut self) {
        // Preserve line breaks instead of converting to space (standard markdown behavior)
        // For conversation display, users expect their line breaks to be kept
        self.flush_pending();
        self.output.push('\n');
    }

    fn hard_break(&mut self) {
        self.flush_pending();
        self.output.push('\n');
        self.at_line_start = true;
    }

    fn rule(&mut self) {
        self.flush_pending();
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
        self.output
            .push_str(&"─".repeat(self.max_width.min(40)).dimmed().to_string());
        self.output.push('\n');
        self.at_line_start = true;
    }

    fn flush_pending(&mut self) {
        if self.pending_text.is_empty() {
            return;
        }

        let text = std::mem::take(&mut self.pending_text);

        // Wrap the plain text (stripping ANSI that may have been added by inline code)
        // Then apply styles to each line
        let wrapped = wrap_text_preserve_ansi(&text, self.max_width);

        for (i, line) in wrapped.iter().enumerate() {
            if i > 0 {
                self.output.push('\n');
                // Add list indent for continuation lines
                if let Some(ctx) = self.list_stack.last() {
                    let indent = "  ".repeat(ctx.depth);
                    self.output.push_str(&format!("{}  ", indent));
                }
            }
            // Styles are already applied in text(), just output the line
            self.output.push_str(line);
        }

        self.at_line_start = false;
    }

    fn finish(mut self) -> String {
        self.flush_pending();
        // Trim trailing whitespace but preserve structure
        while self.output.ends_with("\n\n") {
            self.output.pop();
        }
        self.output
    }
}

fn heading_level_to_usize(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn apply_styles(text: &str, styles: &[TextStyle]) -> String {
    if styles.is_empty() {
        return text.to_string();
    }

    let mut result: ColoredString = text.normal();

    for style in styles {
        result = match style {
            TextStyle::Bold => result.bold(),
            TextStyle::Italic => result.italic(),
            TextStyle::Strikethrough => result.strikethrough(),
            TextStyle::Quote => result.green(),
            TextStyle::Link(_) => result.blue().underline(),
        };
    }

    result.to_string()
}

/// Wrap text while preserving ANSI escape codes
/// This is a simplified approach: we strip ANSI for width calculation
fn wrap_text_preserve_ansi(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 || text.is_empty() {
        return vec![text.to_string()];
    }

    // Simple wrapping that respects word boundaries
    // ANSI codes are preserved but may cause slight width miscalculation
    textwrap::wrap(text, max_width)
        .into_iter()
        .map(|cow| cow.into_owned())
        .collect()
}

/// Render a table with box-drawing characters
fn render_table(rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    // Calculate column widths based on display width (handles Unicode)
    let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_widths = vec![0usize; num_cols];

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < num_cols {
                col_widths[i] = col_widths[i].max(cell.trim().width());
            }
        }
    }

    // Box-drawing characters
    let h = '─'; // horizontal
    let v = '│'; // vertical
    let tl = '┌'; // top-left
    let tr = '┐'; // top-right
    let bl = '└'; // bottom-left
    let br = '┘'; // bottom-right
    let lj = '├'; // left junction
    let rj = '┤'; // right junction
    let tj = '┬'; // top junction
    let bj = '┴'; // bottom junction
    let cj = '┼'; // center junction

    let mut output = String::new();

    // Helper to build horizontal line
    let build_line = |left: char, mid: char, right: char| -> String {
        let mut line = String::new();
        line.push(left);
        for (i, &width) in col_widths.iter().enumerate() {
            line.extend(std::iter::repeat_n(h, width + 2)); // +2 for padding
            if i < col_widths.len() - 1 {
                line.push(mid);
            }
        }
        line.push(right);
        line.push('\n');
        line
    };

    // Top border (dimmed like code fences)
    output.push_str(&build_line(tl, tj, tr).dimmed().to_string());

    // Rows with separators
    for (row_idx, row) in rows.iter().enumerate() {
        // Row content
        output.push_str(&v.to_string().dimmed().to_string());
        for (i, width) in col_widths.iter().enumerate() {
            let cell = row.get(i).map(|s| s.trim()).unwrap_or("");
            let cell_width = cell.width();
            let padding = width.saturating_sub(cell_width);
            output.push(' ');
            output.push_str(cell);
            output.push_str(&" ".repeat(padding + 1));
            output.push_str(&v.to_string().dimmed().to_string());
        }
        output.push('\n');

        // Separator (between all rows)
        if row_idx < rows.len() - 1 {
            output.push_str(&build_line(lj, cj, rj).dimmed().to_string());
        }
    }

    // Bottom border (dimmed like code fences)
    output.push_str(&build_line(bl, bj, br).dimmed().to_string());

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_text() {
        let result = render_markdown("Hello world", 80);
        assert_eq!(result.trim(), "Hello world");
    }

    #[test]
    fn test_inline_code() {
        let result = render_markdown("Use `foo()` here", 80);
        assert!(result.contains("foo()"));
    }

    #[test]
    fn test_bold() {
        // Force colors for test
        colored::control::set_override(true);
        let result = render_markdown("This is **bold** text", 80);
        assert!(result.contains("bold"));
        // Check for ANSI bold code (ESC[1m)
        assert!(
            result.contains("\x1b[1m"),
            "Expected bold ANSI codes in: {:?}",
            result
        );
        colored::control::unset_override();
    }

    #[test]
    fn test_code_block() {
        let result = render_markdown("```rust\nlet x = 1;\n```", 80);
        // With syntax highlighting, tokens are split with ANSI codes
        // Check individual tokens are present
        assert!(result.contains("let"));
        assert!(result.contains("x"));
        assert!(result.contains("1"));
        assert!(result.contains("```"));
        // Verify syntax highlighting is applied (ANSI color codes present)
        assert!(
            result.contains("\x1b[38;2;"),
            "Expected syntax highlighting ANSI codes in: {:?}",
            result
        );
    }

    #[test]
    fn test_list() {
        let result = render_markdown("- item 1\n- item 2", 80);
        assert!(result.contains("- item 1"));
        assert!(result.contains("- item 2"));
    }

    #[test]
    fn test_heading() {
        let result = render_markdown("# Heading", 80);
        assert!(result.contains("#"));
        assert!(result.contains("Heading"));
    }

    #[test]
    fn test_linebreaks_preserved() {
        let input = "Line one here\nLine two here\nLine three";
        let result = render_markdown(input, 80);
        // Should have newlines between lines
        let lines: Vec<&str> = result.lines().collect();
        eprintln!("DEBUG lines: {:?}", lines);
        assert!(
            lines.len() >= 3,
            "Expected at least 3 lines, got {}: {:?}",
            lines.len(),
            lines
        );
    }

    #[test]
    fn test_paragraph_then_list() {
        let input = "Some text here:\n- Item one\n- Item two";
        let result = render_markdown(input, 80);
        eprintln!("DEBUG output:\n{}", result);
        eprintln!("DEBUG escaped: {:?}", result);
        // Should have newline between text and list
        assert!(result.contains("here:\n"), "Expected newline after colon");
    }

    #[test]
    fn test_list_then_paragraph() {
        let input = "- Item with text\n- Another item\n\nParagraph after list.";
        let result = render_markdown(input, 80);
        eprintln!("DEBUG output:\n{}", result);
        eprintln!("DEBUG escaped: {:?}", result);
        // Should have newline between list and paragraph
        assert!(
            result.contains("item\n"),
            "Expected newline after list item"
        );
        assert!(
            result.contains("\nParagraph"),
            "Expected paragraph on new line"
        );
    }

    #[test]
    fn test_complex_structure() {
        let input = r#"Arguments: `--no-review` task description
- Detects OS
- Downloads binary

Next paragraph here."#;
        let result = render_markdown(input, 80);
        eprintln!("DEBUG output:\n{}", result);
        eprintln!("DEBUG escaped: {:?}", result);
    }

    #[test]
    fn test_blank_line_before_list() {
        let input = "Some intro text:\n1. First item\n2. Second item";
        let result = render_markdown(input, 80);
        eprintln!("DEBUG output:\n{}", result);
        eprintln!("DEBUG escaped: {:?}", result);
        // Should have blank line between text and list
        assert!(
            result.contains("text:\n\n"),
            "Expected blank line before list, got: {:?}",
            result
        );
    }

    #[test]
    fn test_table_basic() {
        let input = r#"| A | B |
|---|---|
| 1 | 2 |"#;
        let result = render_markdown(input, 80);
        eprintln!("Table output:\n{}", result);
        assert!(result.contains("┌"), "Expected top-left corner");
        assert!(result.contains("│"), "Expected vertical border");
        assert!(result.contains("└"), "Expected bottom-left corner");
        assert!(result.contains(" A "), "Expected cell A");
        assert!(result.contains(" B "), "Expected cell B");
        assert!(result.contains(" 1 "), "Expected cell 1");
        assert!(result.contains(" 2 "), "Expected cell 2");
    }

    #[test]
    fn test_table_column_widths() {
        let input = r#"| Column A | Column B |
|----------|----------|
| Short    | Longer text |"#;
        let result = render_markdown(input, 80);
        eprintln!("Table output:\n{}", result);
        // Columns should be sized to fit longest content
        assert!(result.contains("Column A"), "Expected Column A");
        assert!(result.contains("Longer text"), "Expected Longer text");
    }

    #[test]
    fn test_table_multiple_rows() {
        let input = r#"| H1 | H2 | H3 |
|----|----|----|
| A  | B  | C  |
| D  | E  | F  |
| G  | H  | I  |"#;
        let result = render_markdown(input, 80);
        eprintln!("Table output:\n{}", result);
        // Should have separators between rows
        assert!(result.contains("├"), "Expected row separators");
        assert!(result.contains("┼"), "Expected cross junctions");
    }
}
