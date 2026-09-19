//! Markdown layout.
//!
//! `layout::LayoutEngine` turns markdown into wrapped, attributed runs; the
//! TUI viewer maps those runs to spans and every other renderer goes through
//! the viewer.

pub mod layout;

use unicode_width::UnicodeWidthStr;

/// Hard-wrap code block lines that exceed max_width at character boundaries.
/// Operates on plain text (before syntax highlighting) so no ANSI handling needed.
pub fn wrap_code_lines(code: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthChar;

    if max_width == 0 {
        return code.to_string();
    }

    let mut result = String::new();
    for line in code.lines() {
        let line_width = line.width();
        if line_width <= max_width {
            result.push_str(line);
            result.push('\n');
        } else {
            let mut current_width = 0;
            for ch in line.chars() {
                let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width + ch_width > max_width && current_width > 0 {
                    result.push('\n');
                    current_width = 0;
                }
                result.push(ch);
                current_width += ch_width;
            }
            result.push('\n');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plain text of a layout: inline code keeps its backticks, nothing else.
    fn render_markdown_plain(input: &str, max_width: usize) -> String {
        let doc = layout::LayoutEngine::render(input, max_width);
        let mut output = String::new();
        for (i, line) in doc.lines.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }
            for run in &line.runs {
                if run.attrs.code {
                    output.push('`');
                    output.push_str(&run.text);
                    output.push('`');
                } else {
                    output.push_str(&run.text);
                }
            }
        }
        output
    }

    #[test]
    fn test_plain_blockquote_wraps_with_prefix() {
        let input = "> This is a rather long quote that will definitely need to wrap to multiple lines and we want the > prefix on each continuation line.";
        let out = render_markdown_plain(input, 40);
        for l in out.lines() {
            assert!(l.starts_with(">"), "Line lost quote prefix: {:?}", l);
        }
    }

    #[test]
    fn test_plain_preserves_blank_lines_in_code_block() {
        let input = "```rust\nfn a() {}\n\nfn b() {}\n```\n";
        let out = render_markdown_plain(input, 80);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "Expected 5 lines, got: {:?}", lines);
        assert_eq!(lines[2], "", "Expected blank line in middle: {:?}", lines);
    }

    #[test]
    fn test_plain_text() {
        let result = render_markdown_plain("Hello world", 80);
        assert_eq!(result.trim(), "Hello world");
    }

    #[test]
    fn test_inline_code() {
        let result = render_markdown_plain("Use `foo()` here", 80);
        assert!(result.contains("foo()"));
    }

    #[test]
    fn test_bold() {
        let doc = layout::LayoutEngine::render("This is **bold** text", 80);
        let bold_runs: Vec<&str> = doc
            .lines
            .iter()
            .flat_map(|line| line.runs.iter())
            .filter(|run| run.attrs.bold)
            .map(|run| run.text.as_str())
            .collect();
        assert_eq!(bold_runs, vec!["bold"]);
    }

    #[test]
    fn test_code_block() {
        let doc = layout::LayoutEngine::render("```rust\nlet x = 1;\n```", 80);
        let text = render_markdown_plain("```rust\nlet x = 1;\n```", 80);
        assert!(text.contains("let x = 1;"));
        assert!(text.contains("```"));
        assert!(
            doc.lines
                .iter()
                .flat_map(|line| line.runs.iter())
                .any(|run| run.attrs.code_block_lang.is_some() && run.text.contains("let")),
            "code block runs should carry their language: {doc:?}"
        );
    }

    #[test]
    fn test_list() {
        let result = render_markdown_plain("- item 1\n- item 2", 80);
        assert!(result.contains("- item 1"));
        assert!(result.contains("- item 2"));
    }

    #[test]
    fn test_heading() {
        let result = render_markdown_plain("# Heading", 80);
        assert!(result.contains("#"));
        assert!(result.contains("Heading"));
    }

    #[test]
    fn test_linebreaks_preserved() {
        let input = "Line one here\nLine two here\nLine three";
        let result = render_markdown_plain(input, 80);
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
        let result = render_markdown_plain(input, 80);
        eprintln!("DEBUG output:\n{}", result);
        eprintln!("DEBUG escaped: {:?}", result);
        // Should have newline between text and list
        assert!(result.contains("here:\n"), "Expected newline after colon");
    }

    #[test]
    fn test_list_then_paragraph() {
        let input = "- Item with text\n- Another item\n\nParagraph after list.";
        let result = render_markdown_plain(input, 80);
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
        let result = render_markdown_plain(input, 80);
        eprintln!("DEBUG output:\n{}", result);
        eprintln!("DEBUG escaped: {:?}", result);
    }

    #[test]
    fn test_blank_line_before_list() {
        let input = "Some intro text:\n1. First item\n2. Second item";
        let result = render_markdown_plain(input, 80);
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
    fn test_code_block_wrapping() {
        // Use plain mode to test wrapping without ANSI codes affecting width
        let long_line = "x".repeat(100);
        let input = format!("```\n{}\n```", long_line);
        let result = render_markdown_plain(&input, 40);
        // Every output line should fit within max_width
        for line in result.lines() {
            let width = UnicodeWidthStr::width(line);
            assert!(
                width <= 40,
                "Line exceeds max_width ({}): {:?}",
                width,
                line
            );
        }
        // Content should still be present (just wrapped)
        let total_x: usize = result.lines().map(|l| l.matches('x').count()).sum();
        assert_eq!(total_x, 100, "All characters should be preserved");
    }

    #[test]
    fn test_table_basic() {
        let input = r#"| A | B |
|---|---|
| 1 | 2 |"#;
        let result = render_markdown_plain(input, 80);
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
        let result = render_markdown_plain(input, 80);
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
        let result = render_markdown_plain(input, 80);
        eprintln!("Table output:\n{}", result);
        // Should have separators between rows
        assert!(result.contains("├"), "Expected row separators");
        assert!(result.contains("┼"), "Expected cross junctions");
    }

    // Tests for render_markdown_plain

    #[test]
    fn test_plain_no_ansi_codes() {
        let input = "This is **bold** and *italic* and `code`";
        let result = render_markdown_plain(input, 80);
        assert!(
            !result.contains("\x1b"),
            "Plain output should not contain ANSI escape codes: {:?}",
            result
        );
    }

    #[test]
    fn test_plain_inline_code_has_backticks() {
        let result = render_markdown_plain("Use `foo()` here", 80);
        assert!(
            result.contains("`foo()`"),
            "Plain inline code should have backticks: {:?}",
            result
        );
    }

    #[test]
    fn test_plain_code_block() {
        let result = render_markdown_plain("```rust\nlet x = 1;\n```", 80);
        assert!(result.contains("```rust"), "Should have opening fence");
        assert!(result.contains("let x = 1;"), "Should have code content");
        // Count closing fences (should have opening and closing)
        assert_eq!(
            result.matches("```").count(),
            2,
            "Should have exactly 2 fences (open + close)"
        );
    }

    #[test]
    fn test_plain_heading() {
        let result = render_markdown_plain("## Heading", 80);
        assert!(
            result.contains("## Heading"),
            "Should have heading with hash prefix: {:?}",
            result
        );
    }

    #[test]
    fn test_plain_list() {
        let result = render_markdown_plain("- item 1\n- item 2", 80);
        assert!(result.contains("- item 1"), "Should have list items");
        assert!(result.contains("- item 2"), "Should have list items");
    }

    #[test]
    fn test_plain_link() {
        let result = render_markdown_plain("[click here](https://example.com)", 80);
        assert!(
            result.contains("click here"),
            "Should have link text: {:?}",
            result
        );
        assert!(
            result.contains("(https://example.com)"),
            "Should have link URL: {:?}",
            result
        );
    }

    #[test]
    fn test_plain_wrapping() {
        let long_text = "word ".repeat(20); // 100 chars
        let result = render_markdown_plain(&long_text, 40);
        for line in result.lines() {
            let width = UnicodeWidthStr::width(line);
            assert!(
                width <= 40,
                "Line exceeds max_width ({}): {:?}",
                width,
                line
            );
        }
    }

    #[test]
    fn test_plain_table() {
        let input = r#"| A | B |
|---|---|
| 1 | 2 |"#;
        let result = render_markdown_plain(input, 80);
        assert!(
            !result.contains("\x1b"),
            "Plain table should not contain ANSI: {:?}",
            result
        );
        assert!(result.contains("┌"), "Should have box-drawing chars");
        assert!(result.contains(" A "), "Should have cell content");
    }

    #[test]
    fn test_plain_block_quote() {
        let result = render_markdown_plain("> quoted text", 80);
        assert!(
            result.contains("> "),
            "Should have block quote prefix: {:?}",
            result
        );
        assert!(
            !result.contains("\x1b"),
            "Plain block quote should not contain ANSI: {:?}",
            result
        );
    }

    #[test]
    fn test_plain_horizontal_rule() {
        let result = render_markdown_plain("---", 80);
        assert!(
            result.contains("─"),
            "Should have horizontal rule: {:?}",
            result
        );
        assert!(
            !result.contains("\x1b"),
            "Plain rule should not contain ANSI: {:?}",
            result
        );
    }

    #[test]
    fn test_plain_list_continuation_indent() {
        // Long list item text should wrap with proper continuation indent
        let input = "4. This is a long list item that should wrap and the continuation line should be indented to match the bullet prefix width";
        let result = render_markdown_plain(input, 50);
        eprintln!("List continuation:\n{}", result);
        let lines: Vec<&str> = result.lines().collect();
        assert!(lines.len() > 1, "Should wrap to multiple lines");
        // First line starts with "4. "
        assert!(
            lines[0].starts_with("4. "),
            "First line should start with bullet: {:?}",
            lines[0]
        );
        // Continuation lines should be indented by 3 spaces (matching "4. " width)
        for line in &lines[1..] {
            assert!(
                line.starts_with("   "),
                "Continuation should be indented 3 spaces: {:?}",
                line
            );
        }
    }

    #[test]
    fn test_plain_unordered_list_continuation_indent() {
        let input = "- This is a long unordered list item that should wrap and the continuation line should be indented to match";
        let result = render_markdown_plain(input, 40);
        eprintln!("Unordered list continuation:\n{}", result);
        let lines: Vec<&str> = result.lines().collect();
        assert!(lines.len() > 1, "Should wrap to multiple lines");
        // Continuation lines should be indented by 2 spaces (matching "- " width)
        for line in &lines[1..] {
            assert!(
                line.starts_with("  "),
                "Continuation should be indented 2 spaces: {:?}",
                line
            );
        }
    }
}
